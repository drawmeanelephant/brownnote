//! Live CLAP instrument backend (#112) through real helper processes.
//!
//! The live modules are compiled here by path until the integrator declares
//! `plugins` in `lib.rs`; they use only the crate's public API, so they build
//! unchanged in either place.
//!
//! The fixture instrument (`native/plugin-host/fault-fixture`) is exact by
//! construction, so every expected sample below is computed from its
//! documented formula, not from another render.
#![allow(unsafe_code)]

mod pedals {
    pub use brownnote_native::pedals::offline;
}
mod performance {
    pub use brownnote_native::performance::*;
}
#[path = "../src/plugins/mod.rs"]
#[allow(dead_code)]
mod plugins;

use brownnote_native::engine::{Command, Meters, Mixer, QUEUE_CAPACITY, Sample, Slots};
use brownnote_native::midi::parser::{Message, parse_message};
use brownnote_native::pedals::offline::{HelperCommand, Param, Stage};
use brownnote_native::performance::{
    Channel, EventId, InstrumentId, NoteKey, NoteLifecycle, NoteOn, OriginalTime, Pitch,
    SampleRate, SourceId, TakeId, TakeTiming, Target, Velocity,
};
use plugins::live::{
    LiveAction, LiveConfig, LiveError, LiveEvent, LiveHost, LiveSender, LiveState, LiveVoice,
    SendOutcome, identify,
};
use plugins::live_protocol::HelperFault;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

struct Counting;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
}

fn count(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    let _ = TRACK.try_with(|track| {
        if track.get() {
            let _ = counter.try_with(|n| n.set(n.get() + 1));
        }
    });
}

// SAFETY: every allocation is forwarded unchanged to System; TLS only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(&FREES);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const RATE: u32 = 48_000;
const INSTRUMENT: &str = "org.brownnote.fixture.instrument";

struct Env {
    helper: HelperCommand,
    fixture: PathBuf,
}

/// The live helper and fixture, or `None` (a skip) unless
/// `BROWNNOTE_REQUIRE_CLAP=1`, which `make native-test` sets.
fn env() -> Option<Env> {
    let require = std::env::var("BROWNNOTE_REQUIRE_CLAP").as_deref() == Ok("1");
    let helper = std::env::var_os("BROWNNOTE_CLAP_LIVE_HELPER")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("BROWNNOTE_CLAP_HELPER").and_then(|offline| {
                Path::new(&offline)
                    .parent()
                    .map(|dir| dir.join("brownnote-clap-live"))
            })
        });
    let fixture = std::env::var_os("BROWNNOTE_CLAP_FAULT_FIXTURE").map(PathBuf::from);
    match (helper, fixture) {
        (Some(helper), Some(fixture)) if helper.is_file() && fixture.is_file() => Some(Env {
            helper: HelperCommand {
                program: helper,
                args: Vec::new(),
            },
            fixture,
        }),
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

fn config(env: &Env, lead_frames: u32) -> LiveConfig {
    LiveConfig {
        lead_frames,
        ..LiveConfig::for_device(env.helper.clone(), RATE, 256)
    }
}

fn stage(env: &Env, id: &str) -> Stage {
    let info = identify(&env.helper, id, &env.fixture, RATE, Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("identify {id}: {error}"));
    Stage {
        plugin: info.plugin,
        location: env.fixture.clone(),
        params: Vec::new(),
        state: None,
    }
}

fn instrument_state(generation: u64, gain: f64) -> Arc<[u8]> {
    let mut bytes = generation.to_le_bytes().to_vec();
    bytes.extend_from_slice(&gain.to_le_bytes());
    Arc::from(bytes)
}

fn id(sequence: u64) -> EventId {
    EventId {
        take: TakeId(7),
        sequence,
    }
}

fn channel(value: u8) -> Channel {
    Channel::try_from(value).unwrap_or_else(|_| unreachable!())
}

fn on(frame: u64, sequence: u64, ch: u8, key: u8, velocity: u8) -> LiveEvent {
    LiveEvent {
        frame,
        action: LiveAction::NoteOn {
            id: id(sequence),
            channel: channel(ch),
            pitch: Pitch::try_from(key).unwrap_or_else(|_| unreachable!()),
            velocity: Velocity::try_from(velocity).unwrap_or_else(|_| unreachable!()),
        },
    }
}

fn off(frame: u64, sequence: u64) -> LiveEvent {
    LiveEvent {
        frame,
        action: LiveAction::NoteOff { id: id(sequence) },
    }
}

fn send_all(sender: &mut LiveSender, events: &[LiveEvent]) {
    for event in events {
        assert_eq!(sender.try_send(*event), SendOutcome::Accepted, "{event:?}");
    }
}

/// Waits (off the callback) until the worker has rendered through `frame` and
/// is running, or has stopped for good. A restart in flight keeps waiting:
/// returning then would let the callback free-run past the worker.
fn wait_rendered(host: &LiveHost, frame: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = host.status();
        let done = match status.state {
            LiveState::Running => status.rendered >= frame,
            LiveState::Restarting => false,
            LiveState::Disabled | LiveState::Stopped => true,
        };
        if done {
            return;
        }
        assert!(Instant::now() < deadline, "worker stalled: {status:?}");
        thread::sleep(Duration::from_micros(200));
    }
}

