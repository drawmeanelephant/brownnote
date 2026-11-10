// Mounted by the integrator: `ui.rs` declares `mod midi;`, and the `State`
// field, the `poll` call and the view slot are listed as wiring requests in the
// issue #105 PR. Until that wiring lands, nothing in the app calls this module.
#![allow(dead_code)]

//! MIDI input: device selection (a machine preference, D8), learn mode and
//! status. The selected input itself lives in `brownnote_native::midi::input`;
//! this module is the focused UI around it, following the `ui/pedals` pattern:
//! `poll`, view fragments and test scenes.
//!
//! `Midi` is a cloneable handle (`Rc<RefCell<..>>`) so view handlers can mutate
//! it through the captured handle before the integrator adds a `State` field;
//! after wiring, handlers may equally use `s.midi` directly.
mod preferences {
    //! Machine-local MIDI input preference (D8): the selected port ID plus the
    //! name as a reconnect hint only. The caller supplies the machine root;
    //! this module never discovers a home directory or touches project documents.
    use serde::{Deserialize, Serialize};
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        path::Path,
    };

    const FILE: &str = "midi-input.json";
    const MAX_BYTES: u64 = 16 * 1024;

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Preference {
        version: u32,
        port_id: Option<String>,
        port_name: Option<String>,
    }

    pub fn load(root: &Path) -> Result<(Option<String>, Option<String>), String> {
        let file = match File::open(root.join(FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((None, None));
            }
            Err(_) => return Err("Cannot read machine MIDI input preferences".into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read machine MIDI input preferences")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Machine MIDI input preferences are too large".into());
        }
        let saved: Preference = serde_json::from_slice(&bytes)
            .map_err(|_| "Corrupt machine MIDI input preferences; choose an input explicitly")?;
        if saved.version != 1 {
            return Err("Unsupported machine MIDI input preference version".into());
        }
        for value in [&saved.port_id, &saved.port_name] {
            if let Some(value) = value
                && value.is_empty()
            {
                return Err("Invalid saved MIDI input preference".into());
            }
        }
        Ok((saved.port_id, saved.port_name))
    }

    pub fn save(root: &Path, id: Option<&str>, name: Option<&str>) -> Result<(), String> {
        let saved = Preference {
            version: 1,
            port_id: id.map(str::to_string),
            port_name: name.map(str::to_string),
        };
        let bytes = serde_json::to_vec_pretty(&saved)
            .map_err(|_| "Cannot encode machine MIDI input preferences")?;
        // One app-side writer. A failed write leaves the last complete file intact.
        let write = || -> std::io::Result<()> {
            fs::create_dir_all(root)?;
            let temporary = root.join("midi-input.json.tmp");
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(temporary, root.join(FILE))?;
            File::open(root)?.sync_all()
        };
        write().map_err(|_| "MIDI input choice is active in memory but could not be saved".into())
    }
}

use super::{State, button, caption, ink, instrument, media::pad_id, transport};
use brownnote_native::{
    engine::Command,
    instrument::INSTRUMENT,
    midi::{
        input::{self, Action, Input, PortInfo, Report, Sink},
        mapping::{ControlAction, ControlIntent, Mapping, MappingKey, Mappings, NoteTarget},
    },
    performance::{OriginalTime, Pitch, SourceId, TakeId},
    project::PadId,
};
use rune::{El, Key, KeyEvent, Tx, col, row};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// App-clock seconds to the take-relative time the controller bases a new
/// connection's epoch on. The recorder (#106) re-anchors event times to the
/// take; until then this is the app clock.
fn now_nanos(s: &State) -> OriginalTime {
    let nanos = (s.now.max(0.0) * 1_000_000_000.0).min(u64::MAX as f64) as u64;
    OriginalTime::from_nanos(nanos)
}

/// The dispatch boundary from the MIDI controller to the app: one-shot pads go
/// through the real 256-slot command queue, and every action is offered to the
/// recording transport. Held-instrument starts/ends, sustain and panic are
/// also forwarded to a mounted live CLAP instrument (no second scheduler: the
/// backend names the output frame).
struct UiSink<'a> {
    state: &'a mut State,
    source: SourceId,
    /// Disconnect/refresh reconciliation ends notes now, on the app clock.
    reconcile: bool,
}

impl<'a> UiSink<'a> {
    fn new(state: &'a mut State, source: SourceId, reconcile: bool) -> Self {
        Self {
            state,
            source,
            reconcile,
        }
    }

    fn at(&mut self, time: OriginalTime) -> u64 {
        if self.reconcile {
            transport::app_nanos(self.state)
        } else {
            transport::midi_time(self.state, self.source, time)
        }
    }
}

impl Sink for UiSink<'_> {
    fn dispatch(&mut self, action: Action) -> bool {
        match action {
            Action::Trigger {
                pad,
                velocity,
                time,
                ..
            } => {
                let index = pad.index();
                let Some((slot, group)) = self.state.assets.playback(pad) else {
                    // No audible sample on this pad: nothing to queue, and
                    // not a queue loss.
                    return true;
                };
                let at = self.at(time);
                transport::capture_pad(self.state, pad, velocity, transport::MIDI_PAD_SOURCE, at);
                let settings = self.state.settings[index];
                let sent = self.state.output.send(Command::Trigger {
                    pad: index,
                    slot,
                    group,
                    velocity: f32::from(velocity.value()) / 127.0,
                    gain: settings.gain,
                    pan: settings.pan,
                    semitones: settings.pitch,
                });
                if sent {
                    self.state.selected = index;
                    self.state.flashes[index] = self.state.now + 0.120;
                }
                sent
            }
            Action::HeldStart(on) => {
                let at = self.at(on.original_time);
                instrument::held_start(self.state, &on);
                super::live::held_start(self.state, &on);
                transport::held_start(self.state, &on, at);
                true
            }
            Action::HeldEnd(note) => {
                let at = self.at(note.original_end());
                instrument::held_end(self.state, &note);
                super::live::held_end(self.state, &note);
                transport::held_end(self.state, &note, at);
                true
            }
            Action::Control {
                action: ControlIntent::Panic,
                ..
            } => {
                super::live::panic(self.state);
                transport::panic(self.state)
            }
            Action::Control { scope, action } => {
                super::live::control(self.state, scope.channel, action);
                // Sustain and all-notes-off are applied by the controller's
                // own lifecycle; their releases arrive here as HeldEnd.
                true
            }
        }
    }
}

/// What a learned CC candidate is assigned to. Note candidates are assigned to
/// a pad via [`assign_to_pad`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assign {
    Sustain,
    AllNotesOff,
    Panic,
}

pub struct MidiInner {
    pub visible: bool,
    pub input: Option<Input>,
    pub ports: Vec<PortInfo>,
    /// Mirror of the connected port, for status and scenes; `poll` syncs it
    /// from `Input::port` whenever a real connection exists.
    pub connected: Option<PortInfo>,
    pub root: Option<PathBuf>,
    pub preference_error: Option<String>,
    pub notice: Option<String>,
    pub error: Option<String>,
    refresh_requested: bool,
    connect_requested: bool,
    disconnect_requested: bool,
    last_report: Option<Report>,
}

