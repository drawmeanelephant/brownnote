//! Live CLAP instrument backend: one pinned instrument in a helper process,
//! played from the audio callback without the callback ever waiting for it.
//!
//! Three parties, each with one job:
//!
//! - The **producer** (MIDI input, UI, take replay) sends [`LiveEvent`]s
//!   through [`LiveSender`]: a bounded wait-free ring. Each event names the
//!   absolute output frame it should sound at, in the same frame count the
//!   callback plays ([`LiveSender::frame`]).
//! - The **worker** thread is the only code that talks to the helper. It
//!   renders fixed [`QUANTUM`]-frame blocks ahead of the callback, at most
//!   [`LiveConfig::lead_frames`] ahead, with sample-exact event offsets. It
//!   applies every deadline, restart, non-finite check and state save. It is
//!   an ordinary thread, not a realtime one.
//! - The **callback** holds a [`LiveVoice`]. [`LiveVoice::render_add`] only
//!   pops already-rendered blocks from a preallocated wait-free ring and adds
//!   them to the output. It never blocks, allocates, frees, makes a system
//!   call or touches the helper. A block that is not there in time is silence
//!   and is counted as an underrun; a block that arrives after its time is
//!   dropped and counted as stale, so lateness never accumulates as delay.
//!
//! Note identity follows D14: every note-on carries its own [`EventId`], and a
//! note-off names the exact note-on it ends (the producer's FIFO
//! [`NoteLifecycle`](crate::performance::NoteLifecycle) supplies it). Each
//! sounding note gets its own CLAP note ID. The sustain pedal is applied here,
//! in the host, so it works the same for every plugin.
//!
//! The instrument is described by the shared plugin stage type
//! ([`Stage`]): identity, location, parameters and opaque state. Parameter
//! changes and saved state are folded back into [`LiveHost::stage`], so a
//! project saves the same type the offline renderer uses.
use super::live_protocol::{Helper, HelperFault, NoteEnd, WireEvent};
use crate::pedals::offline::{
    Backend, HelperCommand, MAX_RATE, MAX_STATE_BYTES, MIN_RATE, Param, Plugin, Stage,
    protocol::{HelperError, Open, PluginInfo},
};
use crate::performance::{Channel, EventId, Pitch, Velocity};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

/// Frames per helper block. Fixed, so the callback's block type is fixed-size.
pub const QUANTUM: usize = 64;
/// The worker always renders stereo; the callback folds it to the device.
const CHANNELS: usize = 2;
const BLOCK_SAMPLES: usize = QUANTUM * CHANNELS;
/// Events the producer can have queued for the worker.
pub const EVENT_CAPACITY: usize = 1024;
/// Notes the worker tracks for release, sustain and panic.
pub const MAX_VOICES: usize = 256;
const MAX_EVENTS_PER_BLOCK: usize = 512;
/// How long the worker sleeps when it is ahead of the callback.
const POLL: Duration = Duration::from_micros(500);

/// What a producer asks the instrument to do at a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LiveAction {
    NoteOn {
        id: EventId,
        channel: Channel,
        pitch: Pitch,
        velocity: Velocity,
    },
    /// Ends exactly the note-on with this ID.
    NoteOff {
        id: EventId,
    },
    Sustain {
        channel: Channel,
        down: bool,
    },
    /// A generic CLAP parameter, by ID, in the plugin's own range.
    Param {
        id: u32,
        value: f64,
    },
    /// Release every note and lift every sustain pedal.
    Panic,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveEvent {
    /// Absolute output frame, in [`LiveSender::frame`]'s count.
    pub frame: u64,
    pub action: LiveAction,
}

#[derive(Clone, Debug)]
pub struct LiveConfig {
    pub helper: HelperCommand,
    pub rate: u32,
    /// How far ahead of the callback the worker renders. Events scheduled at
    /// least this far ahead of [`LiveSender::frame`] sound exactly on time;
    /// earlier ones sound at the start of the next unrendered block.
    pub lead_frames: u32,
    pub open_timeout: Duration,
    pub block_timeout: Duration,
    pub close_timeout: Duration,
    /// Crashes, hangs and non-finite blocks restart the helper with the last
    /// saved state this many times; the next fault disables the instrument.
    pub restart_budget: u32,
}