/// Plays `frames` frames through the callback handle in `sizes`-frame
/// callbacks, each only after its audio is rendered, so no frame underruns.
fn play_lockstep(
    host: &LiveHost,
    voice: &mut LiveVoice,
    frames: usize,
    sizes: &[usize],
    channels: usize,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * channels);
    let mut index = 0;
    while out.len() < frames * channels {
        let n = sizes[index % sizes.len()].min(frames - out.len() / channels);
        index += 1;
        wait_rendered(host, voice.frame() + n as u64);
        let mut buffer = vec![0.0; n * channels];
        voice.render_add(&mut buffer, channels);
        out.extend_from_slice(&buffer);
    }
    out
}

/// Notes as (start frame, end frame, key, velocity).
type Notes = Vec<(u64, u64, u8, u8)>;

/// The fixture's documented output for notes `(start, end, key, velocity)`.
fn expected(notes: &[(u64, u64, u8, u8)], gain: f64, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * 2);
    for frame in 0..frames as u64 {
        let mut left = 0.0f64;
        let mut right = 0.0f64;
        for &(start, end, key, velocity) in notes {
            if (start..end).contains(&frame) {
                let amp = gain * 0.1 * (f64::from(velocity) / 127.0);
                left += amp;
                right += amp * f64::from(key) / 127.0;
            }
        }
        out.push(left as f32);
        out.push(right as f32);
    }
    out
}

fn first_difference(got: &[f32], want: &[f32]) -> Option<(usize, f32, f32)> {
    got.iter()
        .zip(want)
        .enumerate()
        .find(|(_, (g, w))| g.to_bits() != w.to_bits())
        .map(|(i, (g, w))| (i, *g, *w))
}

fn events_scenario(base: u64) -> (Vec<LiveEvent>, Notes) {
    let sustain = |frame, down| LiveEvent {
        frame,
        action: LiveAction::Sustain {
            channel: channel(0),
            down,
        },
    };
    let events = vec![
        on(base + 1000, 1, 0, 60, 100),
        // Same pitch, same channel: D14 gives it its own identity.
        on(base + 1500, 2, 0, 60, 50),
        // Ends exactly note 1, the oldest, not note 2.
        off(base + 3001, 1),
        sustain(base + 4000, true),
        off(base + 4100, 2),
        on(base + 4500, 3, 0, 72, 127),
        off(base + 5000, 3),
        // Pedal up releases both held notes at this frame.
        sustain(base + 6003, false),
        on(base + 7000, 4, 1, 40, 1),
        LiveEvent {
            frame: base + 8000,
            action: LiveAction::Panic,
        },
    ];
    let notes = vec![
        (base + 1000, base + 3001, 60, 100),
        (base + 1500, base + 6003, 60, 50),
        (base + 4500, base + 6003, 72, 127),
        (base + 7000, base + 8000, 40, 1),
    ];
    (events, notes)
}

