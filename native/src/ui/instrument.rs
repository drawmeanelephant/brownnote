//! The sampled instrument in the app (D18, #108): load one SF2 preset from an
//! explicit path, mount it in the output, sound held MIDI notes, replayed
//! takes and auditions, and keep it with the project.
//!
//! Reading and parsing the file run on a worker thread; only the finished
//! [`Font`] reaches the UI. Each output stream gets its own player, built at
//! that stream's rate, so a device change or project open remounts it.
use super::{State, button, caption, ink};
use brownnote_native::{
    engine::Command,
    import::MediaPublisher,
    instrument::{
        DEFAULT_GAIN, Font, INSTRUMENT, InstrumentDocument, InstrumentError, MAX_SOURCE_BYTES,
        RenderKey, describe,
    },
    output::Output,
    performance::{EventId, Note, NoteOn, StreamEpoch, TakeId, Target},
    project::{AssetId, AssetRef},
};
use rune::{El, Key, KeyEvent, TextEdit, TextStyle, Tx, col, row, text_input};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
};

#[cfg(test)]
#[path = "instrument/tests.rs"]
mod tests;

/// Auditions get their own identity space, apart from every take.
const AUDITION: TakeId = TakeId(u128::MAX - 2);
const AUDITION_PITCH: u8 = 60;
const AUDITION_VELOCITY: u8 = 100;
const AUDITION_SECONDS: f64 = 0.6;

/// Renders already frozen for this source and preset, shared with the save
/// worker so each note is rendered once per instrument, not once per save.
pub type Renders = Arc<Mutex<BTreeMap<RenderKey, AssetId>>>;

struct Loaded {
    bytes: Vec<u8>,
    font: Font,
    name: String,
}

#[derive(Default)]
pub struct Instrument {
    pub visible: bool,
    pub path: TextEdit,
    /// The project's instrument, kept even when this build cannot sound it.
    pub document: Option<InstrumentDocument>,
    font: Option<Font>,
    renders: Renders,
    /// Why the project's instrument is silent on this build.
    pub unavailable: Option<String>,
    loading: Option<mpsc::Receiver<Result<Loaded, String>>>,
    mounted: Option<StreamEpoch>,
    audition: Option<(EventId, f64)>,
    auditions: u64,
    /// Releases the command queue refused; retried first on every poll so a
    /// note is never left sounding.
    releases: Vec<EventId>,
    pub live: u64,
    pub notice: Option<String>,
    pub error: Option<String>,
    /// Test seam: hold the loader thread before it reads the file, so a busy
    /// instrument can be observed without racing a fast local read, as
    /// `Export::hold` does for the export worker.
    #[cfg(test)]
    pub(crate) load_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Instrument {
    pub fn busy(&self) -> bool {
        self.loading.is_some()
    }

    /// The saved instrument with its frozen renders, for a project snapshot.
    pub fn snapshot(&self) -> Option<(InstrumentDocument, Option<Font>, Renders)> {
        let mut document = self.document.clone()?;
        if let Ok(renders) = self.renders.lock() {
            for (key, asset) in renders.iter() {
                if document.render(*key).is_none() {
                    document
                        .renders
                        .push(brownnote_native::instrument::RenderDocument {
                            pitch: key.pitch,
                            velocity: key.velocity,
                            gate_frames: key.gate_frames,
                            asset: *asset,
                        });
                }
            }
        }
        Some((document, self.font.clone(), self.renders.clone()))
    }

    /// After a project opens: its instrument, and whether this build could
    /// prepare the saved source. View state carries over.
    pub fn restore(&mut self, saved: Option<(InstrumentDocument, Result<Font, InstrumentError>)>) {
        let renders: BTreeMap<RenderKey, AssetId> = saved
            .iter()
            .flat_map(|(d, _)| d.renders.iter().map(|r| (r.key(), r.asset)))
            .collect();
        let (document, font, unavailable) = match saved {
            None => (None, None, None),
            Some((document, Ok(font))) => (Some(document), Some(font), None),
            Some((document, Err(error))) => (
                Some(document),
                None,
                Some(format!(
                    "{error}. The notes and {} frozen render(s) are kept for export; live and replayed notes are silent.",
                    renders.len()
                )),
            ),
        };
        self.unavailable = unavailable;
        self.document = document;
        self.font = font;
        self.renders = Arc::new(Mutex::new(renders));
        self.loading = None;
        self.mounted = None;
        self.audition = None;
        self.releases.clear();
        self.error = None;
        self.notice = None;
    }

    /// Whether a held note on `target` sounds now.
    pub fn plays(&self, target: Target) -> bool {
        self.font.is_some() && target == Target::Instrument(INSTRUMENT)
    }
}

fn rate(output: &Output) -> u32 {
    output
        .opened()
        .map_or(44_100, |opened| opened.configuration.stream.sample_rate)
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| format!("Cannot open instrument: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read instrument: {error}"))?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(format!(
            "Instrument file is over the {} MiB limit",
            MAX_SOURCE_BYTES / 1024 / 1024
        ));
    }
    Ok(bytes)
}