impl LiveConfig {
    /// Lead of two device buffers (at least 256 frames), 5 s to open,
    /// 250 ms per block, two restarts.
    pub fn for_device(helper: HelperCommand, rate: u32, buffer_frames: u32) -> Self {
        Self {
            helper,
            rate,
            lead_frames: buffer_frames.saturating_mul(2).max(256),
            open_timeout: Duration::from_secs(5),
            block_timeout: Duration::from_millis(250),
            close_timeout: Duration::from_secs(2),
            restart_budget: 2,
        }
    }

    fn validate(&self) -> Result<(), LiveError> {
        if !(MIN_RATE..=MAX_RATE).contains(&self.rate) {
            return Err(LiveError::Invalid(format!("sample rate {}", self.rate)));
        }
        let quantum = QUANTUM as u32;
        if self.lead_frames < 2 * quantum
            || self.lead_frames > self.rate
            || !self.lead_frames.is_multiple_of(quantum)
        {
            return Err(LiveError::Invalid(format!(
                "lead of {} frames: it must be a multiple of {quantum}, at least {} and at most one second",
                self.lead_frames,
                2 * quantum
            )));
        }
        if self.open_timeout.is_zero() || self.block_timeout.is_zero() {
            return Err(LiveError::Invalid("zero deadline".into()));
        }
        Ok(())
    }
}

fn validate_stage(stage: &Stage) -> Result<(), LiveError> {
    if stage.plugin.backend != Backend::Clap {
        return Err(LiveError::Invalid(format!(
            "live instruments are CLAP only, not {}",
            stage.plugin.backend
        )));
    }
    if stage.params.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err(LiveError::Invalid(
            "parameters must be strictly ascending by ID".into(),
        ));
    }
    if stage.params.iter().any(|p| !p.value.is_finite()) {
        return Err(LiveError::Invalid("a parameter is not finite".into()));
    }
    if stage
        .state
        .as_ref()
        .is_some_and(|s| s.len() > MAX_STATE_BYTES)
    {
        return Err(LiveError::Invalid("state is too large".into()));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum LiveError {
    Invalid(String),
    /// The helper loaded nothing and said why, with a protocol error code
    /// such as `bundle-missing`, `unknown-plugin-id` or `unsupported`.
    Unavailable(HelperError),
    /// The plugin on disk is not the one the stage names.
    ArtifactChanged {
        expected: Box<Plugin>,
        found: Box<Plugin>,
    },
    Helper(HelperFault),
    /// The instrument was disabled after a fault; see [`LiveStatus::fault`].
    Disabled,
    Stopped,
}

impl fmt::Display for LiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid live instrument: {message}"),
            Self::Unavailable(error) => write!(f, "{}: {}", error.code, error.message),
            Self::ArtifactChanged { expected, found } => write!(
                f,
                "the plugin changed: expected {} {} ({}), found {} {} ({})",
                expected.id,
                expected.version,
                expected.artifact,
                found.id,
                found.version,
                found.artifact
            ),
            Self::Helper(fault) => fault.fmt(f),
            Self::Disabled => f.write_str("the instrument is disabled after a fault"),
            Self::Stopped => f.write_str("the instrument has stopped"),
        }
    }
}

impl std::error::Error for LiveError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveState {
    Running,
    /// The helper faulted and is being restarted; the callback hears silence.
    Restarting,
    Disabled,
    Stopped,
}

impl LiveState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Running,
            1 => Self::Restarting,
            2 => Self::Disabled,
            _ => Self::Stopped,
        }
    }
}

/// Independent atomic observations, not a transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveCounters {
    pub callbacks: u64,
    /// Frames the callback had audio for.
    pub played_frames: u64,
    /// Frames of silence while running, after the first block played.
    pub underrun_frames: u64,
    /// Frames of silence before the first block, or while not running.
    pub idle_frames: u64,
    /// Rendered frames that arrived after their time and were dropped.
    pub stale_frames: u64,
    pub accepted_events: u64,
    pub full_events: u64,
    pub late_events: u64,
    pub late_frames: u64,
    pub unmatched_note_offs: u64,
    pub duplicate_note_ons: u64,
    pub voice_overflows: u64,
    pub note_ends: u64,
    pub panics: u64,
    pub blocks: u64,
    pub skipped_frames: u64,
    pub timeouts: u64,
    pub exits: u64,
    pub nonfinite_blocks: u64,
    pub protocol_faults: u64,
    pub restarts: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveStatus {
    pub state: LiveState,
    /// The most recent fault, as text for the user.
    pub fault: Option<String>,
    /// Notes started and not yet reported ended by the plugin.
    pub voices: usize,
    /// Frames rendered so far (exclusive end).
    pub rendered: u64,
    pub counters: LiveCounters,
}