#[test]
fn notes_are_sample_exact_with_ids_fifo_sustain_and_panic_at_any_buffer_size() {
    let Some(env) = env() else { return };
    for channels in [2, 1] {
        let (host, mut sender, mut voice) =
            LiveHost::start(stage(&env, INSTRUMENT), config(&env, 1024))
                .unwrap_or_else(|error| panic!("start: {error}"));
        let base = sender.earliest_exact();
        assert_eq!(base, 1024);
        let (events, notes) = events_scenario(base);
        send_all(&mut sender, &events);
        let frames = base as usize + 9000;
        let got = play_lockstep(
            &host,
            &mut voice,
            frames,
            &[37, 256, 512, 31, 64, 1, 200],
            channels,
        );
        let want = expected(&notes, 1.0, frames);
        let want: Vec<f32> = if channels == 2 {
            want
        } else {
            want.as_chunks::<2>()
                .0
                .iter()
                .map(|[l, r]| 0.5 * (l + r))
                .collect()
        };
        assert_eq!(first_difference(&got, &want), None, "{channels} channels");
        assert!(got.iter().any(|s| *s != 0.0));
        let status = host.status();
        let c = status.counters;
        assert_eq!(status.state, LiveState::Running);
        assert_eq!(status.voices, 0, "every note ended");
        assert_eq!(c.note_ends, 4);
        assert_eq!((c.accepted_events, c.late_events), (10, 0));
        assert_eq!((c.unmatched_note_offs, c.duplicate_note_ons), (0, 0));
        assert_eq!((c.underrun_frames, c.stale_frames), (0, 0));
        assert_eq!(c.played_frames, frames as u64);
        assert_eq!(c.panics, 1);
        host.stop().unwrap_or_else(|error| panic!("stop: {error}"));
    }
}

#[test]
fn a_starved_callback_never_waits_and_the_worker_resynchronizes_exactly() {
    let Some(env) = env() else { return };
    let (host, mut sender, mut voice) = LiveHost::start(stage(&env, INSTRUMENT), config(&env, 512))
        .unwrap_or_else(|error| panic!("start: {error}"));
    // Far faster than real time: the callback must not wait for the worker.
    let mut buffer = vec![0.0f32; 1024];
    let mut worst = Duration::ZERO;
    for _ in 0..2000 {
        buffer.fill(0.0);
        let started = Instant::now();
        voice.render_add(&mut buffer, 2);
        worst = worst.max(started.elapsed());
        assert!(buffer.iter().all(|s| s.is_finite()));
    }
    let starved = host.status().counters;
    assert!(starved.underrun_frames > 0, "{starved:?}");
    assert_eq!(starved.played_frames + starved.underrun_frames, 2000 * 512);
    assert!(
        worst < Duration::from_millis(2),
        "a starved callback took {worst:?}"
    );
    // Lateness never becomes delay: once the worker catches up, an event
    // scheduled one lead ahead sounds on its exact frame again.
    let start = voice.frame();
    let at = sender.earliest_exact() + 64 + 3;
    send_all(&mut sender, &[on(at, 1, 0, 64, 90), off(at + 700, 1)]);
    let got = play_lockstep(&host, &mut voice, 2048, &[256], 2);
    let notes = [(at - start, at + 700 - start, 64, 90)];
    assert_eq!(first_difference(&got, &expected(&notes, 1.0, 2048)), None);
    eprintln!(
        "starved: worst callback {worst:?}, underrun {} of {} frames, stale {}, skipped {}",
        starved.underrun_frames,
        2000 * 512,
        starved.stale_frames,
        host.status().counters.skipped_frames
    );
    host.stop().unwrap_or_else(|error| panic!("stop: {error}"));
}

#[test]
fn crashes_restart_with_restored_state_until_the_budget_disables_the_instrument() {
    let Some(env) = env() else { return };
    let mut crash = stage(&env, "org.brownnote.fixture.instrument.crash");
    crash.state = Some(instrument_state(1, 0.5));
    let (host, mut sender, mut voice) =
        LiveHost::start(crash, config(&env, 256)).unwrap_or_else(|error| panic!("start: {error}"));
    // A short note every 128 frames, ahead of everything rendered so far.
    let events: Vec<LiveEvent> = (0..40u64)
        .flat_map(|i| {
            let frame = 256 + i * 128;
            [on(frame, i, 0, 60, 100), off(frame + 64, i)]
        })
        .collect();
    send_all(&mut sender, &events);
    let amp = (0.5 * 0.1 * (100.0 / 127.0)) as f32;
    let mut after_restart = 0;
    let mut buffer = [0.0f32; 128];
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.status().state != LiveState::Disabled {
        assert!(Instant::now() < deadline, "{:?}", host.status());
        let restarts = host.status().counters.restarts;
        wait_rendered(&host, voice.frame() + 64);
        buffer.fill(0.0);
        voice.render_add(&mut buffer, 2);
        for frame in buffer.as_chunks::<2>().0 {
            assert!(frame[0] == 0.0 || frame[0] == amp, "{frame:?}");
            after_restart += usize::from(restarts > 0 && frame[0] == amp);
        }
    }
    assert!(
        after_restart > 0,
        "no audio after a restart: {:?}",
        host.status()
    );
    let status = host.status();
    let c = status.counters;
    assert_eq!((c.exits, c.restarts), (3, 2), "{status:?}");
    let fault = status.fault.unwrap_or_default();
    assert!(fault.contains("signal Some(11)"), "{fault}");
    // Disabled: the callback keeps running and adds nothing.
    buffer.fill(0.25);
    voice.render_add(&mut buffer, 2);
    assert!(buffer.iter().all(|s| *s == 0.25));
    assert_eq!(
        sender.try_send(on(voice.frame() + 512, 99, 0, 60, 1)),
        SendOutcome::Unavailable
    );
    assert_eq!(host.save_state(), Err(LiveError::Disabled));
}

