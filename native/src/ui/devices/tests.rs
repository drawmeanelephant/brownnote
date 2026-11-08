#![allow(clippy::unwrap_used)]
use super::*;
use rune::{Env, Key, KeyEvent, Mods, Runtime};
use std::{
    cell::RefCell,
    fs,
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

fn id(value: &str) -> DeviceId {
    DeviceId::new(cpal::default_host().id(), value)
}

fn discovery() -> Discovery {
    let range = cpal::SupportedStreamConfigRange::new(
        2,
        44_100,
        48_000,
        cpal::SupportedBufferSize::Range { min: 64, max: 1024 },
        cpal::SampleFormat::F32,
    );
    Discovery {
        default_id: Some(id("fixture-b")),
        devices: ["fixture-a", "fixture-b"]
            .map(|name| DeviceInfo {
                id: id(name),
                name: "Duplicate output".into(),
                default_config: range.with_sample_rate(44_100),
                supported_configs: vec![range],
            })
            .into(),
    }
}

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "brownnote-output-{}-{}",
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

type Calls = Rc<RefCell<Vec<OutputSelection>>>;

fn state() -> (State, Calls) {
    let kit = crate::ui_test_support::kit(0.25, 44_100, 44_100);
    let mut s = State::new(kit, true, true);
    s.pedals.auto_load = false;
    s.devices = Devices::disabled();
    s.devices.visible = true;
    s.devices.discovery = Some(discovery());
    s.devices.discover = Rc::new(|| Ok(discovery()));
    let calls = Rc::new(RefCell::new(Vec::new()));
    let recorded = calls.clone();
    s.devices.open = Rc::new(move |_, selection| {
        recorded.borrow_mut().push(selection.clone());
        Err("Fixture output disconnected".into())
    });
    (s, calls)
}

fn finish(s: &mut State) {
    assert!(poll(s), "stop phase changes the visible state");
    assert!(status(s).starts_with("RETRYING"));
    assert!(s.output.opened().is_none());
    assert!(poll(s), "open phase changes the visible state");
    assert!(!s.devices.busy());
    assert!(!poll(s), "idle must not spin");
}

#[test]
fn duplicate_names_select_ids_and_reopen_requested_44100_then_48000() {
    let (mut s, calls) = state();
    s.devices.select_device(Some(id("fixture-b")));
    for rate in [44_100, 48_000] {
        s.devices.rate = TextEdit::new(rate.to_string());
        s.devices.buffer = TextEdit::new("256");
        apply(&mut s);
        finish(&mut s);
        assert_eq!(s.output.selection().device_id, Some(id("fixture-b")));
        assert_eq!(s.output.selection().sample_rate, Some(rate));
        assert_eq!(s.output.selection().buffer_size, BufferSize::Fixed(256));
        assert!(s.output.failed);
        assert!(status(&s).contains("SILENT"));
        assert!(status(&s).contains("Fixture output disconnected"));
    }
    let requests = calls.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].sample_rate, Some(44_100));
    assert_eq!(requests[1].sample_rate, Some(48_000));
}

#[test]
fn invalid_configuration_does_not_stop_voices_change_identity_or_save() {
    let root = Root::new();
    let (mut s, calls) = state();
    s.devices.root = Some(root.0.clone());
    s.trigger(0, 1.0);
    s.output.advance(0.01);
    let initial = s.output.selection().clone();
    for (rate, channels, buffer) in [
        ("wrong", "", ""),
        ("96000", "", ""),
        ("48000", "1", ""),
        ("48000", "2", "0"),
        ("48000", "2", "1025"),
        ("0", "2", "64"),
    ] {
        s.devices.rate = TextEdit::new(rate);
        s.devices.channels = TextEdit::new(channels);
        s.devices.buffer = TextEdit::new(buffer);
        apply(&mut s);
        assert!(s.devices.error.is_some(), "{rate}, {channels}, {buffer}");
        assert!(!s.devices.busy());
        assert_eq!(s.output.selection(), &initial);
        assert!(s.output.meters.counts()[0] > 0);
        assert!(!root.0.join("audio-output.json").exists());
    }
    assert!(calls.borrow().is_empty());
}

#[test]
fn unknown_buffer_bounds_allow_only_default_and_validate_whole_tuple() {
    let (mut s, calls) = state();
    let mut d = discovery();
    let range = cpal::SupportedStreamConfigRange::new(
        2,
        44_100,
        48_000,
        cpal::SupportedBufferSize::Unknown,
        cpal::SampleFormat::F32,
    );
    d.devices[1].supported_configs = vec![range];
    s.devices.discovery = Some(d);
    s.devices.buffer = TextEdit::new("128");
    apply(&mut s);
    assert!(s.devices.error.is_some());
    assert!(!s.devices.busy());
    s.devices.buffer = TextEdit::new("");
    apply(&mut s);
    finish(&mut s);
    assert_eq!(calls.borrow()[0].buffer_size, BufferSize::Default);
}