#[derive(Default)]
struct Shared {
    clock: AtomicU64,
    rendered: AtomicU64,
    state: AtomicU8,
    panic: AtomicBool,
    voices: AtomicUsize,
    callbacks: AtomicU64,
    played_frames: AtomicU64,
    underrun_frames: AtomicU64,
    idle_frames: AtomicU64,
    stale_frames: AtomicU64,
    accepted_events: AtomicU64,
    full_events: AtomicU64,
    late_events: AtomicU64,
    late_frames: AtomicU64,
    unmatched_note_offs: AtomicU64,
    duplicate_note_ons: AtomicU64,
    voice_overflows: AtomicU64,
    note_ends: AtomicU64,
    panics: AtomicU64,
    blocks: AtomicU64,
    skipped_frames: AtomicU64,
    timeouts: AtomicU64,
    exits: AtomicU64,
    nonfinite_blocks: AtomicU64,
    protocol_faults: AtomicU64,
    restarts: AtomicU64,
}

const RELAXED: Ordering = Ordering::Relaxed;

impl Shared {
    fn state(&self) -> LiveState {
        LiveState::from_u8(self.state.load(Ordering::Acquire))
    }
    fn set_state(&self, state: LiveState) {
        self.state.store(state as u8, Ordering::Release);
    }
    fn counters(&self) -> LiveCounters {
        LiveCounters {
            callbacks: self.callbacks.load(RELAXED),
            played_frames: self.played_frames.load(RELAXED),
            underrun_frames: self.underrun_frames.load(RELAXED),
            idle_frames: self.idle_frames.load(RELAXED),
            stale_frames: self.stale_frames.load(RELAXED),
            accepted_events: self.accepted_events.load(RELAXED),
            full_events: self.full_events.load(RELAXED),
            late_events: self.late_events.load(RELAXED),
            late_frames: self.late_frames.load(RELAXED),
            unmatched_note_offs: self.unmatched_note_offs.load(RELAXED),
            duplicate_note_ons: self.duplicate_note_ons.load(RELAXED),
            voice_overflows: self.voice_overflows.load(RELAXED),
            note_ends: self.note_ends.load(RELAXED),
            panics: self.panics.load(RELAXED),
            blocks: self.blocks.load(RELAXED),
            skipped_frames: self.skipped_frames.load(RELAXED),
            timeouts: self.timeouts.load(RELAXED),
            exits: self.exits.load(RELAXED),
            nonfinite_blocks: self.nonfinite_blocks.load(RELAXED),
            protocol_faults: self.protocol_faults.load(RELAXED),
            restarts: self.restarts.load(RELAXED),
        }
    }
}

#[derive(Clone, Copy)]
struct Block {
    time: u64,
    samples: [f32; BLOCK_SAMPLES],
}

const SILENT_BLOCK: Block = Block {
    time: 0,
    samples: [0.0; BLOCK_SAMPLES],
};

/// The audio callback's handle. Everything it needs is allocated before it is
/// handed out.
pub struct LiveVoice {
    audio: rtrb::Consumer<Block>,
    shared: Arc<Shared>,
    block: Block,
    have: bool,
    primed: bool,
    clock: u64,
}

impl LiveVoice {
    /// Adds the instrument's next `out.len() / channels` frames to `out`
    /// (interleaved). Stereo goes to channels 0 and 1; a mono device gets the
    /// average. Missing audio is silence, never a wait.
    pub fn render_add(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let frames = out.len() / channels;
        let start = self.clock;
        let end = start + frames as u64;
        let quantum = QUANTUM as u64;
        let mut pos = start;
        let mut missing = 0;
        let mut stale = 0;
        while pos < end {
            if !self.have {
                match self.audio.pop() {
                    Ok(block) => {
                        self.block = block;
                        self.have = true;
                    }
                    Err(_) => break,
                }
            }
            let block_start = self.block.time;
            let block_end = block_start + quantum;
            if block_end <= pos {
                stale += quantum;
                self.have = false;
                continue;
            }
            if block_start >= end {
                break;
            }
            if block_start > pos {
                missing += block_start - pos;
                pos = block_start;
            }
            let upto = end.min(block_end);
            let from = (pos - block_start) as usize;
            let count = (upto - pos) as usize;
            let offset = (pos - start) as usize;
            let source = &self.block.samples[from * CHANNELS..(from + count) * CHANNELS];
            let target = &mut out[offset * channels..(offset + count) * channels];
            for (frame, sample) in target
                .chunks_exact_mut(channels)
                .zip(source.as_chunks::<CHANNELS>().0)
            {
                if channels == 1 {
                    frame[0] += 0.5 * (sample[0] + sample[1]);
                } else {
                    frame[0] += sample[0];
                    frame[1] += sample[1];
                }
            }
            pos = upto;
            if pos == block_end {
                self.have = false;
            }
        }
        missing += end - pos;
        self.clock = end;
        let s = &*self.shared;
        s.clock.store(end, Ordering::Release);
        s.callbacks.fetch_add(1, RELAXED);
        let played = frames as u64 - missing;
        s.played_frames.fetch_add(played, RELAXED);
        self.primed |= played > 0;
        if missing > 0 {
            if self.primed && s.state() == LiveState::Running {
                s.underrun_frames.fetch_add(missing, RELAXED);
            } else {
                s.idle_frames.fetch_add(missing, RELAXED);
            }
        }
        if stale > 0 {
            s.stale_frames.fetch_add(stale, RELAXED);
        }
    }