#[test]
fn a_hung_plugin_is_cut_at_its_deadline_while_the_callback_keeps_time() {
    let Some(env) = env() else { return };
    let hang = stage(&env, "org.brownnote.fixture.instrument.hang");
    let config = LiveConfig {
        block_timeout: Duration::from_millis(150),
        restart_budget: 0,
        ..config(&env, 256)
    };
    let (host, _sender, mut voice) =
        LiveHost::start(hang, config).unwrap_or_else(|error| panic!("start: {error}"));
    let mut buffer = [0.0f32; 512];
    let mut worst = Duration::ZERO;
    let started = Instant::now();
    let mut disabled_at = None;
    // Real-time pacing: one 256-frame callback every 5.33 ms for 1 s.
    for tick in 0..188u32 {
        let due = started + Duration::from_micros(5_333) * tick;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let begin = Instant::now();
        voice.render_add(&mut buffer, 2);
        worst = worst.max(begin.elapsed());
        if disabled_at.is_none() && host.status().state == LiveState::Disabled {
            disabled_at = Some(started.elapsed());
        }
    }
    let status = host.status();
    assert_eq!(status.state, LiveState::Disabled, "{status:?}");
    assert_eq!(status.counters.timeouts, 1);
    assert!(status.counters.underrun_frames > 0, "{status:?}");
    let disabled_at = disabled_at.unwrap_or(Duration::MAX);
    assert!(disabled_at < Duration::from_millis(600), "{disabled_at:?}");
    assert!(worst < Duration::from_millis(2), "callback took {worst:?}");
    eprintln!(
        "hang: disabled after {disabled_at:?}, worst callback {worst:?}, underrun {} frames",
        status.counters.underrun_frames
    );
}

#[test]
fn non_finite_output_never_reaches_the_callback() {
    let Some(env) = env() else { return };
    let nan = stage(&env, "org.brownnote.fixture.instrument.nonfinite");
    let config = LiveConfig {
        restart_budget: 1,
        ..config(&env, 256)
    };
    let (host, _sender, mut voice) =
        LiveHost::start(nan, config).unwrap_or_else(|error| panic!("start: {error}"));
    let mut buffer = [0.0f32; 128];
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.status().state != LiveState::Disabled {
        assert!(Instant::now() < deadline);
        wait_rendered(&host, voice.frame() + 64);
        voice.render_add(&mut buffer, 2);
        assert!(buffer.iter().all(|s| s.is_finite()));
    }
    let c = host.status().counters;
    assert_eq!((c.nonfinite_blocks, c.restarts), (2, 1), "{c:?}");
}

