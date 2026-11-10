//! The live instrument through the real app entry points: recall, retained
//! take audio, missing bundles and the panel.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{ui::projects, ui::transport, ui_test_support};
use brownnote_native::{
    live_audio,
    pedals::offline::{Hash256, Plugin},
    plugins::live::{LiveState, SendOutcome},
    project::AssetId,
};
use rune::{Cx, El, Env, Key, KeyEvent, Mods, Runtime};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Instant,
};

const INSTRUMENT: &str = "org.brownnote.fixture.instrument";
const RATE: u32 = 48_000;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "brownnote-live-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The live helper and fixture, or `None` (a skip) unless
/// `BROWNNOTE_REQUIRE_CLAP=1`, which `make native-test` sets.
fn env() -> Option<(HelperCommand, PathBuf)> {
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

fn state() -> State {
    let mut state = State::new(ui_test_support::kit(0.25, 44_100, 44_100), true, true);
    state.pedals.auto_load = false;
    state
}

fn finish(s: &mut State) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while (s.projects.busy() || s.live.busy()) && Instant::now() < deadline {
        projects::poll(s);
        poll(s);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!s.projects.busy(), "project worker timed out");
    assert!(!s.live.busy(), "live lookup timed out");
}

fn identify_fixture(helper: &HelperCommand, path: &Path) -> PluginInfo {
    identify(helper, INSTRUMENT, path, RATE, Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("identify: {error}"))
}

/// Waits (off the callback) until the worker has rendered through `frame`.
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
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Plays `frames` frames through the callback handle, only after the worker
/// rendered them, so the capture sees every frame the callback played.
fn play_lockstep(host: &LiveHost, voice: &mut LiveVoice, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * 2);
    while out.len() < frames * 2 {
        let n = 256.min(frames - out.len() / 2);
        wait_rendered(host, voice.frame() + n as u64);
        let mut buffer = vec![0.0; n * 2];
        voice.render_add(&mut buffer, 2);
        out.extend_from_slice(&buffer);
    }
    out
}

fn send(s: &mut State, events: &[LiveEvent]) {
    let session = s.live.session.as_mut().unwrap();
    for event in events {
        assert_eq!(session.sender.try_send(*event), SendOutcome::Accepted);
    }
}

fn note(id: u64, frame: u64, on: bool) -> LiveEvent {
    let event = EventId {
        take: TakeId(3),
        sequence: id,
    };
    LiveEvent {
        frame,
        action: if on {
            LiveAction::NoteOn {
                id: event,
                channel: Channel::try_from(0).unwrap(),
                pitch: Pitch::try_from(64).unwrap(),
                velocity: Velocity::try_from(90).unwrap(),
            }
        } else {
            LiveAction::NoteOff { id: event }
        },
    }
}

