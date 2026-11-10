//! The live CLAP instrument in the app (#112): the machine helper setting,
//! one mounted session, project recall, retained take audio and the panel.
//!
//! Everything here runs on the UI thread. Starting and stopping the helper
//! (and handing the finished [`LiveVoice`] to the output stream) happens off
//! the audio callback; this module never schedules audio itself. It forwards
//! each action to the backend's bounded [`LiveSender`], which names the output
//! frame using the callback's own clock.
//!
//! Identity is the plugin tuple the helper reported (backend, ID, artifact
//! SHA-256, ABI and version). The bundle path is a machine hint only. A take
//! the instrument played into keeps its captured stereo audio as project
//! media (D4); after a save and reopen that audio plays even when the bundle
//! is gone. A device or rate change rebuilds the host and its voice at the new
//! stream's real rate and buffer (D16).
use super::{State, button, caption, ink};
use brownnote_native::{
    import::MediaPublisher,
    live_audio,
    midi::mapping::ControlIntent,
    paths::Resolver,
    pedals::offline::protocol::PluginInfo,
    pedals::offline::{HelperCommand, Stage},
    performance::{Channel, EventId, Note, NoteOn, Pitch, TakeId, Velocity},
    plugins::{
        helper,
        live::{
            LiveAction, LiveConfig, LiveError, LiveEvent, LiveHost, LiveSender, LiveState,
            LiveStatus, LiveVoice, SendOutcome, identify,
        },
    },
    project::{
        AssetId, AssetRef, LiveInstrumentDocument, LiveTakeDocument, session::LiveTakeSlots,
    },
};
use rune::{El, Key, KeyEvent, TextEdit, TextStyle, Tx, col, row, text_input};
use std::{collections::BTreeMap, path::PathBuf, sync::mpsc, time::Duration};

#[cfg(test)]
#[path = "live/tests.rs"]
mod tests;

const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

struct Session {
    host: LiveHost,
    sender: LiveSender,
    plugin: String,
    rate: u32,
}

struct Chosen {
    info: PluginInfo,
    path: PathBuf,
}

struct Capture {
    take: TakeId,
    anchor_app: u64,
    anchor_frame: u64,
    rate: u32,
}

/// The live-instrument lane state. `session` is the only live producer; the
/// callback's voice is owned by the output stream, not here.
#[derive(Default)]
pub struct Live {
    pub visible: bool,
    pub path: TextEdit,
    pub plugin_id: TextEdit,
    /// The project's saved live instrument and every retained take clip.
    pub document: Option<LiveInstrumentDocument>,
    /// Retained take audio already loaded into mixer slots by a project open.
    pub takes: BTreeMap<TakeId, RetainedTake>,
    pub status: Option<LiveStatus>,
    pub notice: Option<String>,
    pub error: Option<String>,
    /// Why the saved instrument does not run here (missing bundle, changed
    /// artifact, a helper fault, or a deliberate clear). Its identity stays.
    pub disabled_reason: Option<String>,
    root: Option<PathBuf>,
    pub helper: Option<HelperCommand>,
    helper_error: Option<String>,
    session: Option<Session>,
    loading: Option<mpsc::Receiver<Result<Chosen, String>>>,
    capture: Option<Capture>,
    /// The take whose live capture was dropped before it committed (the
    /// instrument stopped, CLEAR INSTRUMENT, or an output device change).
    /// The commit still reports that take's audio was not retained.
    abandoned_capture: Option<TakeId>,
}

/// One retained take's decoded stereo audio, loaded as two project-owned slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetainedTake {
    pub asset: AssetId,
    pub left: usize,
    pub right: usize,
    pub rate: u32,
    pub frames: u64,
}

impl From<LiveTakeSlots> for RetainedTake {
    fn from(slots: LiveTakeSlots) -> Self {
        Self {
            asset: slots.asset,
            left: slots.left,
            right: slots.right,
            rate: slots.rate,
            frames: slots.frames,
        }
    }
}