/// Cloneable handle to the MIDI input UI state.
#[derive(Clone)]
pub struct Midi {
    inner: Rc<RefCell<MidiInner>>,
}

impl Midi {
    /// Safe for headless/test constructors: no backend, no files, no ports.
    pub fn disabled() -> Self {
        Self {
            inner: Rc::new(RefCell::new(MidiInner {
                visible: false,
                input: Some(Input::headless(TakeId(0))),
                ports: Vec::new(),
                connected: None,
                root: None,
                preference_error: None,
                notice: None,
                error: None,
                refresh_requested: false,
                connect_requested: false,
                disconnect_requested: false,
                last_report: None,
            })),
        }
    }

    /// Only interactive startup calls this, with an explicit machine root.
    /// Bad preferences stay silent until the user chooses an input explicitly.
    pub fn load(root: Result<PathBuf, String>, backend: bool, take: TakeId) -> Self {
        let midi = Self::disabled();
        {
            let mut inner = midi.inner.borrow_mut();
            inner.input = if backend {
                match Input::new(take) {
                    Ok(input) => Some(input),
                    Err(error) => {
                        inner.error = Some(error);
                        None
                    }
                }
            } else {
                Some(Input::headless(take))
            };
            inner.refresh_requested = true;
        }
        match root {
            Ok(root) => {
                let mut inner = midi.inner.borrow_mut();
                inner.root = Some(root.clone());
                match preferences::load(&root) {
                    Ok((id, name)) => {
                        if let Some(input) = &mut inner.input {
                            input.select_hint(id, name);
                        }
                    }
                    Err(error) => inner.preference_error = Some(error),
                }
            }
            Err(error) => midi.inner.borrow_mut().preference_error = Some(error),
        }
        midi
    }

    fn inner(&self) -> std::cell::Ref<'_, MidiInner> {
        self.inner.borrow()
    }

    fn inner_mut(&self) -> std::cell::RefMut<'_, MidiInner> {
        self.inner.borrow_mut()
    }
}

fn describe_key(key: MappingKey) -> String {
    match key {
        MappingKey::Note { channel, pitch } => {
            format!("ch {} · note {}", channel.value() + 1, pitch.value())
        }
        MappingKey::Controller { channel, number } => {
            format!("ch {} · cc {}", channel.value() + 1, number.value())
        }
    }
}

fn describe_mapping(mapping: &Mapping) -> String {
    match mapping {
        Mapping::Note {
            channel,
            pitch,
            target,
        } => {
            let target = match target {
                NoteTarget::OneShot(pad) => format!("PAD {:02}", pad.index() + 1),
                NoteTarget::Held(instrument) => format!("HELD instrument {}", instrument.0),
            };
            format!(
                "ch {} · note {} → {}",
                channel.value() + 1,
                pitch.value(),
                target
            )
        }
        Mapping::Controller {
            channel,
            number,
            action,
        } => {
            let action = match action {
                ControlAction::Sustain => "SUSTAIN",
                ControlAction::AllNotesOff => "ALL NOTES OFF",
                ControlAction::Panic => "PANIC",
            };
            format!(
                "ch {} · cc {} → {}",
                channel.value() + 1,
                number.value(),
                action
            )
        }
    }
}

/// One-line status: connection plus explicit loss counters.
pub fn status(_s: &State, midi: &Midi) -> String {
    let inner = midi.inner();
    let Some(input) = &inner.input else {
        return "MIDI BACKEND UNAVAILABLE".into();
    };
    let mut text = if let Some(port) = &inner.connected {
        format!(
            "CONNECTED · {} · source {}",
            port.name,
            input.controller().source().0
        )
    } else {
        input.status()
    };
    let counters = input.controller().counters();
    let loss = input.controller().callback_loss();
    text.push_str(&format!(
        "  ·  received {} · malformed {} · unsupported {} · unmapped {} · triggered {} · held {} active",
        counters.received,
        counters.malformed,
        counters.unsupported,
        counters.unmapped,
        counters.triggered,
        input.controller().active_held()
    ));
    let total_loss = loss.lost_raw
        + loss.oversized
        + counters.lost_commands
        + counters.stale_events
        + counters.unmatched_releases
        + counters.clock_anomalies;
    if total_loss > 0 {
        text.push_str(&format!(
            "  ·  LOSS raw {} · oversized {} · commands {} · stale {} · unmatched {} · clock {}",
            loss.lost_raw,
            loss.oversized,
            counters.lost_commands,
            counters.stale_events,
            counters.unmatched_releases,
            counters.clock_anomalies
        ));
    }
    text
}

/// Persist the machine preference (D8): port ID plus name as a hint only.
fn remember(inner: &mut MidiInner, port: &PortInfo) {
    if let Some(root) = inner.root.clone() {
        inner.preference_error = preferences::save(&root, Some(&port.id), Some(&port.name)).err();
    }
}

/// Drain input, run requested refresh/connect/disconnect. Returns whether
/// anything the view shows changed.
pub fn poll(s: &mut State, midi: &Midi) -> bool {
    let mut changed = false;
    let now = now_nanos(s);
    // Take the input out of the shared cell for the duration of the work, so
    // the rest of the inner state can be updated while the input is borrowed.
    let mut owned = midi.inner_mut().input.take();
    if let Some(input) = owned.as_mut() {
        // Read the request flags out first: a Ref from a condition temporary
        // would stay alive inside the block and panic on the next borrow_mut.
        let refresh_requested = midi.inner().refresh_requested;
        if refresh_requested {
            midi.inner_mut().refresh_requested = false;
            let source = input.controller().source();
            match input.refresh(&mut UiSink::new(s, source, true), now) {
                Ok(refresh) => {
                    let mut inner = midi.inner_mut();
                    inner.ports = refresh.ports;
                    if refresh.vanished {
                        let released = input.controller().counters().released_on_disconnect;
                        inner.notice = Some(format!(
                            "Input disconnected: {released} held note(s) released, loss reported; mappings unchanged"
                        ));
                    }
                    if let Some(port) = &refresh.reconnected {
                        remember(&mut inner, port);
                        inner.notice =
                            Some(format!("Reconnected to {} by saved selection", port.name));
                    }
                    inner.error = input.error().map(str::to_string);
                    changed = true;
                }
                Err(error) => {
                    midi.inner_mut().error = Some(error);
                    changed = true;
                }
            }
        }
        let connect_requested = midi.inner().connect_requested;
        if connect_requested {
            midi.inner_mut().connect_requested = false;
            match input.connect(now) {
                Ok(port) => {
                    let mut inner = midi.inner_mut();
                    remember(&mut inner, &port);
                    inner.connected = Some(port);
                    inner.notice = None;
                    inner.error = None;
                    changed = true;
                }
                Err(error) => {
                    midi.inner_mut().error = Some(error);
                    changed = true;
                }
            }
        }
        let disconnect_requested = midi.inner().disconnect_requested;
        if disconnect_requested {
            midi.inner_mut().disconnect_requested = false;
            let source = input.controller().source();
            let mut sink = UiSink::new(s, source, true);
            let released = input.disconnect(&mut sink, now)
                // A fixture input has a controller connection but no backend
                // port; reconcile it the same way (a no-op after a real close).
                + input.controller_mut().end_connection(&mut sink, now);
            let mut inner = midi.inner_mut();
            inner.connected = None;
            inner.notice = Some(format!(
                "Disconnected: {released} held note(s) released; mappings unchanged"
            ));
            changed = true;
        }
        if let Some(port) = input.port() {
            midi.inner_mut().connected = Some(port.clone());
        }
        let source = input.controller().source();
        let report = input.poll(&mut UiSink::new(s, source, false));
        let mut inner = midi.inner_mut();
        if inner.last_report != Some(report) {
            changed = true;
        }
        inner.last_report = Some(report);
    }
    midi.inner_mut().input = owned;
    changed
}