/// Save, then reopen in a new app instance: the opaque state is restored and
/// the recalled instrument renders the same samples as the saved session.
#[test]
fn save_then_reopen_restores_opaque_state_and_renders_identically() {
    let Some((helper, fixture)) = env() else {
        return;
    };
    let root = Root::new();
    let mut s = state();
    s.live.set_helper(helper.clone());
    let info = identify_fixture(&helper, &fixture);
    s.live
        .adopt(
            Chosen {
                info: info.clone(),
                path: fixture.clone(),
            },
            RATE,
        )
        .unwrap();
    let stage = s.live.stage_from_document().unwrap();
    let mut voice = s.live.mount(stage, RATE, 256).unwrap();

    // A parameter change lives only in the plugin's state.
    let base = s.live.session.as_ref().unwrap().sender.earliest_exact();
    send(
        &mut s,
        &[
            LiveEvent {
                frame: base,
                action: LiveAction::Param { id: 0, value: 1.5 },
            },
            note(1, base + 64, true),
            note(1, base + 1_064, false),
        ],
    );
    let played = {
        let session = s.live.session.as_ref().unwrap();
        play_lockstep(&session.host, &mut voice, 2_048)
    };
    assert!(
        played.iter().any(|sample| sample.abs() > 0.1),
        "fixture note was silent"
    );

    let saved = s.live.for_save().unwrap().unwrap();
    assert_eq!(saved.plugin, info.plugin);
    assert_eq!(saved.params.len(), 1);
    assert_eq!(saved.params[0].value.0, 1.5);
    let state_bytes = saved.state.as_ref().unwrap().0.clone();
    assert!(state_bytes.len() >= 16);
    s.live.document = Some(saved.clone());

    s.projects.path = TextEdit::new(root.0.to_string_lossy());
    projects::save(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);

    // Opening the same project while the live session runs is not a user
    // edit: inspecting opaque state must not fail admission.
    projects::open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert!(s.live.mounted());
    assert_eq!(
        s.live.document.as_ref().unwrap().plugin,
        saved.plugin,
        "the opened project keeps the identity"
    );

    // A new app instance, an empty kit, and no live session from the old one.
    let mut reopened = state();
    reopened.live.set_helper(helper);
    reopened.projects.path = TextEdit::new(root.0.to_string_lossy());
    projects::open(&mut reopened);
    finish(&mut reopened);
    assert!(
        reopened.projects.error.is_none(),
        "{:?}",
        reopened.projects.error
    );
    let document = reopened.live.document.clone().unwrap();
    assert_eq!(document.plugin, saved.plugin);
    assert_eq!(document.plugin.artifact, saved.plugin.artifact);
    assert_eq!(document.params, saved.params);
    let recalled_state = document.state.as_ref().unwrap().0.clone();
    assert_eq!(recalled_state.len(), 16);
    assert_eq!(&recalled_state[8..], &1.5f64.to_le_bytes());
    assert_eq!(document.identity_hash(), saved.identity_hash());
    assert!(
        reopened.live.mounted(),
        "reopened project did not start the instrument"
    );
    assert_eq!(reopened.live.status().unwrap().state, LiveState::Running);

    // The recalled session plays the same samples as the saved one.
    let stage = reopened.live.stage_from_document().unwrap();
    let mut voice = reopened.live.mount(stage, RATE, 256).unwrap();
    let base = reopened
        .live
        .session
        .as_ref()
        .unwrap()
        .sender
        .earliest_exact();
    send(
        &mut reopened,
        &[note(1, base + 64, true), note(1, base + 1_064, false)],
    );
    let recalled = {
        let session = reopened.live.session.as_ref().unwrap();
        play_lockstep(&session.host, &mut voice, 2_048)
    };
    assert_eq!(recalled, played);
    assert!(reopened.live.status_line().contains(INSTRUMENT));
}