impl Live {
    /// Safe for headless/test constructors: no settings file, no helper.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Only interactive startup calls this, with the machine preferences root.
    /// A missing optional helper is reported when a session is mounted, not as
    /// a startup error.
    pub fn load(root: Result<PathBuf, String>) -> Self {
        let mut live = Self::disabled();
        match root {
            Ok(root) => live.root = Some(root),
            Err(error) => live.helper_error = Some(error),
        }
        live
    }

    /// The machine-level helper program: saved preference, environment
    /// override, then the bounded executable-relative layout (D8).
    pub fn helper_command(&mut self) -> Result<HelperCommand, String> {
        if let Some(helper) = &self.helper {
            return Ok(helper.clone());
        }
        let paths = Resolver::current().map_err(|error| error.to_string())?;
        let helper = helper::resolve(&paths, self.root.as_deref())?;
        self.helper = Some(helper.clone());
        Ok(helper)
    }

    /// Replace the resolved helper (the preference editor's in-memory value).
    #[cfg(test)]
    pub fn set_helper(&mut self, helper: HelperCommand) {
        self.helper = Some(helper);
        self.helper_error = None;
    }

    #[cfg(test)]
    pub fn mounted(&self) -> bool {
        self.session.is_some()
    }

    pub fn busy(&self) -> bool {
        self.loading.is_some()
    }

    pub fn wants_wake(&self) -> bool {
        self.loading.is_some()
    }

    #[cfg(test)]
    pub fn status(&self) -> Option<LiveStatus> {
        self.session.as_ref().map(|session| session.host.status())
    }

    /// The project record to save, with the host's current opaque state and
    /// parameters folded in. A running host that cannot answer blocks the save
    /// rather than writing stale state.
    pub fn for_save(&self) -> Result<Option<LiveInstrumentDocument>, String> {
        let Some(document) = self.document.clone() else {
            return Ok(None);
        };
        let Some(session) = &self.session else {
            return Ok(Some(document));
        };
        let stage = session
            .host
            .save_state()
            .map_err(|error| format!("Live instrument state was not saved: {error}"))?;
        let mut saved =
            LiveInstrumentDocument::from_stage(&stage, document.name.clone(), document.enabled);
        saved.takes = document.takes;
        Ok(Some(saved))
    }

    /// Stop the helper before a new output stream is opened, keeping the
    /// current opaque state so the rebuilt host recalls it. D16.
    pub fn suspend(&mut self) {
        if let Some(session) = self.session.take()
            && let Ok(stage) = session.host.save_state()
        {
            let previous = self.document.take();
            let enabled = previous.as_ref().is_none_or(|doc| doc.enabled);
            let mut document = LiveInstrumentDocument::from_stage(
                &stage,
                previous
                    .as_ref()
                    .map_or_else(|| session.plugin.clone(), |doc| doc.name.clone()),
                enabled,
            );
            document.takes = previous.map(|doc| doc.takes).unwrap_or_default();
            self.document = Some(document);
        }
        self.status = None;
    }