    /// Frames played so far: the frame the next call starts at.
    pub fn frame(&self) -> u64 {
        self.clock
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Accepted,
    /// The queue was full. A release that could not be queued also requests a
    /// panic, so a note cannot stick because its note-off was lost.
    Full {
        release_recovery: bool,
    },
    /// The instrument is disabled or stopped.
    Unavailable,
}

/// The producer's handle. One producer thread owns it.
pub struct LiveSender {
    events: rtrb::Producer<LiveEvent>,
    shared: Arc<Shared>,
    lead_frames: u32,
}

impl LiveSender {
    /// The callback's play cursor.
    pub fn frame(&self) -> u64 {
        self.shared.clock.load(Ordering::Acquire)
    }

    /// The earliest frame an event sent now is guaranteed to sound at exactly.
    pub fn earliest_exact(&self) -> u64 {
        self.frame() + u64::from(self.lead_frames)
    }

    pub fn try_send(&mut self, event: LiveEvent) -> SendOutcome {
        let s = &*self.shared;
        if matches!(s.state(), LiveState::Disabled | LiveState::Stopped) {
            return SendOutcome::Unavailable;
        }
        match self.events.push(event) {
            Ok(()) => {
                s.accepted_events.fetch_add(1, RELAXED);
                SendOutcome::Accepted
            }
            Err(_) => {
                s.full_events.fetch_add(1, RELAXED);
                let release = matches!(
                    event.action,
                    LiveAction::NoteOff { .. }
                        | LiveAction::Sustain { down: false, .. }
                        | LiveAction::Panic
                );
                if release {
                    s.panic.store(true, RELAXED);
                }
                SendOutcome::Full {
                    release_recovery: release,
                }
            }
        }
    }
}

/// Audio the worker rendered, frame-aligned to the callback's count. Gaps
/// (a restart, or a block that faulted) are silence.
#[derive(Clone, Debug, PartialEq)]
pub struct Captured {
    pub start_frame: u64,
    pub rate: u32,
    /// Stereo, interleaved.
    pub samples: Vec<f32>,
}

type Reply<T> = mpsc::Sender<Result<T, LiveError>>;

enum Command {
    SaveState(Reply<Stage>),
    Capture(bool, Reply<Option<Captured>>),
    Stop(Reply<Stage>),
}

/// The control handle: status, state save, capture and shutdown. Not for the
/// audio callback.
pub struct LiveHost {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    fault: Arc<Mutex<Option<String>>>,
    info: PluginInfo,
    worker: Option<thread::JoinHandle<()>>,
    close_timeout: Duration,
}

fn open_message(stage: &Stage, rate: u32) -> Open {
    Open {
        protocol: super::live_protocol::VERSION,
        backend: Backend::Clap,
        plugin_id: stage.plugin.id.clone(),
        location: stage.location.to_string_lossy().into_owned(),
        rate,
        channels: CHANNELS as u16,
        max_block_frames: QUANTUM as u32,
        params: stage.params.clone(),
        state: stage.state.is_some(),
    }
}

/// Starts a helper and opens the stage's plugin, checking it is the plugin
/// the stage names. Runs on the calling thread, never the audio callback.
fn open_helper(stage: &Stage, config: &LiveConfig) -> Result<(Helper, PluginInfo), LiveError> {
    let mut helper = Helper::spawn(&config.helper, CHANNELS).map_err(LiveError::Helper)?;
    let info = helper
        .open(
            &open_message(stage, config.rate),
            stage.state.as_deref(),
            config.open_timeout,
        )
        .map_err(LiveError::Helper)?
        .map_err(LiveError::Unavailable)?;
    if info.protocol != super::live_protocol::VERSION {
        return Err(LiveError::Helper(HelperFault::Protocol(format!(
            "live protocol {}",
            info.protocol
        ))));
    }
    if info.plugin != stage.plugin {
        return Err(LiveError::ArtifactChanged {
            expected: Box::new(stage.plugin.clone()),
            found: Box::new(info.plugin),
        });
    }
    Ok((helper, info))
}

/// Loads an instrument once, off the audio thread, to learn its identity and
/// parameters. The returned `plugin` is what a new [`Stage`] names.
pub fn identify(
    helper: &HelperCommand,
    plugin_id: &str,
    location: &std::path::Path,
    rate: u32,
    timeout: Duration,
) -> Result<PluginInfo, LiveError> {
    let mut process = Helper::spawn(helper, CHANNELS).map_err(LiveError::Helper)?;
    let open = Open {
        protocol: super::live_protocol::VERSION,
        backend: Backend::Clap,
        plugin_id: plugin_id.into(),
        location: location.to_string_lossy().into_owned(),
        rate,
        channels: CHANNELS as u16,
        max_block_frames: QUANTUM as u32,
        params: Vec::new(),
        state: false,
    };
    let info = process
        .open(&open, None, timeout)
        .map_err(LiveError::Helper)?
        .map_err(LiveError::Unavailable)?;
    let _ = process.end(timeout);
    Ok(info)
}

impl LiveHost {
    /// Prepares everything off the audio thread: starts the helper, loads and
    /// activates the plugin, allocates both rings, and starts the worker,
    /// which renders the first lead before this returns its handles.
    pub fn start(
        stage: Stage,
        config: LiveConfig,
    ) -> Result<(LiveHost, LiveSender, LiveVoice), LiveError> {
        config.validate()?;
        validate_stage(&stage)?;
        let (helper, info) = open_helper(&stage, &config)?;
        let blocks = config.lead_frames as usize / QUANTUM + 4;
        let (audio_in, audio_out) = rtrb::RingBuffer::new(blocks);
        let (events_in, events_out) = rtrb::RingBuffer::new(EVENT_CAPACITY);
        let (commands, receiver) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        shared.set_state(LiveState::Running);
        let fault = Arc::new(Mutex::new(None));
        let params = stage.params.iter().map(|p| (p.id, p.value)).collect();
        let mut worker = Worker {
            stage,
            params,
            helper: Some(helper),
            shared: shared.clone(),
            fault: fault.clone(),
            events: events_out,
            audio: audio_in,
            commands: receiver,
            pending: Vec::with_capacity(EVENT_CAPACITY),
            voices: Vec::with_capacity(MAX_VOICES),
            sustain: [false; 16],
            next_note_id: 0,
            wire: Vec::with_capacity(MAX_EVENTS_PER_BLOCK + MAX_VOICES),
            rendered: 0,
            restarts: 0,
            capture: None,
            close_timeout: config.close_timeout,
            config,
        };
        let close_timeout = worker.config.close_timeout;
        let lead_frames = worker.config.lead_frames;
        // Fill the lead before the callback can ask for audio.
        worker.render_ahead();
        let handle = thread::Builder::new()
            .name("live-instrument".into())
            .spawn(move || worker.run())
            .map_err(|error| LiveError::Helper(HelperFault::Spawn(error.to_string())))?;
        Ok((
            LiveHost {
                commands,
                shared: shared.clone(),
                fault,
                info,
                worker: Some(handle),
                close_timeout,
            },
            LiveSender {
                events: events_in,
                shared: shared.clone(),
                lead_frames,
            },
            LiveVoice {
                audio: audio_out,
                shared,
                block: SILENT_BLOCK,
                have: false,
                primed: false,
                clock: 0,
            },
        ))
    }