/// The controller's project mappings (D8), for saving with the project.
pub fn mappings(midi: &Midi) -> Option<Mappings> {
    midi.inner()
        .input
        .as_ref()
        .map(|input| input.controller().mappings().clone())
}

/// Replace the project mappings after a project opens. Device choice is
/// machine-local and stays as it was.
pub fn set_mappings(s: &mut State, mappings: Mappings) {
    let midi = s.midi.clone();
    let mut owned = midi.inner_mut().input.take();
    if let Some(input) = owned.as_mut() {
        let now = now_nanos(s);
        let source = input.controller().source();
        input.controller_mut().replace_project_mappings(
            mappings,
            &mut UiSink::new(s, source, true),
            now,
        );
    }
    midi.inner_mut().input = owned;
}

/// Deterministic virtual input for scenes and tests: a connected fixture
/// source with `mappings` (a project mappings document) and pre-queued raw
/// packets `(backend microseconds, bytes)`, drained by the next real `poll`.
pub fn fixture(midi: &Midi, source: SourceId, mappings: &str, packets: &[(u64, &[u8])]) {
    let mut inner = midi.inner_mut();
    let Some(input) = inner.input.as_mut() else {
        return;
    };
    let (mut producer, consumer) = rtrb::RingBuffer::new(input::RAW_QUEUE_CAPACITY);
    let controller = input.controller_mut();
    controller.attach(consumer);
    controller.begin_connection(source, OriginalTime::from_nanos(0));
    if let Ok(document) = serde_json::from_str::<input::MappingsDocument>(mappings)
        && let Ok(mappings) = document.to_mappings()
    {
        controller.set_mappings(mappings);
    }
    for (timestamp, bytes) in packets {
        push_raw(&mut producer, *timestamp, bytes);
    }
    inner.connected = Some(PortInfo {
        id: "fixture".into(),
        name: "Fixture MIDI input".into(),
    });
}

/// Reconcile the fixture/real connection as a disconnect, through the same
/// path the panel's DISCONNECT button uses.
pub fn request_disconnect(midi: &Midi) {
    midi.inner_mut().disconnect_requested = true;
}

/// Assign the current learn candidate (a CC) to a control action. Shared by the
/// panel buttons and tests.
pub fn assign(midi: &Midi, target: Assign) {
    let action = match target {
        Assign::Sustain => ControlAction::Sustain,
        Assign::AllNotesOff => ControlAction::AllNotesOff,
        Assign::Panic => ControlAction::Panic,
    };
    let outcome = midi.inner_mut().input.as_mut().and_then(|input| {
        let controller = input.controller_mut();
        let Some(MappingKey::Controller { channel, number }) = controller.take_learn_candidate()
        else {
            return None;
        };
        let mapping = Mapping::Controller {
            channel,
            number,
            action,
        };
        Some(match controller.set_mapping(mapping) {
            Ok(()) => Ok(format!(
                "Learned ch {} · cc {} → {:?}",
                channel.value() + 1,
                number.value(),
                target
            )),
            Err(error) => Err(format!("{:?}", error.0)),
        })
    });
    if let Some(result) = outcome {
        let mut inner = midi.inner_mut();
        match result {
            Ok(notice) => inner.notice = Some(notice),
            Err(error) => inner.error = Some(error),
        }
    }
}

/// Assign the current learn candidate (a note) to an explicit pad as a one-shot.
pub fn assign_to_pad(midi: &Midi, pad: PadId) {
    let outcome = midi.inner_mut().input.as_mut().and_then(|input| {
        let controller = input.controller_mut();
        let Some(MappingKey::Note { channel, pitch }) = controller.take_learn_candidate() else {
            return None;
        };
        let mapping = Mapping::Note {
            channel,
            pitch,
            target: NoteTarget::OneShot(pad),
        };
        Some(match controller.set_mapping(mapping) {
            Ok(()) => Ok(format!(
                "Learned ch {} · note {} → PAD {:02}",
                channel.value() + 1,
                pitch.value(),
                pad.index() + 1
            )),
            Err(error) => Err(format!("{:?}", error.0)),
        })
    });
    if let Some(result) = outcome {
        let mut inner = midi.inner_mut();
        match result {
            Ok(notice) => inner.notice = Some(notice),
            Err(error) => inner.error = Some(error),
        }
    }
}

/// Play every not-yet-mapped note of the learn candidate's channel on the
/// project instrument (held, D14). Existing pad mappings on it are kept.
pub fn assign_channel_to_instrument(midi: &Midi) {
    let outcome = midi.inner_mut().input.as_mut().and_then(|input| {
        let controller = input.controller_mut();
        let Some(MappingKey::Note { channel, .. }) = controller.take_learn_candidate() else {
            return None;
        };
        let mut entries = controller.mappings().entries().to_vec();
        let before = entries.len();
        for pitch in 0..=127 {
            let Ok(pitch) = Pitch::try_from(pitch) else {
                continue;
            };
            let key = MappingKey::Note { channel, pitch };
            if entries.iter().all(|mapping| mapping.key() != key) {
                entries.push(Mapping::Note {
                    channel,
                    pitch,
                    target: NoteTarget::Held(INSTRUMENT),
                });
            }
        }
        let added = entries.len() - before;
        Some(match Mappings::new(entries) {
            Ok(mappings) => {
                controller.set_mappings(mappings);
                Ok(format!(
                    "ch {} → INSTRUMENT: {added} note(s) mapped as held; pad notes kept",
                    channel.value() + 1
                ))
            }
            Err(error) => Err(format!("{:?}", error.0)),
        })
    });
    if let Some(result) = outcome {
        let mut inner = midi.inner_mut();
        match result {
            Ok(notice) => inner.notice = Some(notice),
            Err(error) => inner.error = Some(error),
        }
    }
}

/// Runs before the performance key handler, like the other focused modules.
pub fn on_key(s: &mut State, midi: &Midi, event: &KeyEvent, _tx: &mut Tx) -> bool {
    let _ = s;
    let mut inner = midi.inner_mut();
    if !inner.visible {
        return false;
    }
    if event.key == Key::Escape {
        inner.visible = false;
        true
    } else {
        false
    }
}