#[test]
fn load_failures_are_typed_and_start_nothing() {
    let Some(env) = env() else { return };
    let code = |error: LiveError| match error {
        LiveError::Unavailable(error) => error.code,
        other => panic!("expected a typed helper error, got {other:?}"),
    };
    let missing = Path::new("/nonexistent/Missing Instrument.clap");
    let timeout = Duration::from_secs(5);
    let fail = |id: &str, location: &Path| {
        identify(&env.helper, id, location, RATE, timeout)
            .err()
            .map(code)
    };
    assert_eq!(fail(INSTRUMENT, missing).as_deref(), Some("bundle-missing"));
    assert_eq!(
        fail("org.example.none", &env.fixture).as_deref(),
        Some("unknown-plugin-id")
    );
    // An effect has no note input port.
    assert_eq!(
        fail("org.brownnote.fault.passthrough", &env.fixture).as_deref(),
        Some("unsupported")
    );
    assert_eq!(
        fail("org.brownnote.fault.init-fails", &env.fixture).as_deref(),
        Some("init-failed")
    );
    // A recalled stage whose binary changed is refused, not silently loaded.
    let mut changed = stage(&env, INSTRUMENT);
    changed.plugin.artifact.0[0] ^= 1;
    assert!(matches!(
        LiveHost::start(changed, config(&env, 256)).err(),
        Some(LiveError::ArtifactChanged { .. })
    ));
    let mut wrong_param = stage(&env, INSTRUMENT);
    wrong_param.params = vec![Param { id: 0, value: 9.0 }];
    assert_eq!(
        LiveHost::start(wrong_param, config(&env, 256))
            .err()
            .map(code)
            .as_deref(),
        Some("param-rejected")
    );
    let mut bad_state = stage(&env, INSTRUMENT);
    bad_state.state = Some(instrument_state(1, 7.0));
    assert_eq!(
        LiveHost::start(bad_state, config(&env, 256))
            .err()
            .map(code)
            .as_deref(),
        Some("state-rejected")
    );
    let no_helper = LiveConfig {
        helper: HelperCommand {
            program: PathBuf::from("/nonexistent/brownnote-clap-live"),
            args: Vec::new(),
        },
        ..config(&env, 256)
    };
    assert!(matches!(
        LiveHost::start(stage(&env, INSTRUMENT), no_helper).err(),
        Some(LiveError::Helper(HelperFault::Spawn(_)))
    ));
    assert!(matches!(
        LiveHost::start(stage(&env, INSTRUMENT), config(&env, 100)).err(),
        Some(LiveError::Invalid(_))
    ));
}

#[test]
fn params_and_state_recall_into_a_fresh_helper_and_rendered_audio_is_retained() {
    let Some(env) = env() else { return };
    let (host, mut sender, mut voice) = LiveHost::start(stage(&env, INSTRUMENT), config(&env, 512))
        .unwrap_or_else(|error| panic!("start: {error}"));
    let base = sender.earliest_exact();
    send_all(
        &mut sender,
        &[
            LiveEvent {
                frame: base,
                action: LiveAction::Param { id: 0, value: 1.5 },
            },
            on(base + 100, 1, 0, 64, 127),
            off(base + 900, 1),
        ],
    );
    assert_eq!(host.capture(true), Ok(None));
    let played = play_lockstep(&host, &mut voice, base as usize + 1024, &[128], 2);
    let notes = [(base + 100, base + 900, 64, 127)];
    let want = expected(&notes, 1.5, base as usize + 1024);
    assert_eq!(first_difference(&played, &want), None);
    let saved = host
        .save_state()
        .unwrap_or_else(|error| panic!("save: {error}"));
    assert_eq!(saved.params, vec![Param { id: 0, value: 1.5 }]);
    let state = saved.state.clone().unwrap_or_else(|| panic!("no state"));
    assert_eq!(state.len(), 16);
    assert_eq!(state[8..], 1.5f64.to_le_bytes());
    let captured = host
        .capture(false)
        .unwrap_or_else(|error| panic!("capture: {error}"))
        .unwrap_or_else(|| panic!("nothing captured"));
    // The retained render is the audio the callback played, frame-aligned.
    let from = captured.start_frame as usize * 2;
    let overlap = (played.len() - from).min(captured.samples.len());
    assert!(overlap >= 2 * 1024);
    assert_eq!(
        first_difference(&captured.samples[..overlap], &played[from..from + overlap]),
        None
    );
    let stopped = host.stop().unwrap_or_else(|error| panic!("stop: {error}"));
    assert_eq!(stopped.plugin, saved.plugin);

    // Recall from the state alone, in a fresh helper process.
    let recalled = Stage {
        params: Vec::new(),
        ..stopped.clone()
    };
    let (host, mut sender, mut voice) = LiveHost::start(recalled, config(&env, 512))
        .unwrap_or_else(|error| panic!("recall: {error}"));
    let base = sender.earliest_exact();
    send_all(&mut sender, &[on(base, 1, 0, 64, 127), off(base + 64, 1)]);
    let got = play_lockstep(&host, &mut voice, base as usize + 128, &[64], 2);
    let want = expected(&[(base, base + 64, 64, 127)], 1.5, base as usize + 128);
    assert_eq!(first_difference(&got, &want), None);
    host.stop().unwrap_or_else(|error| panic!("stop: {error}"));

    // The plugin is gone: reopening reports it and touches nothing else, so a
    // project keeps playing its retained audio with the instrument disabled.
    let gone = Stage {
        location: PathBuf::from("/nonexistent/Fixture.clap"),
        ..stopped
    };
    match LiveHost::start(gone, config(&env, 512)) {
        Err(LiveError::Unavailable(error)) => assert_eq!(error.code, "bundle-missing"),
        other => panic!("expected bundle-missing, got {:?}", other.err()),
    }
    assert!(captured.samples.iter().any(|s| *s != 0.0));
}