    /// The default output-open path mounts the current document here at the
    /// stream's real rate and buffer, returning the voice for the callback.
    pub fn mount_current(&mut self, rate: u32, buffer_frames: u32) -> Option<LiveVoice> {
        let document = self.document.clone()?;
        self.stop_session(false);
        if !document.enabled {
            self.disabled_reason = Some("the project disabled this instrument".into());
            return None;
        }
        let location = document
            .location_hint
            .as_deref()
            .filter(|hint| !hint.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let typed = PathBuf::from(self.path.text.trim());
                typed.is_absolute().then_some(typed)
            });
        let Some(location) = location else {
            self.disabled_reason = Some(format!(
                "{} · no saved bundle path; choose the recorded bundle here",
                document.identity_line()
            ));
            return None;
        };
        self.start(document.to_stage(location), rate, buffer_frames)
    }

    fn start(&mut self, stage: Stage, rate: u32, buffer_frames: u32) -> Option<LiveVoice> {
        let helper = match self.helper_command() {
            Ok(helper) => helper,
            Err(error) => {
                self.disabled_reason = Some(error.clone());
                self.error = Some(error);
                return None;
            }
        };
        let config = LiveConfig::for_device(helper, rate, buffer_frames);
        match LiveHost::start(stage, config) {
            Ok((host, sender, voice)) => {
                self.disabled_reason = None;
                self.error = None;
                self.status = None;
                self.session = Some(Session {
                    plugin: host.info().plugin.id.clone(),
                    host,
                    sender,
                    rate,
                });
                Some(voice)
            }
            Err(error) => {
                self.disabled_reason = Some(self.describe_failure(&error));
                self.error = Some(error.to_string());
                self.status = None;
                self.session = None;
                None
            }
        }
    }

    fn describe_failure(&self, error: &LiveError) -> String {
        match &self.document {
            Some(document) => format!("{} · {error}", document.identity_line()),
            None => error.to_string(),
        }
    }

    /// The reason a saved instrument is silent, for the output status line.
    pub fn status_note(&self) -> Option<String> {
        self.disabled_reason.clone()
    }

    /// Stop the helper. Dropping the host asks the worker to close it; the
    /// output stream's voice then plays silence.
    fn stop_session(&mut self, notice: bool) {
        if let Some(session) = self.session.take() {
            if notice {
                self.notice = Some(format!("Live instrument {} stopped", session.plugin));
            }
            drop(session);
        }
        self.status = None;
        if let Some(capture) = self.capture.take() {
            // Keep the fact for the commit: the pass still ends, and its take
            // must say the live audio was not retained.
            self.abandoned_capture = Some(capture.take);
        }
    }

    /// Start a stage at an explicit rate and buffer. Tests and non-device
    /// sessions use this; a real output open calls `mount_current`.
    #[cfg(test)]
    pub fn mount(&mut self, stage: Stage, rate: u32, buffer_frames: u32) -> Option<LiveVoice> {
        self.start(stage, rate, buffer_frames)
    }

    /// Choose a plugin by typed path and ID. Identity lookup runs off the UI
    /// thread; `poll` adopts the result.
    pub fn choose(&mut self) {
        if self.loading.is_some() {
            return;
        }
        let path = PathBuf::from(self.path.text.trim());
        let id = self.plugin_id.text.trim().to_string();
        if !path.is_absolute() {
            self.error = Some("Enter the absolute bundle path recorded for the plugin.".into());
            return;
        }
        if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
            self.error = Some("Enter the plugin ID reported by the bundle.".into());
            return;
        }
        let helper = match self.helper_command() {
            Ok(helper) => helper,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let rate = 44_100;
        let (send, receive) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("brownnote-live-identify".into())
            .spawn(move || {
                let result = identify(&helper, &id, &path, rate, OPEN_TIMEOUT)
                    .map(|info| Chosen { info, path })
                    .map_err(|error| error.to_string());
                let _ = send.send(result);
            });
        match spawned {
            Ok(_) => {
                self.loading = Some(receive);
                self.error = None;
                self.notice = Some("Reading the plugin identity off the UI thread…".into());
            }
            Err(error) => self.error = Some(format!("Cannot start plugin lookup: {error}")),
        }
    }

    fn adopt(&mut self, chosen: Chosen, rate: u32) -> Result<(), String> {
        let Chosen { info, path } = chosen;
        if let Some(document) = &self.document
            && !document.takes.is_empty()
            && document.plugin != info.plugin
        {
            return Err(format!(
                "This project retains takes played by {}. Choose that identity or start a new project.",
                document.identity_line()
            ));
        }
        let plugin_id = info.plugin.id.clone();
        let endpoint = Stage {
            plugin: info.plugin,
            location: path.clone(),
            params: Vec::new(),
            state: None,
        };
        let mut document = LiveInstrumentDocument::from_stage(&endpoint, info.name.clone(), true);
        if let Some(previous) = &self.document {
            document.takes = previous.takes.clone();
        }
        self.path = TextEdit::new(path.to_string_lossy());
        self.plugin_id = TextEdit::new(document.plugin.id.clone());
        self.document = Some(document);
        self.disabled_reason = None;
        self.error = None;
        self.notice = Some(format!(
            "Live instrument {plugin_id}{} ready at {rate} Hz; {} preset parameter(s)",
            if info.name.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", info.name)
            },
            info.params.len()
        ));
        self.start_from_document(rate);
        Ok(())
    }

    /// Start at `rate` without owning a callback voice, for headless/no-audio
    /// sessions and tests. A real output open calls `mount_current` instead.
    fn start_from_document(&mut self, rate: u32) {
        if let Some(stage) = self.stage_from_document() {
            let _ = self.start(stage, rate, 512);
        }
    }

    fn stage_from_document(&self) -> Option<Stage> {
        let document = self.document.as_ref()?;
        if !document.enabled {
            return None;
        }
        let location = document
            .location_hint
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| {
                let typed = PathBuf::from(self.path.text.trim());
                typed.is_absolute().then_some(typed)
            })?;
        Some(document.to_stage(location))
    }

    fn stop(&mut self) {
        self.stop_session(false);
        if let Some(document) = &mut self.document {
            document.enabled = false;
        }
        self.disabled_reason = Some(match &self.document {
            Some(document) => format!(
                "{} · cleared in this project; retained takes still play",
                document.identity_line()
            ),
            None => "no live instrument".into(),
        });
        self.notice = Some("Live instrument stopped and cleared for this project".into());
    }

    /// Project open: replace the document and drop the old session. The
    /// output-open path mounts the new one; a failure leaves the project open.
    pub fn install_project(
        &mut self,
        document: Option<LiveInstrumentDocument>,
        takes: BTreeMap<TakeId, LiveTakeSlots>,
    ) {
        self.stop_session(false);
        // The project's pass is gone with the old document; only a pass that
        // still commits reports its lost audio.
        self.abandoned_capture = None;
        self.takes = takes
            .into_iter()
            .map(|(id, slots)| (id, slots.into()))
            .collect();
        self.document = document;
        self.disabled_reason = None;
        self.status = None;
        self.error = None;
        self.notice = None;
        if let Some(document) = &self.document {
            self.path = TextEdit::new(document.location_hint.clone().unwrap_or_default());
            self.plugin_id = TextEdit::new(document.plugin.id.clone());
        } else {
            // A project without a live instrument must not inherit the
            // previous project's typed path or ID.
            self.path = TextEdit::default();
            self.plugin_id = TextEdit::default();
        }
    }

    pub fn restore_document(&mut self, document: Option<LiveInstrumentDocument>) {
        self.stop_session(false);
        self.document = document;
        self.takes.clear();
        self.disabled_reason = None;
    }

    fn send(&mut self, action: LiveAction) -> Option<SendOutcome> {
        let session = self.session.as_mut()?;
        let frame = session.sender.earliest_exact();
        let outcome = session.sender.try_send(LiveEvent { frame, action });
        if let SendOutcome::Full { release_recovery } = outcome {
            self.notice = Some(if release_recovery {
                "Live event queue full; the instrument was asked to recover held notes".into()
            } else {
                "Live event queue full; a note may be late".into()
            });
        }
        Some(outcome)
    }

    pub fn note_on(
        &mut self,
        id: EventId,
        channel: Channel,
        pitch: Pitch,
        velocity: Velocity,
    ) -> Option<SendOutcome> {
        self.send(LiveAction::NoteOn {
            id,
            channel,
            pitch,
            velocity,
        })
    }

    pub fn note_off(&mut self, id: EventId) -> Option<SendOutcome> {
        self.send(LiveAction::NoteOff { id })
    }

    pub fn sustain(&mut self, channel: Channel, down: bool) -> Option<SendOutcome> {
        self.send(LiveAction::Sustain { channel, down })
    }

    pub fn panic(&mut self) -> Option<SendOutcome> {
        self.send(LiveAction::Panic)
    }

    /// Start retaining the audio the callback plays for a recording pass.
    fn begin_capture(&mut self, take: TakeId, _pass_start: u64, now: u64) {
        let Some(session) = &self.session else {
            return;
        };
        if self.capture.is_some() {
            return;
        }
        match session.host.capture(true) {
            Ok(_) => {
                self.abandoned_capture = None;
                self.capture = Some(Capture {
                    take,
                    anchor_app: now,
                    anchor_frame: session.sender.frame(),
                    rate: session.rate,
                });
            }
            Err(error) => self.error = Some(format!("Take audio capture did not start: {error}")),
        }
    }

    fn cancel_capture(&mut self, take: TakeId) {
        if self.abandoned_capture == Some(take) {
            self.abandoned_capture = None;
        }
        if self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.take == take)
        {
            self.capture = None;
            if let Some(session) = &self.session {
                let _ = session.host.capture(false);
            }
        }
    }

    /// Retain the pass window as project media (D4). The clip is the live
    /// instrument's own output, trimmed to the recording pass, never a render.
    fn finish_capture(
        &mut self,
        media: &dyn MediaPublisher,
        take: TakeId,
        pass_start: u64,
        pass_end: u64,
    ) {
        let Some(capture) = self.capture.take() else {
            // The capture was dropped before the take committed: the
            // instrument stopped during the pass (CLEAR INSTRUMENT, or an
            // output device change that remounts it). Say so; the take is
            // already committed with its MIDI notes.
            let stopped = self.abandoned_capture == Some(take);
            if stopped {
                self.abandoned_capture = None;
            }
            self.notice = Some(format!(
                "Take {} committed; its live audio was not retained: {}",
                take.0,
                if stopped {
                    "the instrument stopped during the pass (CLEAR INSTRUMENT or an output device change)"
                } else {
                    "no capture was running for it"
                }
            ));
            return;
        };
        if capture.take != take {
            self.notice = Some(format!(
                "Take {} committed; its live audio was not retained: the capture belonged to take {}",
                take.0, capture.take.0
            ));
            return;
        }
        let Some(session) = &self.session else {
            self.notice = Some(format!(
                "Take {} committed; its live audio was not retained: the instrument stopped before the pass ended",
                take.0
            ));
            return;
        };
        let captured = match session.host.capture(false) {
            Ok(Some(captured)) => captured,
            Ok(None) => {
                self.notice = Some(format!(
                    "Take {} committed; its live audio was not retained: the instrument kept no capture for the pass",
                    take.0
                ));
                return;
            }
            Err(error) => {
                self.error = Some(format!("Take audio was not retained: {error}"));
                return;
            }
        };
        let start = frame_at(
            capture.anchor_app,
            capture.anchor_frame,
            capture.rate,
            pass_start,
        )
        .saturating_sub(captured.start_frame);
        let end = frame_at(
            capture.anchor_app,
            capture.anchor_frame,
            capture.rate,
            pass_end,
        )
        .saturating_sub(captured.start_frame);
        let frames = captured.samples.len() / 2;
        let start = (start as usize).min(frames);
        let end = (end as usize).min(frames);
        if end <= start {
            self.notice = Some("No live audio fell inside the recorded pass".into());
            return;
        }
        let interleaved = &captured.samples[start * 2..end * 2];
        let bytes = match live_audio::encode(captured.rate, interleaved) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.error = Some(format!("Take audio was not encoded: {error}"));
                return;
            }
        };
        let reference = AssetRef::of(&bytes);
        if media.publish(&reference, &bytes).is_err() {
            self.error = Some("Take audio could not be published to project media".into());
            return;
        }
        let Some(document) = &mut self.document else {
            return;
        };
        let record = LiveTakeDocument {
            take: take.0.to_string(),
            asset: reference.id,
            rate: captured.rate,
            frames: (end - start) as u64,
        };
        document.takes.retain(|saved| saved.take != record.take);
        document.takes.push(record);
        self.notice = Some(format!(
            "Retained {:.2} s of take {} audio as project media",
            (end - start) as f64 / f64::from(captured.rate),
            take.0
        ));
        self.error = None;
    }

    /// Refreshes the observable status. Returns whether what the view shows
    /// changed; a disabled instrument keeps the fault text visible.
    fn poll_status(&mut self) -> bool {
        let mut changed = false;
        let status = self.session.as_ref().map(|session| session.host.status());
        if status != self.status {
            if let Some(status) = &status
                && status.state == LiveState::Disabled
            {
                let fault = status
                    .fault
                    .clone()
                    .unwrap_or_else(|| "the helper was disabled".into());
                self.disabled_reason = Some(match &self.document {
                    Some(document) => format!("{} · {fault}", document.identity_line()),
                    None => fault,
                });
            }
            self.status = status;
            changed = true;
        }
        changed
    }

    /// A finished identity lookup, if one is ready.
    fn take_chosen(&mut self) -> Option<Result<Chosen, String>> {
        let receive = self.loading.as_ref()?;
        match receive.try_recv() {
            Ok(result) => {
                self.loading = None;
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading = None;
                Some(Err("Plugin identity lookup stopped.".into()))
            }
        }
    }

    /// One-line status for the panel, matching the MIDI lane's style.
    pub fn status_line(&self) -> String {
        let identity = self
            .document
            .as_ref()
            .map(LiveInstrumentDocument::identity_line);
        let Some(identity) = identity else {
            return "LIVE · no instrument in this project".into();
        };
        if let Some(status) = &self.status {
            let plugin = self
                .session
                .as_ref()
                .map_or("live instrument", |session| session.plugin.as_str());
            let mut line = format!(
                "LIVE · {plugin} · {:?} · {} voice(s) · underruns {} frame(s)",
                status.state, status.voices, status.counters.underrun_frames
            );
            if let Some(fault) = &status.fault {
                line.push_str(" · ");
                line.push_str(fault);
            }
            return line;
        }
        let reason = self
            .disabled_reason
            .clone()
            .unwrap_or_else(|| "not started on this output".into());
        format!("LIVE · {identity} · DISABLED · {reason}")
    }
}