/// Header toggle, mounted next to the audio-output header.
pub fn header<S: 'static>(_s: &State, midi: &Midi) -> El {
    let inner = midi.inner();
    let label = if let Some(port) = &inner.connected {
        format!("MIDI · {}", port.name)
    } else {
        "MIDI INPUT".to_string()
    };
    let toggle = midi.clone();
    button(
        "midi-toggle",
        if inner.visible {
            "BACK TO PADS"
        } else {
            label.as_str()
        },
    )
    .label(if inner.visible {
        "Close MIDI input settings"
    } else {
        "Open MIDI input settings"
    })
    .checked(inner.visible)
    .help("MIDI input device, learn and status · machine preference, not project data")
    .on_press(move |_s: &mut S| {
        let mut inner = toggle.inner_mut();
        inner.visible = !inner.visible;
        if inner.visible {
            inner.refresh_requested = true;
        }
    })
}

/// The focused panel: selection, learn, mappings and status.
pub fn panel<S: 'static>(s: &State, midi: &Midi) -> El {
    let inner = midi.inner();
    let status_text = status(s, midi);
    let mut body = col()
        .key("midi-panel")
        .role("group")
        .label("MIDI input settings")
        .grow(1.0)
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .padding(12.0)
        .gap(8.0)
        .fill(ink("#101b16"))
        .child(caption("MIDI INPUT · DEVICE CHOICE IS MACHINE-LOCAL (D8)"))
        .child(
            caption(&status_text)
                .wrap()
                .key("midi-status")
                .role("status")
                .label(&status_text),
        );
    if let Some(notice) = &inner.notice {
        body = body.child(
            caption(notice)
                .wrap()
                .key("midi-notice")
                .role("status")
                .label(notice),
        );
    }
    if let Some(error) = &inner.error {
        body = body.child(
            caption(error)
                .wrap()
                .key("midi-error")
                .role("alert")
                .label(error)
                .color(ink("#ffad92")),
        );
    }
    if let Some(error) = &inner.preference_error {
        body = body.child(
            caption(error)
                .wrap()
                .key("midi-preference-error")
                .role("alert")
                .label(error)
                .color(ink("#ffad92")),
        );
    }
    // Device selection: one radio per enumerated port. Names are not unique and
    // are only a reconnect hint (D8); the backend ID selects.
    let selected_id = inner
        .input
        .as_ref()
        .and_then(|input| input.selected().map(|port| port.id.clone()));
    let none_checked = selected_id.is_none() && inner.connected.is_none();
    let mut ports = col().key("midi-ports").gap(4.0).child(
        button("midi-port-none", "No input (disconnect)")
            .role("radio")
            .checked(none_checked)
            .focus_selected(none_checked)
            .on_press({
                let midi = midi.clone();
                move |_s: &mut S| {
                    let mut inner = midi.inner_mut();
                    if let Some(input) = &mut inner.input {
                        input.select_hint(None, None);
                    }
                    inner.disconnect_requested = true;
                }
            }),
    );
    for (index, port) in inner.ports.iter().enumerate() {
        let id = port.id.clone();
        let name = port.name.clone();
        let checked = selected_id.as_deref() == Some(id.as_str());
        ports = ports.child(
            button(&format!("midi-port-{index}"), &format!("{} · {}", name, id))
                .role("radio")
                .checked(checked)
                .focus_selected(checked)
                .on_press({
                    let midi = midi.clone();
                    move |_s: &mut S| {
                        let mut inner = midi.inner_mut();
                        if let Some(input) = &mut inner.input {
                            input.select(PortInfo {
                                id: id.clone(),
                                name: name.clone(),
                            });
                        }
                    }
                }),
        );
    }
    body = body
        .child(ports.role("radiogroup").focus_group().label("MIDI input port"))
        .child(
            row().gap(8.0)
                .child(button("midi-refresh", "REFRESH INPUTS").on_press({
                    let midi = midi.clone();
                    move |_s: &mut S| midi.inner_mut().refresh_requested = true
                }))
                .child(button("midi-connect", "CONNECT").on_press({
                    let midi = midi.clone();
                    move |_s: &mut S| midi.inner_mut().connect_requested = true
                }))
                .child(button("midi-disconnect", "DISCONNECT").on_press({
                    let midi = midi.clone();
                    move |_s: &mut S| midi.inner_mut().disconnect_requested = true
                })),
        )
        .child(
            caption("Connect opens one owned connection; a vanished port is reconciled on refresh, releasing held notes and reporting loss. Mappings never change silently.").wrap(),
        );
    // Learn row.
    let Some(input) = &inner.input else {
        return body;
    };
    let controller = input.controller();
    let learn = controller.learn();
    let candidate = controller.learn_candidate();
    let mut learn_row = row()
        .gap(8.0)
        .center_y()
        .child(
            button("midi-learn", if learn { "LEARN  ON" } else { "LEARN  OFF" })
                .role("switch")
                .checked(learn)
                .on_press({
                    let midi = midi.clone();
                    move |_s: &mut S| {
                        let mut inner = midi.inner_mut();
                        if let Some(input) = &mut inner.input {
                            let on = !input.controller().learn();
                            input.controller_mut().set_learn(on);
                        }
                    }
                }),
        )
        .child(
            caption(match candidate {
                Some(key) => format!("LEARN · captured: {}", describe_key(key)),
                None if learn => "LEARN · play a note or turn a knob".to_string(),
                None => "LEARN · off".to_string(),
            })
            .key("midi-learn-candidate")
            .role("status")
            .label("Learn mode and captured candidate"),
        );
    if let Some(key) = candidate {
        match key {
            MappingKey::Note { .. } => {
                let pad = pad_id(s.selected);
                learn_row = learn_row.child(
                    button(
                        "midi-assign-pad",
                        &format!("MAP TO PAD {:02}", pad.index() + 1),
                    )
                    .on_press({
                        let midi = midi.clone();
                        move |_s: &mut S| assign_to_pad(&midi, pad)
                    }),
                );
                learn_row = learn_row.child(
                    button("midi-assign-instrument", "WHOLE CHANNEL → INSTRUMENT").on_press({
                        let midi = midi.clone();
                        move |_s: &mut S| assign_channel_to_instrument(&midi)
                    }),
                );
            }
            MappingKey::Controller { .. } => {
                for (key_name, label, action) in [
                    ("midi-assign-sustain", "SUSTAIN", Assign::Sustain),
                    (
                        "midi-assign-all-notes-off",
                        "ALL NOTES OFF",
                        Assign::AllNotesOff,
                    ),
                    ("midi-assign-panic", "PANIC", Assign::Panic),
                ] {
                    learn_row = learn_row.child(button(key_name, label).on_press({
                        let midi = midi.clone();
                        move |_s: &mut S| assign(&midi, action)
                    }));
                }
            }
        }
    }
    body = body.child(learn_row).child(
        caption("Learn captures a note-on or CC only — never a release. A note maps to the selected pad (one-shot) or its whole channel to the instrument (held); a CC maps to sustain, all-notes-off or panic.").wrap(),
    );
    // Mapping list (project DTOs; #100 owns persisting them in the project).
    let mut list = col().key("midi-mappings").gap(4.0);
    // A whole channel on the instrument is one row, not 128.
    let mut held = std::collections::BTreeMap::<u8, Vec<MappingKey>>::new();
    for mapping in controller.mappings().entries() {
        if let Mapping::Note {
            channel,
            target: NoteTarget::Held(_),
            ..
        } = mapping
        {
            held.entry(channel.value()).or_default().push(mapping.key());
        }
    }
    held.retain(|_, keys| keys.len() > 1);
    for (channel, keys) in &held {
        let text = format!(
            "ch {} · {} notes → HELD instrument",
            channel + 1,
            keys.len()
        );
        let keys = keys.clone();
        list = list.child(
            row()
                .gap(8.0)
                .center_y()
                .child(
                    caption(&text)
                        .key(format!("midi-held-channel-{channel}"))
                        .role("status")
                        .label(&text),
                )
                .child(
                    button(&format!("midi-remove-held-{channel}"), "REMOVE").on_press({
                        let midi = midi.clone();
                        move |_s: &mut S| {
                            let mut inner = midi.inner_mut();
                            if let Some(input) = &mut inner.input {
                                for key in &keys {
                                    input.controller_mut().remove_mapping(*key);
                                }
                            }
                        }
                    }),
                ),
        );
    }
    for (index, mapping) in controller.mappings().entries().iter().enumerate() {
        if let Mapping::Note {
            channel,
            target: NoteTarget::Held(_),
            ..
        } = mapping
            && held.contains_key(&channel.value())
        {
            continue;
        }
        let key = mapping.key();
        let text = describe_mapping(mapping);
        list = list.child(
            row()
                .gap(8.0)
                .center_y()
                .child(
                    caption(&text)
                        .key(format!("midi-mapping-{index}"))
                        .role("status")
                        .label(&text),
                )
                .child(button(&format!("midi-remove-{index}"), "REMOVE").on_press({
                    let midi = midi.clone();
                    move |_s: &mut S| {
                        let mut inner = midi.inner_mut();
                        if let Some(input) = &mut inner.input {
                            input.controller_mut().remove_mapping(key);
                        }
                    }
                })),
        );
    }
    body.child(list).child(
        caption("Mappings are project data keyed by channel + note/CC (D8) and are saved with the project.").wrap(),
    )
}