/// A recorded pass keeps the exact stereo audio the instrument played. With
/// the bundle deleted the project still opens, the instrument is disabled with
/// its identity for a reason, and the retained take still plays.
#[test]
fn a_recorded_take_survives_a_missing_bundle_and_replays_its_audio() {
    let Some((helper, fixture)) = env() else {
        return;
    };
    let root = Root::new();
    let bundle = root.0.join("pinned-instrument.clap");
    fs::copy(&fixture, &bundle).unwrap();
    let mut s = state();
    s.live.set_helper(helper.clone());
    let info = identify_fixture(&helper, &bundle);
    s.live
        .adopt(
            Chosen {
                info: info.clone(),
                path: bundle.clone(),
            },
            RATE,
        )
        .unwrap();
    let stage = s.live.stage_from_document().unwrap();
    let mut voice = s.live.mount(stage, RATE, 256).unwrap();

    let pass_start = 5_000_000_000;
    let pass_end = pass_start + 100_000_000; // 4,800 frames at 48 kHz
    s.live.begin_capture(TakeId(9), pass_start, pass_start);
    let base = s.live.session.as_ref().unwrap().sender.earliest_exact();
    send(&mut s, &[note(4, base + 64, true)]);
    {
        let session = s.live.session.as_ref().unwrap();
        play_lockstep(&session.host, &mut voice, 4_800);
    }
    let media = s.media.owned.clone();
    s.live
        .finish_capture(media.as_ref(), TakeId(9), pass_start, pass_end);
    assert!(s.live.error.is_none(), "{:?}", s.live.error);
    let retained_notice = s.live.notice.clone().unwrap_or_default();
    assert!(retained_notice.starts_with("Retained"), "{retained_notice}");
    assert!(
        !retained_notice.contains("not retained"),
        "{retained_notice}"
    );
    let saved = s.live.for_save().unwrap().unwrap();
    assert_eq!(
        saved.takes.len(),
        1,
        "{:?} {:?}",
        s.live.notice,
        s.live.error
    );
    let take = &saved.takes[0];
    assert_eq!(take.take, "9");
    assert_eq!(take.rate, RATE);
    assert!(
        (3_776..=4_800).contains(&take.frames),
        "retained {} frames",
        take.frames
    );
    s.live.document = Some(saved.clone());
    s.projects.path = TextEdit::new(root.0.to_string_lossy());
    projects::save(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);

    let project = root.0.clone();
    let retained = fs::read(project.join(take.asset.relative_path())).unwrap();
    let clip = live_audio::decode(&retained, take.frames + 1).unwrap();
    assert_eq!(clip.rate, RATE);
    assert_eq!(clip.frames, take.frames);
    // The fixture plays a constant level while the note is held. Every
    // retained sample after the onset is exactly that level, so this is the
    // audio the callback played, not a re-render.
    let onset = clip
        .left
        .iter()
        .position(|sample| *sample != 0.0)
        .expect("retained audio never sounded");
    let played = clip.left[onset];
    assert!((played - 0.1 * 90.0 / 127.0).abs() < 1e-6);
    assert!(clip.left[onset..].iter().all(|sample| *sample == played));
    assert!(
        clip.right[onset..]
            .iter()
            .all(|sample| (*sample - played * 64.0 / 127.0).abs() < 1e-6),
        "right channel differs from the played note"
    );
    assert!(clip.left[..onset].iter().all(|sample| *sample == 0.0));
    // Capture begins on the worker's 64-frame block boundary, so the onset
    // offset from it is exact.
    assert_eq!((base + 64 - onset as u64) % 64, 0);

    // The bundle is gone: open a new app instance on the saved project.
    fs::remove_file(&bundle).unwrap();
    let mut reopened = state();
    reopened.live.set_helper(helper);
    reopened.projects.path = TextEdit::new(project.to_string_lossy());
    projects::open(&mut reopened);
    finish(&mut reopened);
    assert!(
        reopened.projects.error.is_none(),
        "a missing plugin must not fail the open: {:?}",
        reopened.projects.error
    );
    let document = reopened.live.document.clone().unwrap();
    assert_eq!(document.plugin, info.plugin);
    assert!(!reopened.live.mounted(), "missing bundle must not run");
    let reason = reopened.live.disabled_reason.clone().unwrap();
    assert!(reason.contains(INSTRUMENT), "{reason}");
    assert!(reason.contains(&info.plugin.version), "{reason}");
    assert!(
        reason.contains(&info.plugin.artifact.to_string()[..12]),
        "{reason}"
    );
    assert!(
        reason.contains("CLAP"),
        "the backend must be named: {reason}"
    );
    assert!(!reason.contains(&bundle.display().to_string()), "{reason}");
    assert_eq!(reopened.live.takes.len(), 1);
    let slots = reopened.live.takes[&TakeId(9)];
    assert_ne!(slots.left, slots.right);
    let loaded_left = reopened.assets.slots().get(slots.left).unwrap();
    let loaded_right = reopened.assets.slots().get(slots.right).unwrap();
    assert_eq!(loaded_left.data.as_ref(), clip.left.as_ref());
    assert_eq!(loaded_right.data.as_ref(), clip.right.as_ref());
    assert_eq!(loaded_left.rate, RATE);

    // The retained take plays from its own stereo slots, without a pad meter.
    reopened.now = 1.0;
    transport::play(&mut reopened);
    transport::poll(&mut reopened);
    reopened.output.advance(0.02);
    let peak = f32::from_bits(reopened.output.meters.peak.load(Relaxed));
    assert!(peak > 0.0, "retained take did not play");
    assert_eq!(reopened.output.meters.counts(), [0; 16]);
    transport::stop(&mut reopened, "test");
}