fn frame_at(anchor_app: u64, anchor_frame: u64, rate: u32, app: u64) -> u64 {
    let nanos = u128::from(rate);
    if app >= anchor_app {
        let delta = u128::from(app - anchor_app) * nanos / 1_000_000_000;
        anchor_frame.saturating_add(u64::try_from(delta).unwrap_or(u64::MAX))
    } else {
        let delta = u128::from(anchor_app - app) * nanos / 1_000_000_000;
        anchor_frame.saturating_sub(u64::try_from(delta).unwrap_or(u64::MAX))
    }
}

/// Dispatch helpers called by the MIDI input sink (the existing input path),
/// so there is exactly one scheduler: the backend's [`LiveSender`].
pub fn poll(s: &mut State) -> bool {
    let mut changed = s.live.poll_status();
    if let Some(result) = s.live.take_chosen() {
        let rate = s
            .output
            .opened()
            .map_or(44_100, |opened| opened.configuration.stream.sample_rate);
        match result.and_then(|chosen| s.live.adopt(chosen, rate)) {
            Ok(()) => {
                if s.devices.enabled() {
                    super::devices::reopen(s);
                }
            }
            Err(error) => {
                s.live.error = Some(error);
                s.live.notice = None;
            }
        }
        changed = true;
    }
    changed
}