#[derive(Clone, Copy, Debug)]
pub enum SceneKind {
    Connected,
    Disconnected,
    Learn,
    Unavailable,
}

/// A sink that records actions, for scenes and tests.
#[derive(Default)]
pub struct VecSink {
    pub actions: Vec<Action>,
}

impl Sink for VecSink {
    fn dispatch(&mut self, action: Action) -> bool {
        self.actions.push(action);
        true
    }
}

fn push_raw(producer: &mut rtrb::Producer<input::RawMessage>, ts: u64, bytes: &[u8]) {
    if let Some(raw) = input::RawMessage::new(ts, bytes) {
        let _ = producer.push(raw);
    }
}

/// Deterministic fixture scenes; a scene never runs the backend or touches files.
pub fn scene(s: &mut State, midi: &Midi, kind: SceneKind) {
    {
        let mut inner = midi.inner_mut();
        inner.visible = true;
        inner.error = None;
        inner.notice = None;
        inner.ports = vec![
            PortInfo {
                id: "scene-iac".into(),
                name: "IAC Driver Bus 1".into(),
            },
            PortInfo {
                id: "scene-other".into(),
                name: "Scene Other".into(),
            },
        ];
        if matches!(kind, SceneKind::Unavailable) {
            inner.input = None;
            return;
        }
    }
    // Take the input out of the shared cell while the fixture is installed.
    let mut owned = midi.inner_mut().input.take();
    let mut connected = None;
    let mut notice = None;
    if let Some(input) = owned.as_mut() {
        input.select_hint(Some("scene-iac".into()), Some("IAC Driver Bus 1".into()));
        let (mut producer, consumer) = rtrb::RingBuffer::new(input::RAW_QUEUE_CAPACITY);
        {
            let controller = input.controller_mut();
            controller.attach(consumer);
            controller.begin_connection(SourceId(42), OriginalTime::from_nanos(0));
            // Build the fixture mappings through the project DTO boundary.
            let document: input::MappingsDocument = serde_json::from_str(&format!(
                r#"{{"version":1,"mappings":[
                    {{"type":"note","channel":1,"pitch":40,"target":{{"type":"one_shot","pad":{}}}}},
                    {{"type":"controller","channel":1,"number":64,"action":"sustain"}}
                ]}}"#,
                s.selected
            ))
            .unwrap_or_else(|error| panic!("scene document: {error}"));
            if let Ok(mappings) = document.to_mappings() {
                controller.set_mappings(mappings);
            }
        }
        match kind {
            SceneKind::Connected => {
                connected = Some(PortInfo {
                    id: "scene-iac".into(),
                    name: "IAC Driver Bus 1".into(),
                });
                push_raw(&mut producer, 10, &[0x91, 40, 100]);
            }
            SceneKind::Disconnected => {
                push_raw(&mut producer, 10, &[0x91, 40, 100]);
                push_raw(&mut producer, 20, &[0x81, 40, 0]);
                push_raw(&mut producer, 30, &[0xf8]);
                // Drain while the epoch is still valid, then reconcile.
                let _ = input.poll(&mut VecSink::default());
                let released = input
                    .controller_mut()
                    .end_connection(&mut VecSink::default(), OriginalTime::from_nanos(40_000));
                notice = Some(format!(
                    "Disconnected: {released} held note(s) released; mappings unchanged"
                ));
            }
            SceneKind::Learn => {
                connected = Some(PortInfo {
                    id: "scene-iac".into(),
                    name: "IAC Driver Bus 1".into(),
                });
                input.controller_mut().set_learn(true);
                push_raw(&mut producer, 10, &[0x91, 40, 90]);
            }
            SceneKind::Unavailable => {}
        }
    }
    {
        let mut inner = midi.inner_mut();
        inner.input = owned;
        inner.connected = connected;
        inner.notice = notice;
    }
    poll(s, midi);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::ui_test_support;
    use brownnote_native::{
        engine::QUEUE_CAPACITY,
        midi::{mapping::Mappings, parser::Data7},
        pedals::offline::{Backend, Hash256, HelperCommand, Plugin, Stage},
        performance::{Channel, InstrumentId, Pitch},
        plugins::live::{LiveState, identify},
    };
    use rune::{Cx, El, Env, Key, KeyEvent, Mods, Runtime, Tx};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering::Relaxed},
        thread,
        time::{Duration, Instant},
    };

    struct App {
        state: State,
        midi: Midi,
    }

    fn app() -> App {
        let kit = ui_test_support::kit(0.25, 44_100, 44_100);
        let mut state = State::new(kit, true, true);
        state.pedals.auto_load = false;
        App {
            state,
            midi: Midi::disabled(),
        }
    }

    fn view(app: &App, _cx: &mut Cx<App>) -> El {
        rune::col()
            .child(header::<App>(&app.state, &app.midi))
            .child(panel::<App>(&app.state, &app.midi))
    }

    fn app_on_key(app: &mut App, event: &KeyEvent, tx: &mut Tx) -> bool {
        on_key(&mut app.state, &app.midi, event, tx)
    }

    fn rt(app: App) -> Runtime<App> {
        let mut rt = Runtime::new(app, view, (820.0, 600.0), Env::new(true, 2.0));
        rt.on_key = Some(app_on_key);
        rt.set_full_keyboard_access(true);
        rt.frame(0.0);
        rt
    }

    fn key(rt: &mut Runtime<App>, key: Key) {
        rt.key(KeyEvent {
            key,
            mods: Mods::default(),
        });
        rt.frame(0.0);
    }

    fn focus(rt: &mut Runtime<App>, target: &str) {
        for _ in 0..80 {
            if rt
                .elements()
                .iter()
                .any(|e| e.key.as_deref() == Some(target) && e.focused)
            {
                return;
            }
            key(rt, Key::Tab);
        }
        panic!("Tab never reached {target}");
    }

    fn ch(value: u8) -> Channel {
        Channel::try_from(value).unwrap()
    }
    fn pitch(value: u8) -> Pitch {
        Pitch::try_from(value).unwrap()
    }
    fn cc(value: u8) -> Data7 {
        Data7::try_from(value).unwrap()
    }

    /// A connected, deterministic input inside the app: mapping ch2/note40 →
    /// pad `pad`, plus the producer half of the raw queue.
    fn connected(app: &mut App, pad: u8) -> rtrb::Producer<input::RawMessage> {
        let midi = app.midi.clone();
        let mut inner = midi.inner_mut();
        let input = inner.input.as_mut().unwrap();
        let (producer, consumer) = rtrb::RingBuffer::new(input::RAW_QUEUE_CAPACITY);
        let controller = input.controller_mut();
        controller.attach(consumer);
        controller.begin_connection(SourceId(7), OriginalTime::from_nanos(1_000));
        controller
            .set_mapping(Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target: NoteTarget::OneShot(PadId::try_from(pad).unwrap()),
            })
            .unwrap();
        inner.connected = Some(PortInfo {
            id: "test".into(),
            name: "Test Port".into(),
        });
        drop(inner);
        producer
    }

    fn push(producer: &mut rtrb::Producer<input::RawMessage>, ts: u64, bytes: &[u8]) {
        producer
            .push(input::RawMessage::new(ts, bytes).unwrap())
            .unwrap();
    }

    fn counters(app: &App) -> input::Counters {
        app.midi
            .inner()
            .input
            .as_ref()
            .unwrap()
            .controller()
            .counters()
    }

    struct Root(std::path::PathBuf);
    impl Root {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "brownnote-midi-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn preferences_round_trip_and_reject_corrupt_files() {
        let root = Root::new();
        assert_eq!(
            preferences::load(&root.0).unwrap(),
            (None, None),
            "missing file starts empty without writing"
        );
        preferences::save(&root.0, Some("port-1"), Some("IAC Driver Bus 1")).unwrap();
        assert_eq!(
            preferences::load(&root.0).unwrap(),
            (
                Some("port-1".to_string()),
                Some("IAC Driver Bus 1".to_string())
            )
        );
        fs::write(root.0.join("midi-input.json"), "{ not json").unwrap();
        assert!(preferences::load(&root.0).is_err());
        fs::write(
            root.0.join("midi-input.json"),
            r#"{"version":2,"port_id":null,"port_name":null}"#,
        )
        .unwrap();
        assert!(preferences::load(&root.0).is_err());
        fs::write(
            root.0.join("midi-input.json"),
            r#"{"version":1,"port_id":"","port_name":null}"#,
        )
        .unwrap();
        assert!(preferences::load(&root.0).is_err());
        // A failed save leaves the last complete file intact.
        let before = fs::read_to_string(root.0.join("midi-input.json")).unwrap();
        assert!(preferences::save(&root.0.join("missing").join("x"), Some("a"), Some("b")).is_ok());
        assert_eq!(
            fs::read_to_string(root.0.join("midi-input.json")).unwrap(),
            before
        );
    }

    #[test]
    fn poll_dispatches_midi_note_through_the_real_command_queue() {
        let mut app = app();
        app.state.selected = 4;
        let mut producer = connected(&mut app, 4);
        push(&mut producer, 10, &[0x91, 40, 100]);
        assert!(poll(&mut app.state, &app.midi));
        assert_eq!(
            app.state.selected, 4,
            "the pad the note mapped to is selected"
        );
        assert!(
            app.state.flashes[4] > app.state.now,
            "pad flash set on accepted trigger"
        );
        assert_eq!(counters(&app).triggered, 1);
        assert_eq!(counters(&app).lost_commands, 0);
        // The trigger really reached the engine queue: the mixer voices the pad.
        app.state.output.advance(0.01);
        assert!(app.state.output.meters.counts()[4] > 0);
        // Full velocity stays distinct: 1 vs 127 produce separate triggers.
        push(&mut producer, 20, &[0x91, 40, 1]);
        push(&mut producer, 30, &[0x91, 40, 127]);
        poll(&mut app.state, &app.midi);
        assert_eq!(counters(&app).triggered, 3);
        // The same note on another channel does not respond.
        push(&mut producer, 40, &[0x90, 40, 127]);
        poll(&mut app.state, &app.midi);
        assert_eq!(counters(&app).triggered, 3);
        assert_eq!(counters(&app).unmapped, 1);
    }

    #[test]
    fn queue_saturation_is_counted_and_later_input_continues() {
        let mut app = app();
        let mut producer = connected(&mut app, 0);
        for _ in 0..QUEUE_CAPACITY {
            assert!(app.state.output.send(Command::Panic));
        }
        assert!(!app.state.output.send(Command::Panic));
        for index in 0..4u64 {
            push(&mut producer, 10 + index, &[0x91, 40, 100]);
        }
        poll(&mut app.state, &app.midi);
        assert_eq!(
            counters(&app).lost_commands,
            4,
            "every refused dispatch is counted"
        );
        assert_eq!(counters(&app).triggered, 0);
        assert!(status(&app.state, &app.midi).contains("commands 4"));
        app.state.output.advance(0.01);
        push(&mut producer, 100, &[0x91, 40, 100]);
        poll(&mut app.state, &app.midi);
        assert_eq!(
            counters(&app).triggered,
            1,
            "later input still works after the burst"
        );
        assert_eq!(counters(&app).lost_commands, 4);
    }

    #[test]
    fn a_clock_anomaly_is_visible_in_the_status_line() {
        let mut app = app();
        app.state.selected = 2;
        let mut producer = connected(&mut app, 2);
        push(&mut producer, 20, &[0x91, 40, 100]);
        // A release before the connection's first packet has no take time: it
        // is counted and not dispatched, so the status line must say so.
        push(&mut producer, 10, &[0x81, 40, 64]);
        poll(&mut app.state, &app.midi);
        assert_eq!(counters(&app).clock_anomalies, 1);
        assert_eq!(counters(&app).triggered, 1);
        assert_eq!(counters(&app).unmatched_releases, 0);
        let text = status(&app.state, &app.midi);
        assert!(
            text.contains("commands 0 · stale 0 · unmatched 0 · clock 1"),
            "the counted anomaly is reported in the LOSS group: {text}"
        );
    }

    #[test]
    fn the_status_line_separates_unsupported_classes_from_malformed_packets() {
        let mut app = app();
        app.state.selected = 2;
        let mut producer = connected(&mut app, 2);
        push(&mut producer, 10, &[0xe0, 0, 64]); // pitch bend: valid, unsupported
        push(&mut producer, 11, &[0xf8]); // real-time clock: valid, unsupported
        push(&mut producer, 12, &[0x90, 40]); // truncated: malformed
        push(&mut producer, 13, &[0x91, 40, 100]); // valid and mapped
        poll(&mut app.state, &app.midi);
        assert_eq!(counters(&app).unsupported, 2);
        assert_eq!(counters(&app).malformed, 1);
        assert_eq!(counters(&app).triggered, 1);
        let text = status(&app.state, &app.midi);
        assert!(
            text.contains(
                "received 4 · malformed 1 · unsupported 2 · unmapped 0 · triggered 1 · held 0 active"
            ),
            "both rejection classes are reported in the status group: {text}"
        );
        assert!(
            !text.contains("LOSS"),
            "neither rejection class is reported as loss: {text}"
        );
    }

    #[test]
    fn learn_captures_then_assigns_a_mapping_to_the_selected_pad() {
        let mut app = app();
        app.state.selected = 9;
        let mut producer = connected(&mut app, 9);
        app.midi
            .inner_mut()
            .input
            .as_mut()
            .unwrap()
            .controller_mut()
            .set_learn(true);
        push(&mut producer, 10, &[0x91, 40, 90]);
        poll(&mut app.state, &app.midi);
        {
            let inner = app.midi.inner();
            let controller = inner.input.as_ref().unwrap().controller();
            assert_eq!(
                controller.learn_candidate(),
                Some(MappingKey::Note {
                    channel: ch(1),
                    pitch: pitch(40)
                })
            );
            assert_eq!(
                controller.counters().triggered,
                0,
                "learn captures, does not trigger"
            );
        }
        assign_to_pad(&app.midi, PadId::try_from(9).unwrap());
        {
            let inner = app.midi.inner();
            let mappings: &Mappings = inner.input.as_ref().unwrap().controller().mappings();
            assert!(
                mappings.entries().iter().any(|m| *m
                    == Mapping::Note {
                        channel: ch(1),
                        pitch: pitch(40),
                        target: NoteTarget::OneShot(PadId::try_from(9).unwrap()),
                    }),
                "the learned candidate became a mapping"
            );
        }
        // A release never becomes a candidate.
        push(&mut producer, 20, &[0x81, 40, 0]);
        poll(&mut app.state, &app.midi);
        let inner = app.midi.inner();
        assert_eq!(
            inner.input.as_ref().unwrap().controller().learn_candidate(),
            None
        );
    }

    #[test]
    fn cc_learn_assigns_sustain_and_panic_actions() {
        let mut app = app();
        let mut producer = connected(&mut app, 0);
        app.midi
            .inner_mut()
            .input
            .as_mut()
            .unwrap()
            .controller_mut()
            .set_learn(true);
        push(&mut producer, 10, &[0xb1, 64, 127]);
        poll(&mut app.state, &app.midi);
        assign(&app.midi, Assign::Sustain);
        push(&mut producer, 20, &[0xb1, 120, 0]);
        poll(&mut app.state, &app.midi);
        assign(&app.midi, Assign::Panic);
        let inner = app.midi.inner();
        let entries = inner
            .input
            .as_ref()
            .unwrap()
            .controller()
            .mappings()
            .entries();
        assert!(entries.contains(&Mapping::Controller {
            channel: ch(1),
            number: cc(64),
            action: ControlAction::Sustain,
        }));
        assert!(entries.contains(&Mapping::Controller {
            channel: ch(1),
            number: cc(120),
            action: ControlAction::Panic,
        }));
    }

    #[test]
    fn view_buttons_toggle_learn_request_refresh_and_stay_in_bounds() {
        let app = app();
        {
            let mut inner = app.midi.inner_mut();
            inner.visible = true;
            inner.ports = vec![PortInfo {
                id: "p1".into(),
                name: "Port One".into(),
            }];
        }
        let mut rt = rt(app);
        for (key, role) in [
            ("midi-status", "status"),
            ("midi-port-0", "radio"),
            ("midi-port-none", "radio"),
            ("midi-learn", "switch"),
            ("midi-refresh", "button"),
            ("midi-connect", "button"),
            ("midi-disconnect", "button"),
            ("midi-toggle", "button"),
        ] {
            let element = rt
                .elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some(key))
                .unwrap_or_else(|| panic!("missing element {key}"));
            assert_eq!(element.role, role, "{key}");
        }
        // Learn toggle via the real keyboard path.
        focus(&mut rt, "midi-learn");
        key(&mut rt, Key::Text(" ".into()));
        assert!(
            rt.state
                .midi
                .inner()
                .input
                .as_ref()
                .unwrap()
                .controller()
                .learn()
        );
        // Refresh is queued for poll.
        focus(&mut rt, "midi-refresh");
        key(&mut rt, Key::Text(" ".into()));
        assert!(rt.state.midi.inner().refresh_requested);
        // Selecting a port radio only changes the draft selection: Tab stops at
        // the radio group, arrow keys move inside it (like the output panel).
        focus(&mut rt, "midi-port-none");
        key(&mut rt, Key::Down);
        assert_eq!(
            rt.state
                .midi
                .inner()
                .input
                .as_ref()
                .unwrap()
                .selected()
                .map(|p| p.id.as_str()),
            Some("p1")
        );
        // Layout: focused controls stay inside the viewport. (A radio group
        // keeps only its focused radio in the Tab cycle, so the port radio is
        // checked through `presented` instead of `focus`.)
        for target in [
            "midi-learn",
            "midi-refresh",
            "midi-connect",
            "midi-disconnect",
            "midi-toggle",
        ] {
            focus(&mut rt, target);
            let [x, y, w, h] = rt.presented(target).unwrap().0;
            assert!(
                x >= 0.0 && y >= 0.0 && x + w <= 820.1 && y + h <= 600.1,
                "{target}: {x}, {y}, {w}, {h}"
            );
            assert!(w > 0.0 && h > 0.0);
        }
        let [x, y, w, h] = rt.presented("midi-port-0").unwrap().0;
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= 820.1 && y + h <= 600.1,
            "midi-port-0: {x}, {y}, {w}, {h}"
        );
    }

    #[test]
    fn scenes_render_and_escape_closes_the_panel() {
        for kind in [
            SceneKind::Connected,
            SceneKind::Disconnected,
            SceneKind::Learn,
            SceneKind::Unavailable,
        ] {
            let mut app = app();
            scene(&mut app.state, &app.midi, kind);
            let mut rt = rt(app);
            assert!(rt.find("midi-panel").is_some(), "{kind:?}");
            assert!(rt.find("midi-status").is_some(), "{kind:?}");
            assert!(
                rt.elements()
                    .iter()
                    .any(|e| e.key.as_deref() == Some("midi-status") && e.role == "status"),
                "{kind:?}"
            );
            // The Unavailable scene has no learn row (no backend input).
            let targets: &[&str] = if matches!(kind, SceneKind::Unavailable) {
                &["midi-refresh", "midi-toggle"]
            } else {
                &["midi-learn", "midi-refresh", "midi-toggle"]
            };
            for target in targets {
                focus(&mut rt, target);
                let [x, y, w, h] = rt.presented(target).unwrap().0;
                assert!(
                    x >= 0.0 && y >= 0.0 && x + w <= 820.1 && y + h <= 600.1,
                    "{kind:?} {target}: {x}, {y}, {w}, {h}"
                );
            }
            // Escape closes the panel through the real on_key path.
            key(&mut rt, Key::Escape);
            assert!(!rt.state.midi.inner().visible);
        }
        // Learn scene captured a candidate without triggering.
        let mut fixture = app();
        scene(&mut fixture.state, &fixture.midi, SceneKind::Learn);
        let inner = fixture.midi.inner();
        let controller = inner.input.as_ref().unwrap().controller();
        assert_eq!(
            controller.learn_candidate(),
            Some(MappingKey::Note {
                channel: ch(1),
                pitch: pitch(40)
            })
        );
        assert_eq!(controller.counters().triggered, 0);
        // Disconnected scene reports the reconcile and the unsupported clock.
        let mut fixture = app();
        scene(&mut fixture.state, &fixture.midi, SceneKind::Disconnected);
        let inner = fixture.midi.inner();
        let controller = inner.input.as_ref().unwrap().controller();
        assert_eq!(controller.counters().disconnects, 1);
        assert_eq!(controller.counters().unsupported, 1);
        assert_eq!(controller.counters().malformed, 0);
        assert!(inner.connected.is_none());
        assert!(status(&fixture.state, &fixture.midi).contains("DISCONNECTED"));
    }

    /// The live helper and fixture, or `None` (a skip) unless
    /// `BROWNNOTE_REQUIRE_CLAP=1`, which `make native-test` sets.
    fn live_helper() -> Option<(HelperCommand, PathBuf)> {
        let require = std::env::var("BROWNNOTE_REQUIRE_CLAP").as_deref() == Ok("1");
        let helper = std::env::var_os("BROWNNOTE_CLAP_LIVE_HELPER")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("BROWNNOTE_CLAP_HELPER").and_then(|offline| {
                    std::path::Path::new(&offline)
                        .parent()
                        .map(|dir| dir.join("brownnote-clap-live"))
                })
            });
        let fixture = std::env::var_os("BROWNNOTE_CLAP_FAULT_FIXTURE").map(PathBuf::from);
        match (helper, fixture) {
            (Some(helper), Some(fixture)) if helper.is_file() && fixture.is_file() => Some((
                HelperCommand {
                    program: helper,
                    args: Vec::new(),
                },
                fixture,
            )),
            other => {
                assert!(
                    !require,
                    "BROWNNOTE_REQUIRE_CLAP=1 but the live helper or fixture is missing: {other:?}"
                );
                eprintln!("skipping: live CLAP helper or fixture not built ({other:?})");
                None
            }
        }
    }

    /// The full input path: a mapped held note, release, sustain and panic are
    /// forwarded to a mounted live instrument through the one backend sender.
    #[test]
    fn held_notes_sustain_and_panic_reach_a_mounted_live_instrument() {
        let Some((helper, fixture)) = live_helper() else {
            return;
        };
        let mut app = app();
        app.state.live.set_helper(helper);
        let info = identify(
            &app.state.live.helper.clone().unwrap(),
            "org.brownnote.fixture.instrument",
            &fixture,
            48_000,
            Duration::from_secs(5),
        )
        .unwrap();
        let stage = Stage {
            plugin: info.plugin,
            location: fixture,
            params: Vec::new(),
            state: None,
        };
        let mut voice = app
            .state
            .live
            .mount(stage, 48_000, 256)
            .unwrap_or_else(|| panic!("mount: {:?}", app.state.live.error));

        let mut producer = connected(&mut app, 0);
        {
            let midi = app.midi.clone();
            let mut inner = midi.inner_mut();
            let controller = inner.input.as_mut().unwrap().controller_mut();
            controller
                .set_mapping(Mapping::Note {
                    channel: ch(1),
                    pitch: pitch(40),
                    target: NoteTarget::Held(InstrumentId(1)),
                })
                .unwrap();
            controller
                .set_mapping(Mapping::Controller {
                    channel: ch(1),
                    number: cc(64),
                    action: ControlAction::Sustain,
                })
                .unwrap();
            controller
                .set_mapping(Mapping::Controller {
                    channel: ch(1),
                    number: cc(120),
                    action: ControlAction::Panic,
                })
                .unwrap();
        }
        push(&mut producer, 10, &[0x91, 40, 100]);
        push(&mut producer, 20, &[0x81, 40, 0]);
        push(&mut producer, 30, &[0xb1, 64, 127]);
        push(&mut producer, 40, &[0x91, 40, 100]);
        push(&mut producer, 50, &[0x81, 40, 0]);
        push(&mut producer, 60, &[0xb1, 64, 0]);
        push(&mut producer, 70, &[0xb1, 120, 0]);
        assert!(poll(&mut app.state, &app.midi));

        let status = app
            .state
            .live
            .status()
            .unwrap_or_else(|| unreachable!("mounted"));
        assert_eq!(
            status.counters.accepted_events, 7,
            "note on/off, sustain on/off and panic reached the live sender: {status:?}"
        );

        // Advance the callback's play cursor so the worker renders the frames
        // the senders named, then wait for the panic to be applied.
        let mut buffer = [0.0f32; 512];
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = status;
        while status.counters.panics == 0 {
            voice.render_add(&mut buffer, 2);
            thread::sleep(Duration::from_millis(1));
            assert!(Instant::now() < deadline, "worker stalled: {status:?}");
            status = app.state.live.status().unwrap_or(status);
        }
        assert_eq!(status.state, LiveState::Running);
        assert_eq!(status.counters.panics, 1);
    }

    /// A helper that cannot start produces no voice and leaves the app, its
    /// mixer and its pads working.
    #[test]
    fn a_live_helper_that_fails_to_start_keeps_the_app_working() {
        let mut app = app();
        app.state.live.set_helper(HelperCommand {
            program: PathBuf::from("/nonexistent/brownnote-clap-live"),
            args: Vec::new(),
        });
        let stage = Stage {
            plugin: Plugin {
                backend: Backend::Clap,
                id: "org.brownnote.missing.instrument".into(),
                artifact: Hash256::of(b"missing"),
                abi: "clap:1.2".into(),
                version: "0".into(),
            },
            location: PathBuf::from("/nonexistent/missing.clap"),
            params: Vec::new(),
            state: None,
        };
        assert!(app.state.live.mount(stage, 48_000, 256).is_none());
        assert!(!app.state.live.mounted());
        assert!(app.state.live.error.is_some());
        app.state.trigger(0, 1.0);
        app.state.output.advance(0.01);
        assert!(
            app.state.output.meters.counts()[0] > 0,
            "the sample mixer still plays"
        );
    }
}