/// A pass whose instrument is stopped mid-recording still commits its MIDI
/// take, and the take says its live audio was not retained instead of
/// dropping the loss silently.
#[test]
fn a_take_committed_after_the_instrument_stopped_reports_the_missing_audio() {
    let Some((helper, fixture)) = env() else {
        return;
    };
    let root = Root::new();
    let bundle = root.0.join("pinned-instrument.clap");
    fs::copy(&fixture, &bundle).unwrap();
    let mut s = state();
    s.live.set_helper(helper.clone());
    let info = identify_fixture(&helper, &bundle);
    s.live
        .adopt(
            Chosen {
                info,
                path: bundle.clone(),
            },
            RATE,
        )
        .unwrap();
    let stage = s.live.stage_from_document().unwrap();
    let _voice = s.live.mount(stage, RATE, 256).unwrap();

    s.now = 0.25;
    transport::record(&mut s);
    assert!(
        s.live.capture.is_some(),
        "REC starts retaining the live audio"
    );
    s.now = 0.26;
    s.trigger(4, 1.0);
    s.now = 0.27;
    s.trigger(4, 0.3);

    // The panel's CLEAR INSTRUMENT drops the live capture during the pass.
    clear(&mut s);
    assert!(s.live.capture.is_none());
    assert!(
        s.live
            .notice
            .as_deref()
            .unwrap_or_default()
            .contains("cleared"),
        "{:?}",
        s.live.notice
    );

    transport::stop(&mut s, "Stopped");
    let take = s.takes.selected().expect("the pass still commits its take");
    let notes = s.takes.take(take).map_or(0, |pass| pass.notes().len());
    assert!(notes > 0, "the committed take kept its MIDI notes");
    let notice = s.live.notice.clone().expect("the loss is reported");
    assert!(notice.contains(&take.0.to_string()), "{notice}");
    assert!(notice.contains("live audio was not retained"), "{notice}");
    assert!(notice.contains("stopped during the pass"), "{notice}");
    assert!(
        !notice.contains("failed"),
        "a user CLEAR is not a device failure: {notice}"
    );
    assert!(s.live.error.is_none(), "{:?}", s.live.error);
    assert!(
        s.live.for_save().unwrap().unwrap().takes.is_empty(),
        "no audio was published for the take"
    );
}

/// The host can answer that it kept nothing for an active capture (a capture
/// that already stopped inside the helper). The take still says the audio was
/// not retained rather than returning silently.
#[test]
fn a_take_whose_host_kept_no_capture_reports_the_missing_audio() {
    let Some((helper, fixture)) = env() else {
        return;
    };
    let root = Root::new();
    let bundle = root.0.join("pinned-instrument.clap");
    fs::copy(&fixture, &bundle).unwrap();
    let mut s = state();
    s.live.set_helper(helper.clone());
    let info = identify_fixture(&helper, &bundle);
    s.live
        .adopt(
            Chosen {
                info,
                path: bundle.clone(),
            },
            RATE,
        )
        .unwrap();
    let stage = s.live.stage_from_document().unwrap();
    let mut voice = s.live.mount(stage, RATE, 256).unwrap();

    let pass_start = 5_000_000_000;
    let pass_end = pass_start + 100_000_000;
    s.live.begin_capture(TakeId(9), pass_start, pass_start);
    let base = s.live.session.as_ref().unwrap().sender.earliest_exact();
    send(&mut s, &[note(5, base + 64, true)]);
    {
        let session = s.live.session.as_ref().unwrap();
        play_lockstep(&session.host, &mut voice, 1_024);
    }
    // The helper already stopped keeping the capture: the UI's request then
    // returns `Ok(None)`.
    assert!(
        s.live
            .session
            .as_ref()
            .unwrap()
            .host
            .capture(false)
            .unwrap()
            .is_some()
    );

    let media = s.media.owned.clone();
    s.live
        .finish_capture(media.as_ref(), TakeId(9), pass_start, pass_end);
    let notice = s.live.notice.clone().expect("the loss is reported");
    assert!(notice.contains("9"), "{notice}");
    assert!(notice.contains("live audio was not retained"), "{notice}");
    assert!(notice.contains("kept no capture"), "{notice}");
    assert!(s.live.error.is_none(), "{:?}", s.live.error);
    assert!(s.live.for_save().unwrap().unwrap().takes.is_empty());
}