pub fn wants_wake(s: &State) -> bool {
    s.live.wants_wake()
}

pub fn held_start(s: &mut State, on: &NoteOn) {
    let _ = s
        .live
        .note_on(on.id, on.key.channel, on.key.pitch, on.velocity);
}

pub fn held_end(s: &mut State, note: &Note) {
    let _ = s.live.note_off(note.on().id);
}

pub fn control(s: &mut State, channel: Channel, action: ControlIntent) {
    match action {
        ControlIntent::Sustain(down) => {
            let _ = s.live.sustain(channel, down);
        }
        ControlIntent::Panic => {
            let _ = s.live.panic();
        }
        // The controller ends released notes through `HeldEnd`.
        ControlIntent::AllNotesOff => {}
    }
}

pub fn panic(s: &mut State) {
    let _ = s.live.panic();
}

/// Start retaining the live output for a recording pass that just started.
pub fn begin_take_capture(s: &mut State, take: TakeId, pass_start: u64) {
    let now = super::transport::app_nanos(s);
    s.live.begin_capture(take, pass_start, now);
}

/// Keep the pass's live audio as project media once the take commits.
pub fn finish_take_capture(s: &mut State, take: TakeId, pass_start: u64, pass_end: u64) {
    let media = s.media.owned.clone();
    s.live
        .finish_capture(media.as_ref(), take, pass_start, pass_end);
}