/// Read and prepare the file at the typed path, off the UI thread.
pub fn load(s: &mut State) {
    if s.instrument.busy() {
        return;
    }
    let path = PathBuf::from(s.instrument.path.text.trim_end_matches(['\r', '\n']));
    if !path.is_absolute() {
        s.instrument.error = Some("Enter an absolute path to an .sf2 file.".into());
        return;
    }
    let (send, receive) = mpsc::sync_channel(1);
    #[cfg(test)]
    let load_hook = s.instrument.load_hook.clone();
    let spawned = thread::Builder::new()
        .name("brownnote-instrument".into())
        .spawn(move || {
            #[cfg(test)]
            {
                if let Some(hook) = load_hook {
                    hook();
                }
            }
            let result = read(&path).and_then(|bytes| {
                let font = Font::prepare(&bytes, None).map_err(|error| error.to_string())?;
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(Loaded { bytes, font, name })
            });
            let _ = send.send(result);
        });
    match spawned {
        Ok(_) => {
            s.instrument.loading = Some(receive);
            s.instrument.error = None;
            s.instrument.notice = Some("Loading instrument off the UI thread…".into());
        }
        Err(error) => s.instrument.error = Some(format!("Cannot start loading: {error}")),
    }
}

fn adopt(s: &mut State, loaded: Loaded) -> Result<(), String> {
    let reference = AssetRef::of(&loaded.bytes);
    s.media
        .owned
        .publish(&reference, &loaded.bytes)
        .map_err(|error| format!("Cannot own the instrument file: {error}"))?;
    let preset = loaded.font.preset();
    // The same file and preset again (a relink) keeps its frozen renders.
    let same = s
        .instrument
        .document
        .as_ref()
        .is_some_and(|d| d.source == reference.id && d.preset() == preset);
    let (gain, renders) = match (&s.instrument.document, same) {
        (Some(document), true) => (document.gain, s.instrument.renders.clone()),
        _ => (DEFAULT_GAIN, Renders::default()),
    };
    let mut document = describe(INSTRUMENT, &loaded.name, reference.id, preset, gain);
    if same && let Some(old) = &s.instrument.document {
        document.renders = old.renders.clone();
    }
    let i = &mut s.instrument;
    i.notice = Some(format!(
        "Loaded {} · preset {} \"{}\" · {} preset(s) in file",
        loaded.name,
        preset,
        loaded.font.name(),
        loaded.font.presets().len()
    ));
    i.document = Some(document);
    i.font = Some(loaded.font);
    i.renders = renders;
    i.unavailable = None;
    i.mounted = None;
    Ok(())
}

fn send_release(s: &mut State, id: EventId) {
    if !s.output.send(Command::HeldOff { id }) {
        s.instrument.releases.push(id);
    }
}

/// Call before MIDI and transport polling so they find the player mounted.
pub fn poll(s: &mut State) -> bool {
    let mut changed = false;
    let releases = std::mem::take(&mut s.instrument.releases);
    for id in releases {
        send_release(s, id);
    }
    if let Some(receive) = &s.instrument.loading {
        match receive.try_recv() {
            Ok(result) => {
                s.instrument.loading = None;
                match result.and_then(|loaded| adopt(s, loaded)) {
                    Ok(()) => s.instrument.error = None,
                    Err(error) => {
                        s.instrument.notice = None;
                        s.instrument.error = Some(error);
                    }
                }
                changed = true;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                s.instrument.loading = None;
                s.instrument.error = Some("Instrument loader stopped.".into());
                changed = true;
            }
        }
    }
    let epoch = s.output.scheduler().epoch();
    if s.instrument.mounted != Some(epoch)
        && let Some(font) = &s.instrument.font
    {
        let gain = s
            .instrument
            .document
            .as_ref()
            .map_or(DEFAULT_GAIN, |d| d.gain);
        match font.player(rate(&s.output), gain) {
            Ok(player) => {
                if s.output.send(Command::Instrument(player)) {
                    s.instrument.mounted = Some(epoch);
                    changed = true;
                }
            }
            Err(error) => {
                s.instrument.error = Some(format!("Instrument cannot play here: {error}"));
                s.instrument.font = None;
                changed = true;
            }
        }
    }
    if let Some((id, until)) = s.instrument.audition
        && s.now >= until
    {
        s.instrument.audition = None;
        send_release(s, id);
        changed = true;
    }
    changed
}

/// A live held note from the MIDI controller, by its own identity (D14).
pub fn held_start(s: &mut State, on: &NoteOn) {
    if s.instrument.plays(on.target)
        && s.output.send(Command::HeldOn {
            id: on.id,
            pitch: on.key.pitch.value(),
            velocity: on.velocity.value(),
        })
    {
        s.instrument.live += 1;
    }
}