#[test]
fn software_midi_input_records_a_take_that_replays_and_recalls_identically() {
    let Some(env) = env() else { return };
    let source = SourceId(3);
    let take = TakeId(11);
    let rate = SampleRate::try_from(RATE).unwrap_or_else(|_| unreachable!());
    // Software MIDI bytes, including two overlapping notes on one pitch.
    let input: [(u64, [u8; 3]); 8] = [
        (0, [0x90, 60, 100]),
        (480, [0x90, 60, 40]),
        (960, [0x80, 60, 0]),
        (1200, [0x91, 67, 127]),
        (1500, [0xb0, 64, 127]),
        (1700, [0x80, 60, 0]),
        (2400, [0x81, 67, 0]),
        (3000, [0xb0, 64, 0]),
    ];
    let (host, mut sender, mut voice) = LiveHost::start(stage(&env, INSTRUMENT), config(&env, 512))
        .unwrap_or_else(|error| panic!("start: {error}"));
    let base = sender.earliest_exact();
    let mut lifecycle = NoteLifecycle::default();
    let mut notes = Vec::new();
    for (sequence, (offset, bytes)) in input.iter().enumerate() {
        let frame = base + offset;
        let time = OriginalTime::from_frames(*offset, rate).unwrap_or_else(|_| unreachable!());
        let action = match parse_message(bytes).unwrap_or_else(|e| panic!("{e:?}")) {
            Message::NoteOn {
                channel,
                pitch,
                velocity,
            } => {
                let on = NoteOn {
                    id: EventId {
                        take,
                        sequence: sequence as u64,
                    },
                    target: Target::Instrument(InstrumentId(1)),
                    key: NoteKey {
                        source,
                        channel,
                        pitch,
                    },
                    velocity,
                    original_time: time,
                };
                lifecycle
                    .note_on(on.clone())
                    .unwrap_or_else(|e| panic!("{e}"));
                LiveAction::NoteOn {
                    id: on.id,
                    channel,
                    pitch,
                    velocity,
                }
            }
            Message::NoteOff { channel, pitch, .. } => {
                let key = NoteKey {
                    source,
                    channel,
                    pitch,
                };
                let note = lifecycle
                    .note_off(key, time)
                    .unwrap_or_else(|e| panic!("{e}"));
                let id = note.on().id;
                notes.push(note);
                LiveAction::NoteOff { id }
            }
            Message::ControlChange {
                channel,
                controller,
                value,
            } => {
                assert_eq!(controller.value(), 64);
                LiveAction::Sustain {
                    channel,
                    down: value.value() >= 64,
                }
            }
        };
        send_all(&mut sender, &[LiveEvent { frame, action }]);
    }
    assert_eq!(lifecycle.active_count(), 0);
    assert_eq!(host.capture(true), Ok(None));
    let total = base as usize + 4096;
    let live = play_lockstep(&host, &mut voice, total, &[160, 96], 2);
    let first = host
        .capture(false)
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("nothing captured"));
    // FIFO: the first note-off ended the first 60. The sustained notes end at
    // the pedal-up.
    let want = expected(
        &[
            (base, base + 960, 60, 100),
            (base + 480, base + 3000, 60, 40),
            (base + 1200, base + 2400, 67, 127),
        ],
        1.0,
        total,
    );
    // Sustain is a performance event, not part of the take's notes; replay
    // uses the recorded note ends, which the pedal extended live.
    assert_eq!(first_difference(&live, &want), None);
    let stopped = host.stop().unwrap_or_else(|e| panic!("{e}"));

    // Replay the recorded take (its notes, through the take timing view) in a
    // fresh helper recalled from the saved stage, with the pedal events.
    let timing = TakeTiming::new(take, Default::default());
    let (host, mut sender, mut voice) =
        LiveHost::start(stopped, config(&env, 512)).unwrap_or_else(|e| panic!("{e}"));
    let replay_base = sender.earliest_exact();
    let tempo = timing.recording_tempo();
    let frame_of = |position: brownnote_native::performance::MusicalPosition| {
        replay_base
            + position
                .at_tempo_frames(tempo, rate)
                .unwrap_or_else(|e| panic!("{e}"))
    };
    let mut replay = Vec::new();
    for note in &notes {
        let view = timing.view(note).unwrap_or_else(|e| panic!("{e}"));
        replay.push(LiveEvent {
            frame: frame_of(view.start),
            action: LiveAction::NoteOn {
                id: view.id,
                channel: view.key.channel,
                pitch: view.key.pitch,
                velocity: view.velocity,
            },
        });
        replay.push(LiveEvent {
            frame: frame_of(view.end),
            action: LiveAction::NoteOff { id: view.id },
        });
    }
    for (offset, down) in [(1500, true), (3000, false)] {
        replay.push(LiveEvent {
            frame: replay_base + offset,
            action: LiveAction::Sustain {
                channel: channel(0),
                down,
            },
        });
    }
    replay.sort_by_key(|event| event.frame);
    send_all(&mut sender, &replay);
    assert_eq!(host.capture(true), Ok(None));
    let _ = play_lockstep(&host, &mut voice, replay_base as usize + 4096, &[512], 2);
    let second = host
        .capture(false)
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("nothing captured"));
    let take_audio = |captured: &plugins::live::Captured, base: u64| -> Vec<f32> {
        let from = (base - captured.start_frame) as usize * 2;
        captured.samples[from..from + 2 * 3500].to_vec()
    };
    assert_eq!(
        first_difference(&take_audio(&second, replay_base), &take_audio(&first, base)),
        None
    );
    host.stop().unwrap_or_else(|e| panic!("{e}"));
}