    /// What the helper reported when the plugin was first opened.
    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    pub fn status(&self) -> LiveStatus {
        LiveStatus {
            state: self.shared.state(),
            fault: self.fault.lock().ok().and_then(|f| f.clone()),
            voices: self.shared.voices.load(RELAXED),
            rendered: self.shared.rendered.load(Ordering::Acquire),
            counters: self.shared.counters(),
        }
    }

    fn ask<T>(
        &self,
        command: Command,
        receive: mpsc::Receiver<Result<T, LiveError>>,
    ) -> Result<T, LiveError> {
        self.commands
            .send(command)
            .map_err(|_| LiveError::Stopped)?;
        // The worker answers between blocks; a block can take its deadline,
        // and a fault can add a restart.
        receive
            .recv_timeout(self.close_timeout * 4)
            .map_err(|_| LiveError::Helper(HelperFault::Timeout))?
    }

    /// Saves the plugin's state now, between blocks, and returns the stage
    /// with that state and the current parameters. Later restarts restore it.
    pub fn save_state(&self) -> Result<Stage, LiveError> {
        let (reply, receive) = mpsc::channel();
        self.ask(Command::SaveState(reply), receive)
    }

    /// Starts keeping every rendered block (`true`), or stops and returns
    /// what was kept (`false`).
    pub fn capture(&self, on: bool) -> Result<Option<Captured>, LiveError> {
        let (reply, receive) = mpsc::channel();
        self.ask(Command::Capture(on, reply), receive)
    }