pub fn held_end(s: &mut State, note: &Note) {
    if s.instrument.plays(note.on().target) {
        send_release(s, note.on().id);
    }
}

/// Play one fixed-length middle C through the mounted player.
pub fn audition(s: &mut State) {
    if s.instrument.font.is_none() {
        return;
    }
    if let Some((id, _)) = s.instrument.audition.take() {
        send_release(s, id);
    }
    s.instrument.auditions += 1;
    let id = EventId {
        take: AUDITION,
        sequence: s.instrument.auditions,
    };
    if s.output.send(Command::HeldOn {
        id,
        pitch: AUDITION_PITCH,
        velocity: AUDITION_VELOCITY,
    }) {
        s.instrument.audition = Some((id, s.now + AUDITION_SECONDS));
    }
}

pub fn wants_wake(s: &State) -> bool {
    s.instrument.busy() || s.instrument.audition.is_some() || !s.instrument.releases.is_empty()
}

/// Call before pad shortcuts: typing a path must not play pads.
pub fn on_key(s: &mut State, event: &KeyEvent, _tx: &mut Tx) -> bool {
    if !s.instrument.visible {
        return false;
    }
    if event.key == Key::Escape {
        s.instrument.visible = false;
        return true;
    }
    matches!(&event.key, Key::Text(_))
        && !event.mods.cmd
        && !event.mods.ctrl
        && !event.mods.alt
        && !event.mods.logo
}

/// One line: what is loaded, whether it sounds, and why not.
pub fn status(s: &State) -> String {
    let i = &s.instrument;
    let Some(document) = &i.document else {
        return "NO INSTRUMENT · load an SF2 to play held notes".into();
    };
    let renders = i.renders.lock().map_or(0, |r| r.len());
    let state = if let Some(reason) = &i.unavailable {
        format!("SILENT · {reason}")
    } else if i.mounted.is_some() {
        format!("PLAYING AT {} HZ", rate(&s.output))
    } else {
        "MOUNTING".into()
    };
    format!(
        "{} · PRESET {} · {state} · {} LIVE · {renders} FROZEN",
        document.name,
        document.preset(),
        i.live
    )
}

pub fn header(s: &State) -> El {
    button(
        "instrument-toggle",
        if s.instrument.visible {
            "BACK TO PADS"
        } else {
            "INSTRUMENT"
        },
    )
    .role("switch")
    .checked(s.instrument.visible)
    .help("Load one SF2 instrument preset for held MIDI notes")
    .on_press(|s: &mut State| s.instrument.visible = !s.instrument.visible)
}

pub fn panel(s: &State) -> El {
    let i = &s.instrument;
    let busy = i.busy();
    let status = status(s);
    let mut body = col()
        .key("instrument-panel")
        .role("group")
        .label("Sampled instrument")
        .grow(1.0)
        .min_h(0.0)
        .gap(8.0)
        .padding(12.0)
        .fill(ink("#101b16"))
        .child(caption("SAMPLED INSTRUMENT · ONE SF2 PRESET · RUSTYSYNTH 1.3.8"))
        .child(
            caption(&status)
                .key("instrument-status")
                .role("status")
                .label(&status)
                .wrap(),
        )
        .child(
            text_input(&i.path, TextStyle::new(12.0, 500.0))
                .key("instrument-path")
                .label("Absolute SF2 file path")
                .single_line()
                .h(32.0)
                .px(8.0)
                .fill(ink("#28382f"))
                .disabled(busy)
                .focusable(!busy)
                .on_edit(|s: &mut State| &mut s.instrument.path)
                .on_submit(|s: &mut State, tx: &mut Tx| {
                    if !tx.key_repeat() {
                        load(s);
                    }
                }),
        )
        .child(
            row()
                .gap(8.0)
                .child(
                    button("instrument-load", if busy { "LOADING…" } else { "LOAD SF2" })
                        .disabled(busy)
                        .focusable(!busy)
                        .on_press(load),
                )
                .child(
                    button("instrument-audition", "AUDITION C4")
                        .disabled(i.font.is_none())
                        .focusable(i.font.is_some())
                        .on_press(audition),
                ),
        )
        .child(caption("Held MIDI notes mapped to the instrument play it live and are recorded with their real lengths. Saving freezes each recorded note to audio kept in the project; export uses that audio.").wrap());
    if let Some(notice) = &i.notice {
        body = body.child(
            caption(notice)
                .key("instrument-notice")
                .role("status")
                .wrap(),
        );
    }
    if let Some(error) = &i.error {
        body = body.child(
            caption(error)
                .key("instrument-error")
                .role("alert")
                .label(error)
                .color(ink("#ffad92"))
                .wrap(),
        );
    }
    body
}