#[test]
fn missing_id_is_clear_and_retry_ignores_unsaved_edits_until_explicit_fallback() {
    let (mut s, calls) = state();
    let missing = OutputSelection {
        device_id: Some(id("stale-id")),
        sample_rate: Some(48_000),
        ..Default::default()
    };
    s.stop_for_output_change(missing.clone(), "Device lost".into());
    s.devices.set_draft(missing.clone());
    apply(&mut s);
    assert!(s.devices.error.as_deref().unwrap().contains("unavailable"));
    assert!(calls.borrow().is_empty());
    s.devices.select_device(Some(id("fixture-a")));
    retry(&mut s);
    finish(&mut s);
    assert_eq!(calls.borrow()[0], missing);
    assert_eq!(s.output.selection(), &missing);
    system_default(&mut s);
    finish(&mut s);
    assert_eq!(calls.borrow()[1], OutputSelection::default());
    assert_eq!(s.output.selection(), &OutputSelection::default());
}

#[test]
fn loss_while_voices_run_stops_them_and_preserves_prune_and_master_choke_state() {
    let (mut s, _) = state();
    let selection = OutputSelection {
        device_id: Some(id("fixture-a")),
        ..Default::default()
    };
    s.install_output(Output::silent_selected(
        s.assets.slots().clone(),
        selection.clone(),
        "fixture".into(),
    ));
    s.master = 0.0;
    s.choke = false;
    let retired_slot = crate::ui_test_support::retire_one(&mut s, 15);
    s.trigger(0, 1.0);
    s.output.advance(0.01);
    s.counts = s.output.meters.counts();
    s.peak = 0.5;
    assert!(s.counts[0] > 0);
    s.recover_audio_error("Fixture stream lost".into());
    assert_eq!(s.output.selection(), &selection);
    assert_eq!(s.counts, [0; 16]);
    assert_eq!(s.peak, 0.0);
    assert_eq!(s.output.meters.counts(), [0; 16]);
    s.output.advance(0.01);
    s.poll_assets();
    assert!(s.assets.slots().is_empty(retired_slot));
    assert!(status(&s).contains("Fixture stream lost"));
    assert!(status(&s).contains("voices stopped"));
    // SetMaster ramps over the first block, as it does for existing callers.
    s.output.advance(0.01);
    for _ in 0..2 {
        s.trigger(0, 1.0);
    }
    s.output.advance(0.01);
    assert_eq!(s.output.meters.counts()[0], 2, "choke restored off");
    assert_eq!(
        s.output.meters.peak.load(Relaxed),
        0,
        "master restored at zero"
    );
}

#[test]
fn partial_success_never_claims_running_and_repeated_apply_is_not_queued_twice() {
    let (mut s, calls) = state();
    let recorded = calls.clone();
    s.devices.open = Rc::new(move |slots, selection| {
        recorded.borrow_mut().push(selection.clone());
        Ok(Output::silent_selected(
            slots,
            selection.clone(),
            "Stream did not start".into(),
        ))
    });
    apply(&mut s);
    apply(&mut s);
    assert!(poll(&mut s));
    s.trigger(0, 1.0);
    s.output.advance(0.01);
    assert!(s.output.meters.counts()[0] > 0);
    system_default(&mut s);
    assert!(poll(&mut s));
    assert_eq!(calls.borrow().len(), 1);
    assert!(status(&s).starts_with("SILENT"));
    assert!(!status(&s).contains("RUNNING"));
    assert_eq!(s.output.meters.counts(), [0; 16]);
}

#[test]
fn preferences_survive_failed_reopen_and_a_new_feature_instance() {
    let root = Root::new();
    let (mut s, calls) = state();
    s.devices.root = Some(root.0.clone());
    s.devices.select_device(Some(id("fixture-a")));
    s.devices.rate = TextEdit::new("48000");
    s.devices.buffer = TextEdit::new("256");
    apply(&mut s);
    finish(&mut s);
    let remembered = s.output.selection().clone();
    let mut reopened = Devices::load(Ok(root.0.clone()));
    reopened.discover = Rc::new(|| Ok(discovery()));
    reopened.open = s.devices.open.clone();
    s.output = reopened.initial_output(s.assets.slots().clone(), "test".into());
    s.devices = reopened;
    finish(&mut s);
    assert_eq!(calls.borrow().len(), 2);
    assert_eq!(s.output.selection(), &remembered);
    assert_eq!(preferences::load(&root.0).unwrap(), remembered);
    assert!(s.devices.preference_error.is_none());
}