    /// Ends the session: saves the final state and closes the helper. The
    /// returned stage is what the project keeps.
    pub fn stop(mut self) -> Result<Stage, LiveError> {
        let (reply, receive) = mpsc::channel();
        let result = self.ask(Command::Stop(reply), receive);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        result
    }
}

impl Drop for LiveHost {
    fn drop(&mut self) {
        // Dropping the command sender tells the worker to kill the helper and
        // exit. It is not joined here: it may be inside a block deadline.
        self.shared.set_state(LiveState::Stopped);
    }
}

struct Voice {
    id: EventId,
    note_id: i32,
    channel: u8,
    key: u8,
    released: bool,
    sustained: bool,
}

struct Capture {
    start: Option<u64>,
    samples: Vec<f32>,
}

struct Worker {
    config: LiveConfig,
    stage: Stage,
    params: BTreeMap<u32, f64>,
    helper: Option<Helper>,
    shared: Arc<Shared>,
    fault: Arc<Mutex<Option<String>>>,
    events: rtrb::Consumer<LiveEvent>,
    audio: rtrb::Producer<Block>,
    commands: mpsc::Receiver<Command>,
    pending: Vec<LiveEvent>,
    voices: Vec<Voice>,
    sustain: [bool; 16],
    next_note_id: i32,
    wire: Vec<WireEvent>,
    rendered: u64,
    restarts: u32,
    capture: Option<Capture>,
    close_timeout: Duration,
}

enum Fault {
    Helper(HelperFault),
    Error(HelperError),
    NonFinite,
}

impl Worker {
    fn run(mut self) {
        loop {
            loop {
                match self.commands.try_recv() {
                    Ok(Command::SaveState(reply)) => {
                        let _ = reply.send(self.save_state());
                    }
                    Ok(Command::Capture(on, reply)) => {
                        let _ = reply.send(Ok(self.set_capture(on)));
                    }
                    Ok(Command::Stop(reply)) => {
                        let _ = reply.send(self.stop());
                        return;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.helper = None;
                        self.shared.set_state(LiveState::Stopped);
                        return;
                    }
                }
            }
            if !self.render_ahead() {
                thread::park_timeout(POLL);
            }
        }
    }

    fn take_events(&mut self) {
        let before = self.pending.len();
        while let Ok(event) = self.events.pop() {
            self.pending.push(event);
        }
        if self.pending.len() != before {
            self.pending.sort_by_key(|event| event.frame);
        }
    }

    /// Renders until the lead is full or the audio ring is. Returns whether
    /// anything was rendered.
    fn render_ahead(&mut self) -> bool {
        self.take_events();
        if self.shared.state() != LiveState::Running {
            self.pending.clear();
            return false;
        }
        let clock = self.shared.clock.load(Ordering::Acquire);
        if self.rendered < clock {
            // The callback is past us: anything rendered for those frames
            // would only be dropped as stale.
            self.shared
                .skipped_frames
                .fetch_add(clock - self.rendered, RELAXED);
            self.rendered = clock;
        }
        let quantum = QUANTUM as u64;
        let horizon = clock + u64::from(self.config.lead_frames);
        let mut any = false;
        while self.rendered + quantum <= horizon && self.audio.slots() > 0 {
            if self.shared.state() != LiveState::Running {
                break;
            }
            self.render_block();
            any = true;
        }
        any
    }