/// The composed callback the output stream would run: the engine's own
/// drain/render, then the live instrument, into the same buffer.
struct Composed {
    mixer: Mixer,
    commands: rtrb::Consumer<Command>,
    voice: LiveVoice,
}

impl Composed {
    fn callback(&mut self, data: &mut [f32]) {
        self.mixer.drain(&mut self.commands);
        self.mixer.render(data, 2);
        self.voice.render_add(data, 2);
    }
}

fn tracked(composed: &mut Composed, data: &mut [f32]) {
    TRACK.set(true);
    composed.callback(data);
    TRACK.set(false);
}

#[test]
fn engine_callback_with_the_live_instrument_allocates_and_frees_nothing() {
    let Some(env) = env() else { return };
    for (rate, id) in [
        (44_100, INSTRUMENT),
        (48_000, INSTRUMENT),
        (48_000, "org.brownnote.fixture.instrument.crash"),
    ] {
        for block in [31usize, 64, 256, 512] {
            let mut plugin = stage(&env, id);
            plugin.state = Some(instrument_state(1, 1.0));
            let config = LiveConfig {
                rate,
                lead_frames: 1024,
                ..LiveConfig::for_device(env.helper.clone(), rate, 512)
            };
            let (host, mut sender, voice) =
                LiveHost::start(plugin, config).unwrap_or_else(|e| panic!("{e}"));
            let (mut send, commands) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
            let (retired, _returns) = rtrb::RingBuffer::new(4);
            let mixer = Mixer::new(
                Slots::from_samples([Sample {
                    data: Arc::from(vec![0.01; rate as usize]),
                    rate,
                }]),
                rate,
                Arc::new(Meters::default()),
                retired,
            );
            let mut composed = Composed {
                mixer,
                commands,
                voice,
            };
            let mut data = vec![0.0f32; block * 2];
            ALLOCS.set(0);
            FREES.set(0);
            // The very first call, straight after start, before any wait.
            tracked(&mut composed, &mut data);
            // A storm: more notes at one frame than a block can carry, plus
            // engine triggers, rendered in lockstep.
            let at = sender.earliest_exact();
            for i in 0..600u64 {
                let _ = sender.try_send(on(at + i % 7, i, (i % 16) as u8, (i % 128) as u8, 100));
            }
            let _ = send.push(Command::Trigger {
                pad: 0,
                slot: 0,
                group: 0,
                velocity: 1.0,
                gain: 1.0,
                pan: 0.0,
                semitones: 0.0,
            });
            for _ in 0..(4096 / block).max(8) {
                wait_rendered(&host, composed.voice.frame() + block as u64);
                tracked(&mut composed, &mut data);
                assert!(data.iter().all(|s| s.is_finite()));
            }
            let _ = sender.try_send(LiveEvent {
                frame: sender.earliest_exact(),
                action: LiveAction::Panic,
            });
            // Starved calls: underruns and stale blocks on the callback path.
            for _ in 0..200 {
                tracked(&mut composed, &mut data);
            }
            // Through restarts and the disabled state, for the crash variant.
            let deadline = Instant::now() + Duration::from_secs(10);
            while id != INSTRUMENT && host.status().state != LiveState::Disabled {
                assert!(Instant::now() < deadline, "{:?}", host.status());
                wait_rendered(&host, composed.voice.frame() + block as u64);
                tracked(&mut composed, &mut data);
            }
            for _ in 0..16 {
                tracked(&mut composed, &mut data);
            }
            assert_eq!(ALLOCS.get(), 0, "callback allocated: {id} {rate}/{block}");
            assert_eq!(FREES.get(), 0, "callback freed: {id} {rate}/{block}");
            let c = host.status().counters;
            assert!(c.played_frames > 0 && c.callbacks > 0, "{c:?}");
            if id != INSTRUMENT {
                assert_eq!(host.status().state, LiveState::Disabled);
            }
        }
    }
}