#[test]
fn corrupt_invalid_version_and_invalid_ids_stay_silent_without_overwriting_file() {
    let root = Root::new();
    let (mut s, calls) = state();
    for content in [
        "{broken",
        r#"{"version":99,"device_id":null,"sample_rate":null,"channels":null,"buffer_frames":null}"#,
        r#"{"version":1,"device_id":"invalid","sample_rate":null,"channels":null,"buffer_frames":null}"#,
        r#"{"version":1,"device_id":null,"sample_rate":0,"channels":null,"buffer_frames":null}"#,
    ] {
        let path = root.0.join("audio-output.json");
        fs::write(&path, content).unwrap();
        let mut loaded = Devices::load(Ok(root.0.clone()));
        assert!(loaded.preference_error.is_some());
        assert!(!loaded.busy());
        loaded.discover = Rc::new(|| Ok(discovery()));
        loaded.open = s.devices.open.clone();
        s.output = loaded.initial_output(s.assets.slots().clone(), "test".into());
        s.devices = loaded;
        poll(&mut s);
        assert!(status(&s).starts_with("SILENT"));
        assert!(calls.borrow().is_empty());
        assert_eq!(fs::read_to_string(path).unwrap(), content);
    }
    system_default(&mut s);
    finish(&mut s);
    assert!(s.devices.preference_error.is_none());
    assert_eq!(
        preferences::load(&root.0).unwrap(),
        OutputSelection::default()
    );
}

#[test]
fn failed_preference_save_does_not_hide_stream_failure_or_forget_retry_selection() {
    let root = Root::new();
    let blocked = root.0.join("not-a-directory");
    fs::write(&blocked, "owned fixture").unwrap();
    let (mut s, calls) = state();
    s.devices.root = Some(blocked);
    s.devices.select_device(Some(id("fixture-a")));
    apply(&mut s);
    finish(&mut s);
    assert!(s.devices.preference_error.is_some());
    assert!(status(&s).contains("disconnected"));
    retry(&mut s);
    finish(&mut s);
    assert_eq!(calls.borrow()[0], calls.borrow()[1]);
}

#[test]
fn discovery_failure_preserves_voices_and_saved_selection_until_deliberate_retry() {
    let (mut s, calls) = state();
    s.trigger(0, 1.0);
    s.output.advance(0.01);
    s.devices.discover = Rc::new(|| Err("Fixture discovery failure".into()));
    refresh(&mut s);
    assert!(poll(&mut s));
    assert!(s.devices.discovery.is_none());
    assert!(
        s.devices
            .error
            .as_deref()
            .unwrap()
            .contains("discovery failure")
    );
    assert!(s.output.meters.counts()[0] > 0);
    apply(&mut s);
    assert!(
        s.devices
            .error
            .as_deref()
            .unwrap()
            .contains("Refresh outputs")
    );
    assert!(calls.borrow().is_empty());
    s.devices.discover = Rc::new(|| Ok(discovery()));
    refresh(&mut s);
    poll(&mut s);
    assert!(s.devices.error.is_none());
    assert!(s.output.meters.counts()[0] > 0);
    apply(&mut s);
    finish(&mut s);
    assert_eq!(calls.borrow()[0], OutputSelection::default());
}

#[test]
fn startup_retries_a_stale_saved_id_even_if_system_default_is_present() {
    let root = Root::new();
    let (mut s, calls) = state();
    let selection = OutputSelection {
        device_id: Some(id("absent-saved-id")),
        sample_rate: Some(48_000),
        ..Default::default()
    };
    preferences::save(&root.0, &selection).unwrap();
    let mut loaded = Devices::load(Ok(root.0.clone()));
    loaded.discover = Rc::new(|| Ok(discovery()));
    loaded.open = s.devices.open.clone();
    s.output = loaded.initial_output(s.assets.slots().clone(), "test".into());
    s.devices = loaded;
    finish(&mut s);
    assert_eq!(calls.borrow()[0], selection);
    assert_eq!(s.output.selection(), &selection);
    assert!(status(&s).starts_with("SILENT"));
}

#[test]
fn disabled_session_cannot_open_hardware_even_after_explicit_ui_actions() {
    let (mut s, _) = state();
    s.devices = Devices::disabled();
    refresh(&mut s);
    poll(&mut s);
    assert!(s.devices.discovery.is_none());
    assert!(s.devices.error.as_deref().unwrap().contains("disabled"));
    system_default(&mut s);
    finish(&mut s);
    assert!(s.output.opened().is_none());
    assert!(status(&s).contains("disabled"));
    assert!(s.devices.root.is_none());
}