/// A pass captured for one take is not reported as the audio of another.
#[test]
fn a_take_finished_without_its_capture_reports_which_capture_was_running() {
    let Some((helper, fixture)) = env() else {
        return;
    };
    let root = Root::new();
    let bundle = root.0.join("pinned-instrument.clap");
    fs::copy(&fixture, &bundle).unwrap();
    let mut s = state();
    s.live.set_helper(helper.clone());
    let info = identify_fixture(&helper, &bundle);
    s.live
        .adopt(
            Chosen {
                info,
                path: bundle.clone(),
            },
            RATE,
        )
        .unwrap();
    let stage = s.live.stage_from_document().unwrap();
    let _voice = s.live.mount(stage, RATE, 256).unwrap();

    s.live
        .begin_capture(TakeId(4), 1_000_000_000, 1_000_000_000);
    assert_eq!(
        s.live.capture.as_ref().map(|capture| capture.take),
        Some(TakeId(4))
    );
    let media = s.media.owned.clone();
    s.live
        .finish_capture(media.as_ref(), TakeId(5), 1_000_000_000, 2_000_000_000);
    let notice = s.live.notice.clone().expect("the loss is reported");
    assert!(notice.contains("Take 5"), "{notice}");
    assert!(notice.contains("live audio was not retained"), "{notice}");
    assert!(notice.contains("belonged to take 4"), "{notice}");
    assert!(s.live.for_save().unwrap().unwrap().takes.is_empty());
}

fn panel_view(state: &State, _cx: &mut Cx<State>) -> El {
    rune::col().child(header(state)).child(panel(state))
}

fn key(rt: &mut Runtime<State>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(0.0);
    rt.frame(0.001);
}

fn focus(rt: &mut Runtime<State>, target: &str) {
    for _ in 0..120 {
        if rt
            .elements()
            .iter()
            .any(|element| element.key.as_deref() == Some(target) && element.focused)
        {
            return;
        }
        key(rt, Key::Tab);
    }
    panic!("Tab never reached {target}");
}

/// The panel names the instrument, its status reason and the bundled path/id
/// controls, and clears the instrument without touching the retained audio.
#[test]
fn the_panel_shows_the_instrument_status_and_clears_it() {
    let mut s = state();
    let stage = Stage {
        plugin: Plugin {
            backend: brownnote_native::pedals::offline::Backend::Clap,
            id: INSTRUMENT.into(),
            artifact: Hash256::of(b"panel fixture"),
            abi: "clap-1.2".into(),
            version: "0.4.0".into(),
        },
        location: PathBuf::from("/machine/fixture.clap"),
        params: Vec::new(),
        state: None,
    };
    s.live.document = Some(LiveInstrumentDocument::from_stage(
        &stage,
        "Fixture Instrument".into(),
        true,
    ));
    s.live.takes.insert(
        TakeId(9),
        RetainedTake {
            asset: AssetId::of(b"retained"),
            left: 0,
            right: 1,
            rate: RATE,
            frames: 4_800,
        },
    );
    s.live.disabled_reason = Some(format!(
        "{} · missing bundle",
        s.live.document.as_ref().unwrap().identity_line()
    ));
    s.live.visible = true;
    let mut rt = Runtime::new(s, panel_view, (820.0, 600.0), Env::new(true, 2.0));
    rt.set_full_keyboard_access(true);
    rt.frame(0.0);

    for (key, role) in [
        ("live-panel", "group"),
        ("live-status", "status"),
        ("live-path", "textbox"),
        ("live-plugin-id", "textbox"),
        ("live-choose", "button"),
        ("live-clear", "button"),
    ] {
        let element = rt
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("missing element {key}"));
        assert_eq!(element.role, role, "{key}");
    }
    let status = rt
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("live-status"))
        .unwrap();
    let status_text = status.text.as_deref().unwrap_or("");
    assert!(status_text.contains(INSTRUMENT), "{status_text}");
    assert!(status_text.contains("missing bundle"), "{status_text}");

    focus(&mut rt, "live-clear");
    key(&mut rt, Key::Text(" ".into()));
    let document = rt.state.live.document.as_ref().unwrap();
    assert!(!document.enabled);
    assert_eq!(document.plugin.id, INSTRUMENT);
    assert!(
        rt.state
            .live
            .disabled_reason
            .as_deref()
            .unwrap()
            .contains("cleared in this project")
    );
    assert_eq!(
        rt.state.live.takes[&TakeId(9)].left,
        0,
        "clearing the plugin must keep retained takes"
    );
    assert!(
        rt.state
            .live
            .status_line()
            .contains("cleared in this project")
    );
    assert_eq!(
        rt.state.live.path.text, "",
        "clearing leaves no path to reuse silently"
    );
}