/// A discarded pass leaves no retained audio.
pub fn cancel_take_capture(s: &mut State, take: TakeId) {
    s.live.cancel_capture(take);
}

pub fn choose(s: &mut State) {
    s.live.choose();
}

pub fn clear(s: &mut State) {
    s.live.stop();
}

/// Call before pad shortcuts: typing a path must not play pads.
pub fn on_key(s: &mut State, event: &KeyEvent, _tx: &mut Tx) -> bool {
    if !s.live.visible {
        return false;
    }
    if event.key == Key::Escape {
        s.live.visible = false;
        return true;
    }
    matches!(&event.key, Key::Text(_))
        && !event.mods.cmd
        && !event.mods.ctrl
        && !event.mods.alt
        && !event.mods.logo
}

pub fn header(s: &State) -> El {
    button(
        "live-toggle",
        if s.live.visible {
            "BACK TO PADS"
        } else {
            "LIVE"
        },
    )
    .role("switch")
    .checked(s.live.visible)
    .help("One live CLAP instrument recalled by the project")
    .on_press(|s: &mut State| s.live.visible = !s.live.visible)
}

pub fn panel(s: &State) -> El {
    let live = &s.live;
    let busy = live.busy();
    let status = live.status_line();
    let mut body = col()
        .key("live-panel")
        .role("group")
        .label("Live CLAP instrument")
        .grow(1.0)
        .min_h(0.0)
        .gap(8.0)
        .padding(12.0)
        .fill(ink("#101b16"))
        .child(caption("LIVE CLAP INSTRUMENT · ONE PINNED BUNDLE · PROJECT IDENTITY"))
        .child(
            caption(&status)
                .key("live-status")
                .role("status")
                .label(&status)
                .wrap(),
        )
        .child(
            text_input(&live.path, TextStyle::new(12.0, 500.0))
                .key("live-path")
                .label("Absolute plugin bundle path (machine hint)")
                .single_line()
                .h(32.0)
                .px(8.0)
                .fill(ink("#28382f"))
                .disabled(busy)
                .focusable(!busy)
                .on_edit(|s: &mut State| &mut s.live.path)
                .on_submit(|s: &mut State, tx: &mut Tx| {
                    if !tx.key_repeat() {
                        choose(s);
                    }
                }),
        )
        .child(
            text_input(&live.plugin_id, TextStyle::new(12.0, 500.0))
                .key("live-plugin-id")
                .label("Plugin ID reported by the bundle")
                .single_line()
                .h(32.0)
                .px(8.0)
                .fill(ink("#28382f"))
                .disabled(busy)
                .focusable(!busy)
                .on_edit(|s: &mut State| &mut s.live.plugin_id)
                .on_submit(|s: &mut State, tx: &mut Tx| {
                    if !tx.key_repeat() {
                        choose(s);
                    }
                }),
        )
        .child(
            row()
                .gap(8.0)
                .child(
                    button("live-choose", if busy { "READING…" } else { "CHOOSE PLUGIN" })
                        .disabled(busy)
                        .focusable(!busy)
                        .on_press(choose),
                )
                .child(
                    button("live-clear", "CLEAR INSTRUMENT")
                        .on_press(clear),
                ),
        )
        .child(caption("The path is a machine hint; backend, plugin ID, artifact hash, ABI and version are identity. A take the live instrument played into keeps its captured stereo audio as project media, so it still plays after the plugin is removed. Installed-plugin discovery is not part of this first scope.").wrap());
    if let Some(notice) = &live.notice {
        body = body.child(caption(notice).key("live-notice").role("status").wrap());
    }
    if let Some(error) = &live.error {
        body = body.child(
            caption(error)
                .key("live-error")
                .role("alert")
                .label(error)
                .color(ink("#ffad92"))
                .wrap(),
        );
    }
    body
}