#[test]
fn missing_preference_file_starts_with_device_defaults_without_writing_a_file() {
    let root = Root::new();
    let (mut s, calls) = state();
    let mut loaded = Devices::load(Ok(root.0.clone()));
    assert!(loaded.preference_error.is_none());
    assert_eq!(loaded.draft, OutputSelection::default());
    loaded.discover = Rc::new(|| Ok(discovery()));
    loaded.open = s.devices.open.clone();
    s.output = loaded.initial_output(s.assets.slots().clone(), "test".into());
    s.devices = loaded;
    finish(&mut s);
    assert_eq!(calls.borrow()[0], OutputSelection::default());
    assert!(!root.0.join("audio-output.json").exists());
}

#[test]
fn missing_preference_root_waits_for_explicit_fallback_without_disabling_audio() {
    let (mut s, calls) = state();
    let mut loaded = Devices::load(Err("Machine preference root is unavailable".into()));
    assert!(loaded.root.is_none());
    assert!(!loaded.busy());
    loaded.discover = Rc::new(|| Ok(discovery()));
    loaded.open = s.devices.open.clone();
    s.output = loaded.initial_output(s.assets.slots().clone(), "unused disabled reason".into());
    s.devices = loaded;
    poll(&mut s);
    assert!(calls.borrow().is_empty());
    assert!(status(&s).starts_with("SILENT"));
    assert!(s.devices.preference_error.is_some());
    system_default(&mut s);
    finish(&mut s);
    assert_eq!(calls.borrow().as_slice(), &[OutputSelection::default()]);
    assert!(s.devices.preference_error.is_some());
    assert!(s.devices.root.is_none());
}

fn rt() -> Runtime<State> {
    let (s, _) = state();
    let mut rt = Runtime::new(s, crate::ui::view, (820.0, 600.0), Env::new(true, 2.0));
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.set_full_keyboard_access(true);
    rt.frame(0.0);
    rt
}

fn key(rt: &mut Runtime<State>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(0.0);
}

fn focus(rt: &mut Runtime<State>, target: &str) {
    for _ in 0..60 {
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

#[test]
fn tab_keyboard_device_navigation_text_edit_roles_and_status_work_in_real_view() {
    let mut rt = rt();
    for (key, role) in [
        ("output-rate", "textbox"),
        ("output-device-0", "radio"),
        ("output-status", "status"),
        ("output-apply", "button"),
    ] {
        let element = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some(key))
            .unwrap();
        assert_eq!(element.role, role);
    }
    focus(&mut rt, "output-device-default");
    key(&mut rt, Key::Down);
    assert_eq!(rt.state.devices.draft.device_id, Some(id("fixture-a")));
    key(&mut rt, Key::Down);
    assert_eq!(rt.state.devices.draft.device_id, Some(id("fixture-b")));
    focus(&mut rt, "output-rate");
    key(&mut rt, Key::Text("48000".into()));
    assert_eq!(rt.state.devices.rate.text, "48000");
    focus(&mut rt, "output-buffer");
    key(&mut rt, Key::Text("256".into()));
    assert_eq!(rt.state.devices.buffer.text, "256");
    focus(&mut rt, "output-apply");
    key(&mut rt, Key::Text(" ".into()));
    assert!(status(&rt.state).starts_with("RETRYING"));
    rt.frame(0.01);
    assert!(status(&rt.state).contains("silent while opening"));
    let legacy_retry = rt
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("retry-audio"))
        .unwrap();
    assert!(
        !legacy_retry.focusable,
        "legacy retry must not interrupt an opening"
    );
    rt.frame(0.01);
    assert!(status(&rt.state).starts_with("SILENT"));
    for target in [
        "output-retry",
        "output-fallback",
        "output-refresh",
        "output-toggle",
    ] {
        focus(&mut rt, target);
    }
    key(&mut rt, Key::Text(" ".into()));
    assert!(!rt.state.devices.visible);
}

#[test]
fn scenes_and_minimum_size_expose_status_errors_and_keep_controls_in_bounds() {
    for kind in [SceneKind::Selection, SceneKind::Missing, SceneKind::Invalid] {
        let mut rt = rt();
        scene(&mut rt.state, kind);
        rt.frame(0.0);
        assert!(rt.find("output-status").is_some());
        for target in [
            "output-refresh",
            "output-rate",
            "output-apply",
            "output-fallback",
            "output-toggle",
        ] {
            focus(&mut rt, target);
            let [x, y, w, h] = rt.presented(target).unwrap().0;
            assert!(
                x >= 0.0 && y >= 0.0 && x + w <= 820.1 && y + h <= 600.1,
                "{target}: {x}, {y}, {w}, {h}"
            );
            assert!(w > 0.0 && h > 0.0);
        }
    }
}