    fn render_block(&mut self) {
        let start = self.rendered;
        let end = start + QUANTUM as u64;
        self.wire.clear();
        if self.shared.panic.swap(false, RELAXED) {
            self.panic(0);
        }
        let mut taken = 0;
        for index in 0..self.pending.len() {
            let event = self.pending[index];
            if event.frame >= end || self.wire.len() >= MAX_EVENTS_PER_BLOCK {
                break;
            }
            taken += 1;
            let offset = if event.frame < start {
                self.shared.late_events.fetch_add(1, RELAXED);
                self.shared
                    .late_frames
                    .fetch_add(start - event.frame, RELAXED);
                0
            } else {
                (event.frame - start) as u32
            };
            self.apply(event.action, offset);
        }
        self.pending.drain(..taken);
        let Some(helper) = self.helper.as_mut() else {
            return;
        };
        match helper.process(QUANTUM as u32, &self.wire, self.config.block_timeout) {
            Ok(Ok((samples, ends))) => {
                if samples.iter().any(|s| !s.is_finite()) {
                    self.fault(Fault::NonFinite);
                    return;
                }
                self.note_ends(&ends);
                let mut block = Block {
                    time: start,
                    samples: [0.0; BLOCK_SAMPLES],
                };
                block.samples.copy_from_slice(&samples);
                if let Some(capture) = &mut self.capture {
                    let first = *capture.start.get_or_insert(start);
                    let expected = first + (capture.samples.len() / CHANNELS) as u64;
                    let gap = start.saturating_sub(expected) as usize;
                    capture
                        .samples
                        .resize(capture.samples.len() + gap * CHANNELS, 0.0);
                    capture.samples.extend_from_slice(&samples);
                }
                // `render_ahead` checked for a free slot.
                let _ = self.audio.push(block);
                self.rendered = end;
                self.shared.rendered.store(end, Ordering::Release);
                self.shared.blocks.fetch_add(1, RELAXED);
            }
            Ok(Err(error)) => self.fault(Fault::Error(error)),
            Err(fault) => self.fault(Fault::Helper(fault)),
        }
    }

    fn apply(&mut self, action: LiveAction, time: u32) {
        match action {
            LiveAction::NoteOn {
                id,
                channel,
                pitch,
                velocity,
            } => {
                if self.voices.iter().any(|v| v.id == id) {
                    self.shared.duplicate_note_ons.fetch_add(1, RELAXED);
                    return;
                }
                if self.voices.len() >= MAX_VOICES {
                    // Forget the oldest released note first; the plugin still
                    // owns its tail.
                    match self.voices.iter().position(|v| v.released) {
                        Some(index) => {
                            self.voices.remove(index);
                        }
                        None => {
                            self.shared.voice_overflows.fetch_add(1, RELAXED);
                            return;
                        }
                    }
                }
                let note_id = self.next_note_id;
                self.next_note_id = self.next_note_id.checked_add(1).unwrap_or(0);
                self.voices.push(Voice {
                    id,
                    note_id,
                    channel: channel.value(),
                    key: pitch.value(),
                    released: false,
                    sustained: false,
                });
                self.wire.push(WireEvent::NoteOn {
                    time,
                    note_id,
                    channel: channel.value(),
                    key: pitch.value(),
                    velocity: f64::from(velocity.value()) / 127.0,
                });
            }
            LiveAction::NoteOff { id } => {
                let Some(index) = self.voices.iter().position(|v| v.id == id && !v.released) else {
                    self.shared.unmatched_note_offs.fetch_add(1, RELAXED);
                    return;
                };
                if self.sustain[usize::from(self.voices[index].channel)] {
                    self.voices[index].sustained = true;
                } else {
                    self.release(index, time);
                }
            }
            LiveAction::Sustain { channel, down } => {
                let channel = channel.value();
                self.sustain[usize::from(channel)] = down;
                if !down {
                    for index in 0..self.voices.len() {
                        let v = &self.voices[index];
                        if v.channel == channel && v.sustained && !v.released {
                            self.release(index, time);
                        }
                    }
                }
            }
            LiveAction::Param { id, value } => {
                self.params.insert(id, value);
                self.wire.push(WireEvent::Param { time, id, value });
            }
            LiveAction::Panic => self.panic(time),
        }
    }

    fn release(&mut self, index: usize, time: u32) {
        let voice = &mut self.voices[index];
        voice.released = true;
        voice.sustained = false;
        self.wire.push(WireEvent::NoteOff {
            time,
            note_id: voice.note_id,
            channel: voice.channel,
            key: voice.key,
        });
    }

    fn panic(&mut self, time: u32) {
        self.sustain = [false; 16];
        for index in 0..self.voices.len() {
            if !self.voices[index].released {
                self.release(index, time);
            }
        }
        self.shared.panics.fetch_add(1, RELAXED);
    }