/// Opt-in (`BROWNNOTE_LIVE_CPAL=1`): the composed callback on the default
/// output device through CPAL. It measures callback duration against the
/// device period and counts underruns. The device receives silence: the
/// callback renders into its buffer, measures, then zeroes it.
#[test]
fn cpal_device_callback_timing_with_the_live_instrument() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    if std::env::var("BROWNNOTE_LIVE_CPAL").as_deref() != Ok("1") {
        eprintln!("skipping: set BROWNNOTE_LIVE_CPAL=1 to open the default output device");
        return;
    }
    let Some(env) = env() else { return };
    let device = cpal::default_host()
        .default_output_device()
        .unwrap_or_else(|| panic!("no output device"));
    let supported = device
        .default_output_config()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(supported.sample_format(), cpal::SampleFormat::F32);
    let stream_config: cpal::StreamConfig = supported.config();
    let rate = stream_config.sample_rate;
    let channels = usize::from(stream_config.channels);
    let config = LiveConfig {
        rate,
        ..LiveConfig::for_device(env.helper.clone(), rate, 512)
    };
    let (host, mut sender, voice) =
        LiveHost::start(stage(&env, INSTRUMENT), config).unwrap_or_else(|e| panic!("{e}"));
    let (_send, commands) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
    let (retired, _returns) = rtrb::RingBuffer::new(4);
    let mixer = Mixer::new(Slots::empty(), rate, Arc::new(Meters::default()), retired);
    let mut composed = Composed {
        mixer,
        commands,
        voice,
    };
    let worst = Arc::new(AtomicU64::new(0));
    let over = Arc::new(AtomicU64::new(0));
    let calls = Arc::new(AtomicU64::new(0));
    let largest = Arc::new(AtomicU64::new(0));
    let (w, o, n, l) = (worst.clone(), over.clone(), calls.clone(), largest.clone());
    let mut stereo = vec![0.0f32; 16384];
    let stream = device
        .build_output_stream(
            stream_config,
            move |data: &mut [f32], _| {
                let started = Instant::now();
                let frames = data.len() / channels;
                let scratch = &mut stereo[..(frames * 2).min(16384)];
                scratch.fill(0.0);
                composed.callback(scratch);
                let elapsed = started.elapsed().as_nanos() as u64;
                data.fill(0.0);
                let period = frames as u64 * 1_000_000_000 / u64::from(rate);
                w.fetch_max(elapsed, Relaxed);
                l.fetch_max(frames as u64, Relaxed);
                o.fetch_add(u64::from(elapsed > period / 2), Relaxed);
                n.fetch_add(1, Relaxed);
            },
            |error| eprintln!("stream error: {error}"),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    stream.play().unwrap_or_else(|e| panic!("{e}"));
    let started = Instant::now();
    let mut sequence = 0;
    while started.elapsed() < Duration::from_secs(5) {
        let at = sender.earliest_exact();
        let _ = sender.try_send(on(at, sequence, 0, 60, 100));
        let _ = sender.try_send(off(at + u64::from(rate) / 20, sequence));
        sequence += 1;
        thread::sleep(Duration::from_millis(100));
    }
    drop(stream);
    let status = host.status();
    eprintln!(
        "cpal: {} Hz, {} ch, {} callbacks, largest {} frames, worst callback {} us, {} over half a period, {:?}",
        rate,
        channels,
        calls.load(Relaxed),
        largest.load(Relaxed),
        worst.load(Relaxed) / 1000,
        over.load(Relaxed),
        status.counters
    );
    assert_eq!(status.state, LiveState::Running);
    assert!(calls.load(Relaxed) > 0);
}