    fn note_ends(&mut self, ends: &[NoteEnd]) {
        for end in ends {
            if let Some(index) = self.voices.iter().position(|v| v.note_id == end.note_id) {
                self.voices.remove(index);
                self.shared.note_ends.fetch_add(1, RELAXED);
            }
        }
        self.shared.voices.store(self.voices.len(), RELAXED);
    }

    fn fault(&mut self, fault: Fault) {
        let s = &*self.shared;
        let text = match &fault {
            Fault::Helper(HelperFault::Timeout) => {
                s.timeouts.fetch_add(1, RELAXED);
                format!(
                    "{} did not answer within {} ms and was stopped",
                    self.stage.plugin.id,
                    self.config.block_timeout.as_millis()
                )
            }
            Fault::Helper(fault @ HelperFault::Exited { .. }) => {
                s.exits.fetch_add(1, RELAXED);
                format!("{}: {fault}", self.stage.plugin.id)
            }
            Fault::Helper(fault) => {
                s.protocol_faults.fetch_add(1, RELAXED);
                format!("{}: {fault}", self.stage.plugin.id)
            }
            Fault::Error(error) => {
                s.protocol_faults.fetch_add(1, RELAXED);
                format!(
                    "{}: {}: {}",
                    self.stage.plugin.id, error.code, error.message
                )
            }
            Fault::NonFinite => {
                s.nonfinite_blocks.fetch_add(1, RELAXED);
                format!("{} produced non-finite audio", self.stage.plugin.id)
            }
        };
        if let Ok(mut slot) = self.fault.lock() {
            *slot = Some(text);
        }
        // Every voice died with the old process.
        self.helper = None;
        self.voices.clear();
        self.sustain = [false; 16];
        s.voices.store(0, RELAXED);
        if self.restarts >= self.config.restart_budget {
            s.set_state(LiveState::Disabled);
            return;
        }
        self.restarts += 1;
        s.restarts.fetch_add(1, RELAXED);
        s.set_state(LiveState::Restarting);
        self.stage.params = self.current_params();
        match open_helper(&self.stage, &self.config) {
            Ok((helper, _)) => {
                self.helper = Some(helper);
                s.set_state(LiveState::Running);
            }
            Err(error) => {
                if let Ok(mut slot) = self.fault.lock() {
                    *slot = Some(format!("restart failed: {error}"));
                }
                s.set_state(LiveState::Disabled);
            }
        }
    }

    fn current_params(&self) -> Vec<Param> {
        self.params
            .iter()
            .map(|(id, value)| Param {
                id: *id,
                value: *value,
            })
            .collect()
    }

    fn save_state(&mut self) -> Result<Stage, LiveError> {
        self.stage.params = self.current_params();
        let state = self.shared.state();
        let Some(helper) = self.helper.as_mut().filter(|_| state == LiveState::Running) else {
            return Err(if state == LiveState::Stopped {
                LiveError::Stopped
            } else {
                LiveError::Disabled
            });
        };
        match helper.save_state(self.close_timeout) {
            Ok(Ok(state)) => {
                self.stage.state = state.map(Arc::from);
                Ok(self.stage.clone())
            }
            Ok(Err(error)) => {
                self.fault(Fault::Error(error.clone()));
                Err(LiveError::Unavailable(error))
            }
            Err(fault) => {
                self.fault(Fault::Helper(fault.clone()));
                Err(LiveError::Helper(fault))
            }
        }
    }

    fn set_capture(&mut self, on: bool) -> Option<Captured> {
        let previous = if on {
            self.capture.replace(Capture {
                start: None,
                samples: Vec::new(),
            })
        } else {
            self.capture.take()
        };
        previous.map(|capture| Captured {
            start_frame: capture.start.unwrap_or(self.rendered),
            rate: self.config.rate,
            samples: capture.samples,
        })
    }

    fn stop(&mut self) -> Result<Stage, LiveError> {
        self.stage.params = self.current_params();
        let running = self.shared.state() == LiveState::Running;
        self.shared.set_state(LiveState::Stopped);
        let Some(mut helper) = self.helper.take() else {
            return Ok(self.stage.clone());
        };
        if !running {
            return Ok(self.stage.clone());
        }
        match helper.end(self.close_timeout) {
            Ok(Ok(state)) => {
                self.stage.state = state.map(Arc::from);
                Ok(self.stage.clone())
            }
            Ok(Err(error)) => Err(LiveError::Unavailable(error)),
            Err(fault) => Err(LiveError::Helper(fault)),
        }
    }
}
