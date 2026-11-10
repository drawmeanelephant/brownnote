#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use brownnote_native::{
    engine::{Command, Sample},
    import::{MediaPublisher, retain},
    instrument::{INSTRUMENT, Preset, describe},
    kit::{self, Kit},
    pedals::{
        offline::{Backend, Hash256, Plugin, Stage as OfflineStage},
        render::{Chain, Stage},
    },
    project::{
        Asset, AssetRef, MediaKind, SampleAssignment,
        live::{LiveInstrumentDocument, LiveTakeDocument},
        store::{CommitState, SaveFailure, Store, StoreError},
    },
};
use rune::{Env, Key, KeyEvent, Mods, Runtime};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "brownnote-project-controls-{}-{}",
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
fn encoded(value: f32, frames: usize) -> Vec<u8> {
    kit::encode_wav(&Sample {
        data: vec![value; frames].into(),
        rate: 8_000,
    })
    .unwrap()
}
fn state() -> State {
    let mut state = State::new(
        Kit::from_encoded(std::array::from_fn(|i| {
            Ok(encoded(0.1 + i as f32 * 0.01, 80_000))
        })),
        true,
        true,
    );
    state.pedals.auto_load = false;
    // Tests never resolve the real machine recovery directory. A test that
    // expects a copy points this at its own temporary root.
    state.projects.recovery_base = Some(std::env::temp_dir().join(format!(
        "brownnote-project-recovery-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Relaxed)
    )));
    state
}

/// A prompt autosave for tests: the real idle interval is a minute.
fn autosave_enabled(s: &mut State, base: &std::path::Path) {
    fs::create_dir_all(base).unwrap();
    s.projects.recovery_base = Some(base.to_path_buf());
    s.projects.autosave_after = Duration::ZERO;
    s.projects.check_every = Duration::ZERO;
}

/// Poll until the recovery write, if any, has left the worker.
fn autosaved(s: &mut State) {
    let deadline = Instant::now() + Duration::from_secs(10);
    poll(s);
    while s.projects.autosave_job.is_some() && Instant::now() < deadline {
        poll(s);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        s.projects.autosave_job.is_none(),
        "autosave worker timed out"
    );
}
fn finish(s: &mut State) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.projects.busy() && Instant::now() < deadline {
        poll(s);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!s.projects.busy(), "project worker timed out");
}
fn set_path(s: &mut State, path: &std::path::Path) {
    s.projects.path = TextEdit::new(path.to_string_lossy());
}
fn saved(s: &mut State, path: &std::path::Path) {
    set_path(s, path);
    save(s);
    finish(s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
}
fn playing(s: &mut State) -> Arc<[f32]> {
    s.trigger(1, 1.0);
    s.output.advance(0.01);
    assert!(s.output.meters.counts()[1] > 0);
    s.assets
        .audible(session::pad_id(1))
        .unwrap()
        .imported
        .sample
        .data
        .clone()
}
fn unchanged_playing(s: &mut State, old: &Arc<[f32]>) {
    assert!(Arc::ptr_eq(
        old,
        &s.assets
            .audible(session::pad_id(1))
            .unwrap()
            .imported
            .sample
            .data
    ));
    assert!(
        s.output.meters.counts()[1] > 0,
        "failed operation stopped old voices"
    );
    s.trigger(2, 1.0);
    s.output.advance(0.01);
    assert!(
        s.output.meters.counts()[2] > 0,
        "unrelated pad stopped working"
    );
}
fn custom(s: &mut State, root: &Root) -> PathBuf {
    let source = root.0.join("my-custom.wav");
    fs::write(&source, encoded(0.4, 80_000)).unwrap();
    s.import_sample(0, Some(source.clone())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.media.busy() && Instant::now() < deadline {
        media::poll(s);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!s.media.busy());
    source
}
fn retained(s: &mut State, frames: usize) {
    let imported = retain(
        Sample {
            data: vec![0.25; frames].into(),
            rate: 8_000,
        },
        "retained custom".into(),
        s.media.owned.as_ref(),
    )
    .unwrap();
    let ticket = s.assets.begin(session::pad_id(0));
    s.assets
        .replace_retained(ticket, &imported, |command| s.output.send(command))
        .unwrap();
    s.pedals.pads[0].chain = Chain {
        stages: vec![Stage {
            effect: "Tape Echo".into(),
            pots: vec![("Feedback".into(), 60)],
            mix: Some(40),
        }],
    };
    s.pedals.pads[0].status = pedals::Status::Ready {
        seconds: frames as f32 / 8_000.0,
        clipped: false,
    };
    let chain = s.pedals.pads[0].chain.clone();
    s.pedals.pads[0].restore_project_chain(chain, Some(frames as f32 / 8_000.0));
}

#[test]
fn project_controls_save_reopen_relocate_custom_and_retained_full_kit() {
    let root = Root::new();
    let mut s = state();
    let source = custom(&mut s, &root);
    retained(&mut s, 8_000 * 35); // A legitimate retained tail, not a D7 import.
    for (index, setting) in s.settings.iter_mut().enumerate() {
        setting.gain = 0.5 + index as f32 / 32.0;
        setting.pan = index as f32 / 8.0 - 1.0;
        setting.pitch = index as f32 - 8.0;
    }
    s.master = 0.37;
    s.choke = false;
    let expected = snapshot(&s).unwrap().document;
    let project = root.0.join("saved");
    saved(&mut s, &project);
    let moved = root.0.join("relocated");
    fs::rename(&project, &moved).unwrap();
    fs::remove_file(source).unwrap();
    s.settings[0].gain = 0.01;
    s.master = 0.9;
    let old_owned = s.media.owned.clone();
    let _ = playing(&mut s);
    set_path(&mut s, &moved);
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert_eq!(snapshot(&s).unwrap().document, expected);
    assert!(!Arc::ptr_eq(&s.media.owned, &old_owned));
    assert_eq!(s.output.meters.counts(), [0; 16]);
    assert_eq!(s.pad_audio(0), (35.0, 8_000));
    assert_eq!(
        s.assets
            .audible(session::pad_id(0))
            .unwrap()
            .imported
            .sample
            .data[0],
        0.25
    );
    assert_eq!(
        s.source_wave(0),
        &kit::waveform(&Sample {
            data: vec![0.4; 80_000].into(),
            rate: 8_000
        })
    );
    // Save-as again after deleting the entire opened project proves in-memory
    // ownership, not a later path/cache/source reread.
    fs::remove_dir_all(&moved).unwrap();
    saved(&mut s, &root.0.join("saved-again"));
    s.trigger(0, 1.0);
    s.output.advance(0.01);
    assert!(s.output.meters.counts()[0] > 0);
}

#[test]
fn project_controls_missing_corrupt_media_keep_active_kit_voices_and_other_pads() {
    for missing in [true, false] {
        let root = Root::new();
        let project = root.0.join("saved");
        let mut s = state();
        saved(&mut s, &project);
        let reference = snapshot(&s).unwrap().document.assets[0].reference.clone();
        let path = project.join(reference.id.relative_path());
        if missing {
            fs::remove_file(path).unwrap();
        } else {
            fs::write(path, vec![0; reference.bytes as usize]).unwrap();
        }
        let before = snapshot(&s).unwrap().document;
        let voice = playing(&mut s);
        open(&mut s);
        finish(&mut s);
        assert!(s.projects.error.is_some());
        assert_eq!(snapshot(&s).unwrap().document, before);
        unchanged_playing(&mut s, &voice);
    }
}

#[test]
fn project_controls_hash_valid_bad_retained_decode_never_falls_back_to_source() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    retained(&mut s, 80_001);
    let mut doc = snapshot(&s).unwrap().document;
    let bytes = b"hash-valid but not a WAV";
    let reference = AssetRef::of(bytes);
    doc.assets.retain(|a| a.kind != MediaKind::RetainedRender);
    doc.assets.push(Asset {
        reference: reference.clone(),
        kind: MediaKind::RetainedRender,
    });
    doc.pads[0].sample.as_mut().unwrap().retained = Some(reference.id);
    let owned = Arc::new(brownnote_native::import::SessionMedia::default());
    s.media.owned.adopt(owned.as_ref()).unwrap();
    owned.publish(&reference, bytes).unwrap();
    let service = Service::new().unwrap();
    let job = service
        .start(Request::Save {
            root: project.clone(),
            new_root: false,
            snapshot: Box::new(Snapshot {
                document: doc,
                media: owned,
                freeze: None,
            }),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = job.take() {
            assert!(result.is_ok());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = snapshot(&s).unwrap().document;
    let voice = playing(&mut s);
    set_path(&mut s, &project);
    open(&mut s);
    finish(&mut s);
    assert!(
        s.projects
            .error
            .as_deref()
            .unwrap()
            .contains("Cannot decode")
    );
    assert_eq!(snapshot(&s).unwrap().document, before);
    unchanged_playing(&mut s, &voice);
}

#[test]
fn project_controls_output_and_queue_admission_failure_are_atomic() {
    for full in [false, true] {
        let root = Root::new();
        let project = root.0.join("saved");
        let mut s = state();
        saved(&mut s, &project);
        let before = snapshot(&s).unwrap().document;
        let voice = playing(&mut s);
        s.projects.open_output = Some(Rc::new(move |slots, _| {
            if !full {
                return Err("injected output refusal".into());
            }
            let mut candidate = Output::virtual_driver(slots, "candidate".into(), false);
            // Leave exactly one admission slot: master succeeds, choke fails.
            for _ in 0..brownnote_native::engine::QUEUE_CAPACITY - 1 {
                assert!(candidate.send(Command::Panic));
            }
            Ok(candidate)
        }));
        open(&mut s);
        finish(&mut s);
        assert!(
            s.projects
                .error
                .as_deref()
                .unwrap()
                .contains("not admitted")
        );
        assert_eq!(snapshot(&s).unwrap().document, before);
        unchanged_playing(&mut s, &voice);
    }
}

#[test]
fn project_controls_failed_save_preserves_old_generation_and_active_operations() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let before = Store::new(project.clone())
        .inspect_committed()
        .unwrap()
        .unwrap();
    s.master = 0.2;
    let current = snapshot(&s).unwrap().document;
    // A corrupt immutable destination causes publisher rejection, not overwrite.
    let extra = AssetRef::of(b"extra immutable media");
    s.media
        .owned
        .publish(&extra, b"extra immutable media")
        .unwrap();
    fs::write(project.join(extra.id.relative_path()), b"corrupt").unwrap();
    let voice = playing(&mut s);
    let ticket = s.assets.begin(session::pad_id(9));
    save(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_some());
    assert!(s.assets.current(ticket), "save invalidated unrelated work");
    assert_eq!(snapshot(&s).unwrap().document, current);
    assert_eq!(
        Store::new(project.clone())
            .inspect_committed()
            .unwrap()
            .unwrap(),
        before
    );
    unchanged_playing(&mut s, &voice);
    fs::write(project.join("project.json"), b"corrupt current").unwrap();
    save(&mut s);
    finish(&mut s);
    assert_eq!(
        fs::read(project.join("project.json")).unwrap(),
        b"corrupt current"
    );
    assert!(s.projects.error.as_deref().unwrap().contains("recovery"));
}

#[test]
fn project_controls_snapshot_overlaps_cancellation_and_stale_open_completion() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    let expected = snapshot(&s).unwrap().document;
    set_path(&mut s, &project);
    save(&mut s);
    s.master = 0.23; // Captured save must not drift to this later edit.
    open(&mut s); // Busy, never replaces/drops the in-flight save.
    assert!(s.projects.busy());
    assert!(s.projects.error.is_some());
    cancel(&mut s); // An admitted save is not cancellable.
    assert!(s.projects.busy());
    finish(&mut s);
    assert_eq!(
        Store::new(project.clone())
            .inspect_committed()
            .unwrap()
            .unwrap()
            .document,
        expected
    );
    let voice = playing(&mut s);
    open(&mut s);
    cancel(&mut s);
    assert!(!s.projects.busy());
    // A new receiver, late cancelled completion cannot reach it. A cancelled
    // request can still occupy the worker's one waiting slot briefly.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.projects.busy() {
        open(&mut s);
        assert!(Instant::now() < deadline);
        if !s.projects.busy() {
            assert!(s.projects.error.as_deref().unwrap().contains("queued"));
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    s.settings[3].gain = 0.123;
    finish(&mut s);
    assert!(
        s.projects
            .error
            .as_deref()
            .unwrap()
            .contains("changed while opening")
    );
    assert_eq!(s.settings[3].gain, 0.123);
    unchanged_playing(&mut s, &voice);
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert_eq!(snapshot(&s).unwrap().document, expected);
}

#[test]
fn project_controls_recovery_requires_choice_and_saves_to_another_root() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let expected = snapshot(&s).unwrap().document;
    let generation = Store::new(project.clone())
        .inspect_committed()
        .unwrap()
        .unwrap()
        .generation;
    s.master = 0.32;
    saved(&mut s, &project);
    fs::write(project.join("project.json"), b"corrupt").unwrap();
    recover(&mut s);
    finish(&mut s);
    assert_eq!(s.master, 0.32, "listing silently applied recovery");
    assert!(
        s.projects
            .recovery
            .as_ref()
            .unwrap()
            .1
            .recoverable
            .contains(&generation)
    );
    open_choice(&mut s, project.clone(), OpenChoice::Committed(generation));
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert_eq!(snapshot(&s).unwrap().document, expected);
    save(&mut s);
    assert!(
        s.projects
            .error
            .as_deref()
            .unwrap()
            .contains("different project")
    );
    assert_eq!(fs::read(project.join("project.json")).unwrap(), b"corrupt");
    set_path(&mut s, &root.0.join("saved/../saved"));
    save(&mut s);
    finish(&mut s);
    assert!(s.projects.error.as_deref().unwrap().contains("nonexistent"));
    assert_eq!(fs::read(project.join("project.json")).unwrap(), b"corrupt");
    saved(&mut s, &root.0.join("rescued"));
    assert_eq!(fs::read(project.join("project.json")).unwrap(), b"corrupt");
}

#[test]
fn project_controls_ready_stale_chain_is_not_a_saveable_render() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    retained(&mut s, 80_001);
    saved(&mut s, &project);
    let committed = fs::read(project.join("project.json")).unwrap();
    let original = s.pedals.pads[0].chain.clone();
    s.pedals.pads[0].chain.stages[0].mix = Some(99);
    assert!(matches!(
        s.pedals.pads[0].status,
        pedals::Status::Ready { .. }
    ));
    save(&mut s);
    assert!(!s.projects.busy());
    assert!(s.projects.error.is_some());
    assert_eq!(fs::read(project.join("project.json")).unwrap(), committed);
    s.pedals.pads[0].chain = original;
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none());
    // Removing the final stage without a catalog must not persist the old
    // retained sound under an empty chain, even if Ready is still displayed.
    s.pedals.pads[0].chain.stages.clear();
    save(&mut s);
    assert!(!s.projects.busy());
    assert!(s.projects.error.is_some());
    assert_eq!(fs::read(project.join("project.json")).unwrap(), committed);
}

#[test]
fn project_controls_reject_pending_work_and_relative_paths_without_io() {
    let root = Root::new();
    let mut s = state();
    set_path(&mut s, &root.0.join("must-not-exist"));
    s.pedals.pads[0].status = pedals::Status::Rendering;
    let ticket = s.assets.begin(session::pad_id(0));
    save(&mut s);
    assert!(!s.projects.busy());
    open(&mut s);
    assert!(!s.projects.busy());
    assert!(s.assets.current(ticket));
    assert!(!root.0.join("must-not-exist").exists());
    s.pedals.pads[0].status = pedals::Status::Base;
    s.projects.path = TextEdit::new("relative");
    save(&mut s);
    assert!(s.projects.error.as_deref().unwrap().contains("absolute"));
    assert!(!s.projects.busy());
}

#[test]
fn project_controls_runtime_buttons_path_entry_and_shortcuts_use_real_callers() {
    let root = Root::new();
    let project = root.0.join("from controls ");
    let mut rt = Runtime::new(
        state(),
        crate::ui::view,
        (820.0, 600.0),
        Env::new(true, 2.0),
    );
    rt.set_full_keyboard_access(true);
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.frame(0.01);
    control_press(&mut rt, "projects-toggle");
    assert!(rt.state.projects.visible);
    assert!(rt.find("projects-path").is_some());
    control_focus(&mut rt, "projects-path");
    control_key(&mut rt, Key::Text(project.to_str().unwrap().into()));
    assert_eq!(rt.state.projects.path.text, project.to_str().unwrap());
    control_key(&mut rt, Key::Escape);
    assert!(
        !rt.state.projects.visible,
        "Escape in the path field did not close projects"
    );
    control_press(&mut rt, "projects-toggle");
    assert_eq!(rt.state.projects.path.text, project.to_str().unwrap());
    control_press(&mut rt, "projects-save");
    finish_runtime(&mut rt);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    assert!(project.join("project.json").is_file());
    assert!(
        !root.0.join("from controls").exists(),
        "path whitespace was stripped"
    );
    control_key(&mut rt, Key::Text("1".into()));
    assert_eq!(
        rt.state.output.meters.counts(),
        [0; 16],
        "path entry played a pad"
    );
    rt.state.master = 0.11;
    control_press(&mut rt, "projects-open");
    finish_runtime(&mut rt);
    assert!(rt.state.projects.error.is_none());
    assert_eq!(rt.state.master, 0.8);
    control_key(&mut rt, Key::Escape);
    assert!(!rt.state.projects.visible);
    control_key(&mut rt, Key::Text("q".into()));
    assert_eq!(rt.state.counts[4], 1, "performance keys did not resume");
}

fn control_key(rt: &mut Runtime<State>, key: Key) {
    rt.key(key.into());
    rt.frame(0.0);
    rt.frame(0.001);
}

fn control_focus(rt: &mut Runtime<State>, target: &str) {
    for _ in 0..32 {
        if rt
            .elements()
            .iter()
            .any(|el| el.key.as_deref() == Some(target) && el.focused)
        {
            return;
        }
        control_key(rt, Key::Tab);
    }
    panic!(
        "Tab did not reach {target}: busy={}, visible={}, focusable={:?}",
        rt.state.projects.busy(),
        rt.state.projects.visible,
        rt.elements()
            .iter()
            .filter(|el| el.focusable || el.focused)
            .map(|el| (el.key.as_deref(), el.focused))
            .collect::<Vec<_>>()
    );
}

fn control_press(rt: &mut Runtime<State>, target: &str) {
    control_focus(rt, target);
    control_key(rt, Key::Text(" ".into()));
}

fn finish_runtime(rt: &mut Runtime<State>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while rt.state.projects.busy() && Instant::now() < deadline {
        rt.frame(0.001);
        std::thread::yield_now();
    }
    assert!(
        !rt.state.projects.busy(),
        "mounted project worker timed out"
    );
    rt.frame(0.0);
    rt.frame(0.01);
}

#[test]
fn project_controls_real_view_recovery_requires_a_keyboard_choice() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let generation = Store::new(project.clone())
        .inspect_committed()
        .unwrap()
        .unwrap()
        .generation;
    s.master = 0.32;
    saved(&mut s, &project);
    fs::write(project.join("project.json"), b"corrupt").unwrap();
    let mut rt = Runtime::new(s, crate::ui::view, (820.0, 600.0), Env::new(true, 2.0));
    rt.set_full_keyboard_access(true);
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.frame(0.01);
    control_press(&mut rt, "projects-toggle");
    control_press(&mut rt, "projects-recover");
    finish_runtime(&mut rt);
    assert_eq!(
        rt.state.master, 0.32,
        "listing applied recovery without a choice"
    );
    control_press(&mut rt, &format!("projects-generation-{generation}"));
    finish_runtime(&mut rt);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    assert_eq!(rt.state.master, 0.8);
    assert_eq!(fs::read(project.join("project.json")).unwrap(), b"corrupt");
    control_press(&mut rt, "projects-save");
    assert!(
        rt.state.projects.error.is_some(),
        "recovery overwrote its original root"
    );
    control_focus(&mut rt, "projects-path");
    rt.key(KeyEvent {
        key: Key::Command('a'),
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
    });
    rt.frame(0.001);
    let rescued = root.0.join("rescued");
    control_key(&mut rt, Key::Text(rescued.to_str().unwrap().into()));
    control_press(&mut rt, "projects-save");
    finish_runtime(&mut rt);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    assert!(rescued.join("project.json").is_file());
    assert_eq!(fs::read(project.join("project.json")).unwrap(), b"corrupt");
}

#[test]
fn project_controls_keyboard_import_save_fresh_state_and_relocate() {
    let root = Root::new();
    let source = root.0.join("picked sample.wav");
    let bytes = encoded(0.4, 80_000);
    let expected_asset = AssetRef::of(&bytes);
    fs::write(&source, bytes).unwrap();
    let mut rt = Runtime::new(
        state(),
        crate::ui::view,
        (820.0, 600.0),
        Env::new(true, 2.0),
    );
    rt.set_full_keyboard_access(true);
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.frame(0.01);
    control_press(&mut rt, "import-toggle");
    control_focus(&mut rt, "import-path");
    control_key(&mut rt, Key::Text(source.to_str().unwrap().into()));
    control_key(&mut rt, Key::Enter);
    let deadline = Instant::now() + Duration::from_secs(10);
    while rt.state.media.busy() && Instant::now() < deadline {
        rt.frame(0.001);
        std::thread::yield_now();
    }
    assert!(!rt.state.media.busy(), "mounted import worker timed out");
    assert_eq!(
        rt.state
            .assets
            .source(session::pad_id(0))
            .unwrap()
            .imported
            .asset
            .reference,
        expected_asset
    );
    control_key(&mut rt, Key::Escape);
    control_focus(&mut rt, "gain");
    for _ in 0..3 {
        control_key(&mut rt, Key::Left);
    }
    assert!((rt.state.settings[0].gain - 0.85).abs() < 0.00001);
    let saved = root.0.join("Original kit");
    control_press(&mut rt, "projects-toggle");
    control_focus(&mut rt, "projects-path");
    control_key(&mut rt, Key::Text(saved.to_str().unwrap().into()));
    control_press(&mut rt, "projects-save");
    finish_runtime(&mut rt);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    drop(rt);
    let moved = root.0.join("Relocated kit");
    fs::rename(saved, &moved).unwrap();
    fs::remove_file(source).unwrap();
    let mut rt = Runtime::new(
        state(),
        crate::ui::view,
        (820.0, 600.0),
        Env::new(true, 2.0),
    );
    rt.set_full_keyboard_access(true);
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.frame(0.01);
    assert_eq!(rt.state.settings[0].gain, 1.0);
    control_press(&mut rt, "projects-toggle");
    control_focus(&mut rt, "projects-path");
    control_key(&mut rt, Key::Text(moved.to_str().unwrap().into()));
    control_press(&mut rt, "projects-open");
    finish_runtime(&mut rt);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    assert!((rt.state.settings[0].gain - 0.85).abs() < 0.00001);
    let sample = &rt.state.assets.source(session::pad_id(0)).unwrap().imported;
    assert_eq!(sample.asset.reference, expected_asset);
    assert_eq!(sample.sample.data[0], 0.4);
    control_key(&mut rt, Key::Escape);
    control_key(&mut rt, Key::Text("1".into()));
    assert_eq!(rt.state.counts[0], 1);
    assert!(rt.state.peak > 0.0);
}

#[test]
fn project_controls_take_keyboard_priority_over_an_open_import_panel() {
    let mut s = state();
    s.projects.visible = true;
    s.media.controls.visible = true;
    let mut rt = Runtime::new(s, crate::ui::view, (820.0, 600.0), Env::new(true, 2.0));
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.frame(0.01);
    assert!(rt.find("projects-panel").is_some());
    assert!(rt.find("import-panel").is_none());
    rt.key(KeyEvent {
        key: Key::Command('i'),
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
    });
    rt.frame(0.001);
    assert!(
        rt.state.media.controls.visible,
        "project key changed a hidden import panel"
    );
}

#[test]
fn project_controls_decodes_pcm16_stereo_and_rejects_capacity_before_swap() {
    let root = Root::new();
    let project = root.0.join("pcm16");
    let mut bytes = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut bytes,
            hound::WavSpec {
                channels: 2,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample(16384_i16).unwrap();
        writer.write_sample(0_i16).unwrap();
        writer.finalize().unwrap();
    }
    let encoded = bytes.into_inner();
    let reference = AssetRef::of(&encoded);
    let owned = Arc::new(brownnote_native::import::SessionMedia::default());
    owned.publish(&reference, &encoded).unwrap();
    let mut document = Document::default();
    document.assets.push(Asset {
        reference: reference.clone(),
        kind: MediaKind::Source,
    });
    document.pads[15].sample = Some(SampleAssignment {
        source: reference.id,
        retained: None,
    });
    fs::create_dir(&project).unwrap();
    owned
        .adopt(&brownnote_native::import::ProjectMedia::new(
            project.clone(),
        ))
        .unwrap();
    Store::new(project.clone()).save(&document, &[]).unwrap();
    let mut s = state();
    set_path(&mut s, &project);
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none());
    assert_eq!(
        s.assets
            .audible(session::pad_id(15))
            .unwrap()
            .imported
            .sample
            .data
            .as_ref(),
        &[0.25]
    );
    assert!(s.assets.playback(session::pad_id(0)).is_none());
    // More declared objects than runtime slots, even if unassigned, is an
    // explicit runtime-admission error. Never partially apply a playable subset.
    for index in 0..brownnote_native::engine::SLOTS {
        let bytes = super::tests::encoded(index as f32 / 1000.0, 2);
        let reference = AssetRef::of(&bytes);
        owned.publish(&reference, &bytes).unwrap();
        document.assets.push(Asset {
            reference,
            kind: MediaKind::Source,
        });
    }
    owned
        .adopt(&brownnote_native::import::ProjectMedia::new(
            project.clone(),
        ))
        .unwrap();
    Store::new(project.clone()).save(&document, &[]).unwrap();
    let before = snapshot(&s).unwrap().document;
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.as_deref().unwrap().contains("capacity"));
    assert_eq!(snapshot(&s).unwrap().document, before);
}

#[test]
fn project_controls_open_clears_old_held_notes_only_after_success() {
    let root = Root::new();
    let project = root.0.join("next project");
    let mut s = state();
    saved(&mut s, &project);
    let midi = s.midi.clone();
    midi::fixture(
        &midi,
        brownnote_native::performance::SourceId(19),
        r#"{"version":1,"mappings":[
            {"type":"note","channel":0,"pitch":60,"target":{"type":"held","instrument":"1"}}
        ]}"#,
        &[(0, &[0x90, 60, 100]), (100_000, &[0x90, 60, 90])],
    );
    s.now = 1.0;
    midi::poll(&mut s, &midi);
    assert!(midi::status(&s, &midi).contains("held 2 active"));
    s.now = 2.0;

    set_path(&mut s, &root.0.join("missing"));
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_some());
    assert!(
        midi::status(&s, &midi).contains("held 2 active"),
        "failed project open must not release the active kit's notes"
    );

    set_path(&mut s, &project);
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    let status = midi::status(&s, &midi);
    assert!(status.contains("held 0 active"), "{status}");
    assert!(
        status.starts_with("CONNECTED") && status.contains("source 19"),
        "{status}"
    );
    assert!(midi::mappings(&midi).unwrap().entries().is_empty());
}

#[test]
fn project_controls_save_and_reopen_recorded_takes_edits_and_mappings_then_replay() {
    let root = Root::new();
    let project = root.0.join("takes");
    let mut s = state();
    transport::cycle_loop(&mut s);
    transport::record(&mut s);
    s.now = 0.25;
    s.trigger(4, 1.0);
    s.now = 0.27;
    s.trigger(4, 0.3);
    assert!(snapshot(&s).is_err(), "a recording pass never saves");
    transport::stop(&mut s, "Stopped");
    let take = s.takes.selected().unwrap();
    s.edit.model.toggle_quantization(&s.takes, take).unwrap();
    let mappings: brownnote_native::midi::input::MappingsDocument = serde_json::from_str(
        r#"{"version":1,"mappings":[{"type":"note","channel":1,"pitch":40,"target":{"type":"one_shot","pad":4}}]}"#,
    )
    .unwrap();
    midi::set_mappings(&mut s, mappings.to_mappings().unwrap());
    let expected = snapshot(&s).unwrap().document;
    assert!(expected.takes.is_some() && expected.edits.is_some());
    assert!(expected.midi_mappings.is_some());
    saved(&mut s, &project);

    // Diverge after saving: another take, no edits, no mappings.
    transport::record(&mut s);
    s.trigger(1, 1.0);
    transport::stop(&mut s, "Stopped");
    s.edit = edit::Editor::default();
    midi::set_mappings(&mut s, Document::default().mappings().unwrap());
    assert_ne!(snapshot(&s).unwrap().document, expected);

    set_path(&mut s, &project);
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert_eq!(snapshot(&s).unwrap().document, expected);
    assert!(!s.transport.active());
    let notes = s.takes.take(take).unwrap().notes();
    assert_eq!(notes.len(), 2);
    assert_eq!(
        notes[1].on().original_time.nanos() - notes[0].on().original_time.nanos(),
        20_000_000
    );
    assert_eq!(notes[1].on().velocity.value(), 38);

    // The reopened take replays through the scheduler.
    transport::play(&mut s);
    for _ in 0..300 {
        s.now += 0.01;
        s.output.advance(0.01);
        transport::poll(&mut s);
    }
    assert!(s.output.schedule_counters().dispatched >= 2);
    assert_eq!(s.transport.counters.schedule_lost, 0);
}

fn recovery_root(s: &State, project: &std::path::Path) -> PathBuf {
    session::recovery_copy_root(s.projects.recovery_base.as_ref().unwrap(), project).unwrap()
}

fn copy_document(s: &State, project: &std::path::Path) -> Option<Document> {
    let root = recovery_root(s, project);
    if !root.join("project.json").is_file() {
        return None;
    }
    Store::new(root)
        .inspect_committed()
        .unwrap()
        .map(|copy| copy.document)
}

fn copy_played(s: &mut State, project: &std::path::Path) -> Option<Document> {
    autosaved(s);
    copy_document(s, project)
}

fn project_files(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn project_controls_autosave_writes_one_separate_recovery_copy_after_idle() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let committed = fs::read(project.join("project.json")).unwrap();
    let before = project_files(&project);
    let recovery = root.0.join("recovery");
    autosave_enabled(&mut s, &recovery);

    // A clean, idle project produces no recovery copy at all.
    autosaved(&mut s);
    assert!(
        !recovery_root(&s, &project).exists(),
        "a clean project must not produce a copy"
    );

    s.settings[3].gain = 0.123;
    s.master = 0.44;
    let expected = snapshot(&s).unwrap().document;
    assert_eq!(copy_played(&mut s, &project), Some(expected.clone()));
    assert!(s.projects.dirty, "a copy is not a save");
    assert!(
        s.projects.autosave_status.contains("Recovery copy"),
        "{}",
        s.projects.autosave_status
    );

    // The copy lives in the machine root, never in the saved project.
    let copy_root = recovery_root(&s, &project);
    assert!(copy_root.starts_with(&recovery));
    assert_ne!(copy_root, project);
    assert_eq!(fs::read(project.join("project.json")).unwrap(), committed);
    assert_eq!(project_files(&project), before);
    assert_eq!(s.settings[3].gain, 0.123, "the active kit kept the edit");
}

/// Every dirty source is captured by the same comparison: settings, chains and
/// retained media, imported samples, MIDI mappings, takes/edits, and the
/// instrument and live-take records.
#[test]
fn project_controls_autosave_covers_every_dirty_source_and_clean_idle_writes_none() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let recovery = root.0.join("recovery");
    autosave_enabled(&mut s, &recovery);
    autosaved(&mut s);
    assert_eq!(
        copy_document(&s, &project),
        None,
        "clean project wrote a copy"
    );

    let copied = |s: &mut State, label: &str| {
        let expected = snapshot(s)
            .unwrap_or_else(|error| panic!("{label}: {error}"))
            .document;
        assert_eq!(
            copy_played(s, &project),
            Some(expected),
            "{label} was not captured by autosave"
        );
        assert!(s.projects.dirty, "{label}");
    };

    // Pad settings and master.
    s.settings[5].gain = 0.33;
    s.settings[5].pan = -0.5;
    s.master = 0.61;
    copied(&mut s, "pad settings");

    // An imported sample.
    custom(&mut s, &root);
    copied(&mut s, "imported media");

    // A chain with its retained render and media.
    retained(&mut s, 8_000);
    copied(&mut s, "chain and retained media");

    // MIDI mappings.
    let mappings: brownnote_native::midi::input::MappingsDocument = serde_json::from_str(
        r#"{"version":1,"mappings":[{"type":"note","channel":0,"pitch":60,"target":{"type":"one_shot","pad":2}}]}"#,
    )
    .unwrap();
    midi::set_mappings(&mut s, mappings.to_mappings().unwrap());
    copied(&mut s, "MIDI mappings");

    // A committed take and its edit layer.
    transport::record(&mut s);
    s.trigger(4, 1.0);
    transport::stop(&mut s, "Stopped");
    let take = s.takes.selected().unwrap();
    s.edit.model.toggle_quantization(&s.takes, take).unwrap();
    copied(&mut s, "takes and edits");

    // The instrument record and its exact source bytes.
    let source = b"fixture instrument bytes".to_vec();
    let reference = AssetRef::of(&source);
    s.media.owned.publish(&reference, &source).unwrap();
    s.instrument.document = Some(describe(
        INSTRUMENT,
        "Fixture instrument",
        reference.id,
        Preset {
            bank: 0,
            program: 0,
        },
        0.5,
    ));
    copied(&mut s, "instrument record");

    // A live-instrument record with its take entry.
    let stage = live_stage("org.brownnote.fixture.autosave");
    let mut live = LiveInstrumentDocument::from_stage(&stage, "Fixture".into(), false);
    let clip = b"fixture live take bytes".to_vec();
    let clip_reference = AssetRef::of(&clip);
    s.media.owned.publish(&clip_reference, &clip).unwrap();
    live.takes.push(LiveTakeDocument {
        take: "7".into(),
        asset: clip_reference.id,
        rate: 48_000,
        frames: 4_800,
    });
    s.live.document = Some(live);
    copied(&mut s, "live instrument record");
}

fn live_stage(id: &str) -> OfflineStage {
    OfflineStage {
        plugin: Plugin {
            backend: Backend::Clap,
            id: id.into(),
            artifact: Hash256::of(id.as_bytes()),
            abi: "clap-1.2".into(),
            version: "1.0.0".into(),
        },
        location: PathBuf::from("/machine/fixture.clap"),
        params: Vec::new(),
        state: None,
    }
}

#[test]
fn project_controls_autosave_skips_armed_busy_pending_and_unrendered_ticks() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    autosave_enabled(&mut s, &root.0.join("recovery"));

    // An armed pass holds tentative capture that must never be saved (D6).
    transport::record(&mut s);
    s.settings[0].gain = 0.51;
    autosaved(&mut s);
    assert_eq!(copy_document(&s, &project), None);
    assert!(
        s.projects
            .autosave_status
            .to_lowercase()
            .contains("skipped"),
        "{}",
        s.projects.autosave_status
    );
    assert!(
        s.projects.autosave_status.contains("recording"),
        "{}",
        s.projects.autosave_status
    );
    transport::stop(&mut s, "Stopped");

    // A busy pedal render blocks the capture.
    s.pedals.pads[1].status = pedals::Status::Rendering;
    s.settings[1].gain = 0.4;
    autosaved(&mut s);
    assert_eq!(copy_document(&s, &project), None);
    assert!(
        s.projects.autosave_status.contains("skipped"),
        "{}",
        s.projects.autosave_status
    );
    s.pedals.pads[1].status = pedals::Status::Base;

    // A chain edited after its render is not a saveable sound.
    s.pedals.pads[0].restore_project_chain(
        Chain {
            stages: vec![Stage {
                effect: "Tape Echo".into(),
                pots: Vec::new(),
                mix: None,
            }],
        },
        Some(1.0),
    );
    s.pedals.pads[0].chain.stages[0].mix = Some(60);
    assert!(matches!(
        s.pedals.pads[0].status,
        pedals::Status::Ready { .. }
    ));
    autosaved(&mut s);
    assert_eq!(copy_document(&s, &project), None);
    assert!(
        s.projects.autosave_status.contains("skipped"),
        "{}",
        s.projects.autosave_status
    );
    s.pedals.pads[0].chain.stages.clear();
    s.pedals.pads[0].restore_project_chain(Chain::default(), None);

    // An in-flight project operation skips the tick without writing.
    let job = s
        .projects
        .service
        .as_ref()
        .unwrap()
        .start(Request::Recovery {
            root: project.clone(),
        })
        .unwrap();
    s.projects.pending = Some(Pending {
        root: project.clone(),
        kind: Kind::Recovery,
        job,
    });
    s.settings[2].gain = 0.2;
    autosaved(&mut s);
    assert_eq!(copy_document(&s, &project), None);
    assert!(
        s.projects.autosave_status.contains("operation is running"),
        "{}",
        s.projects.autosave_status
    );
    finish(&mut s);
}

#[test]
fn project_controls_autosave_skips_over_capacity_projects() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    autosave_enabled(&mut s, &root.0.join("recovery"));
    let mut document =
        LiveInstrumentDocument::from_stage(&live_stage("capacity"), "Over".into(), false);
    for index in 0..30u8 {
        document.takes.push(LiveTakeDocument {
            take: index.to_string(),
            asset: brownnote_native::project::AssetId::of(&[index]),
            rate: 48_000,
            frames: 4,
        });
    }
    s.live.document = Some(document);
    autosaved(&mut s);
    assert_eq!(copy_document(&s, &project), None);
    assert!(
        s.projects.autosave_status.contains("capacity"),
        "{}",
        s.projects.autosave_status
    );
}

#[test]
fn project_controls_failed_autosave_keeps_the_previous_copy_and_reports_commit_state() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let committed = fs::read(project.join("project.json")).unwrap();
    autosave_enabled(&mut s, &root.0.join("recovery"));
    s.settings[4].gain = 0.9;
    let previous = copy_played(&mut s, &project).unwrap();

    // Break only the copy store's writer lock; the saved project is untouched.
    let copy_root = recovery_root(&s, &project);
    let lock = copy_root.join(".brownnote-store/lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    s.settings[4].gain = 0.95;
    autosaved(&mut s);
    assert!(
        s.projects.autosave_status.contains("unchanged"),
        "{}",
        s.projects.autosave_status
    );
    assert!(!s.projects.autosave_status.contains("Recovery copy"));
    assert_eq!(copy_document(&s, &project), Some(previous.clone()));
    assert_eq!(fs::read(project.join("project.json")).unwrap(), committed);

    // The store's distinctions stay distinct rather than becoming success.
    let generation = s.projects.saved.as_ref().unwrap().generation;
    for (commit, needle) in [
        (CommitState::Unchanged, "unchanged"),
        (CommitState::Unknown(generation), "may have changed"),
        (
            CommitState::PublishedNotDurable(generation),
            "not proven durable",
        ),
    ] {
        let error = Error::Save(Box::new(SaveFailure {
            commit,
            error: Box::new(StoreError::Io(std::io::ErrorKind::Other)),
        }));
        let status = autosave_failure(&error);
        assert!(status.contains(needle), "{status}");
    }
}

#[test]
fn project_controls_open_offers_a_newer_recovery_copy_without_applying_it() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let saved_document = snapshot(&s).unwrap().document;
    let committed = fs::read(project.join("project.json")).unwrap();
    autosave_enabled(&mut s, &root.0.join("recovery"));
    s.settings[6].gain = 0.77;
    let recovery_document = snapshot(&s).unwrap().document;
    assert_eq!(
        copy_played(&mut s, &project),
        Some(recovery_document.clone())
    );

    // Reopening the saved project offers the copy and applies nothing.
    set_path(&mut s, &project);
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert_eq!(snapshot(&s).unwrap().document, saved_document);
    let copy = s.projects.recovery_copy.clone().unwrap();
    assert!(copy.offered.is_some(), "{}", copy.note);
    assert!(s.projects.recovery_note.contains("offered"));
    assert_eq!(fs::read(project.join("project.json")).unwrap(), committed);

    // Selecting it opens through the explicit recovery flow.
    let generation = copy.offered.unwrap();
    open_choice(&mut s, copy.root.clone(), OpenChoice::Committed(generation));
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert_eq!(
        s.projects.recovered_from.as_deref(),
        Some(copy.root.as_path())
    );
    assert_eq!(snapshot(&s).unwrap().document, recovery_document);

    // Recovered content only saves to a new, nonexistent directory.
    set_path(&mut s, &project);
    save(&mut s);
    finish(&mut s);
    assert_eq!(
        s.projects.error.as_deref(),
        Some("Save recovered content to a new, nonexistent project directory.")
    );
    let fresh = root.0.join("rescued");
    saved(&mut s, &fresh);
    assert_eq!(
        Store::new(fresh)
            .inspect_committed()
            .unwrap()
            .unwrap()
            .document,
        recovery_document
    );
}

#[test]
fn project_controls_recovery_copy_with_missing_media_is_rejected_not_offered() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    let saved_document = snapshot(&s).unwrap().document;
    autosave_enabled(&mut s, &root.0.join("recovery"));
    s.settings[8].gain = 0.19;
    let copy = copy_played(&mut s, &project).unwrap();
    let copy_root = recovery_root(&s, &project);
    let reference = copy.assets[0].reference.clone();
    fs::remove_file(copy_root.join(reference.id.relative_path())).unwrap();

    set_path(&mut s, &project);
    open(&mut s);
    finish(&mut s);
    assert!(s.projects.error.is_none(), "{:?}", s.projects.error);
    assert_eq!(snapshot(&s).unwrap().document, saved_document);
    let report = s.projects.recovery_copy.as_ref().unwrap();
    assert!(report.offered.is_none());
    assert!(report.options.current.is_err());
    assert!(
        s.projects.recovery_note.contains("not offered"),
        "{}",
        s.projects.recovery_note
    );
}

#[test]
fn project_controls_cancel_and_close_drop_the_pending_autosave_without_a_partial_file() {
    let root = Root::new();
    let project = root.0.join("saved");
    let mut s = state();
    saved(&mut s, &project);
    autosave_enabled(&mut s, &root.0.join("recovery"));

    s.settings[7].gain = 0.55;
    poll(&mut s);
    assert!(s.projects.autosave_job.is_some());
    cancel(&mut s);
    assert!(s.projects.autosave_job.is_none());
    assert!(
        s.projects.autosave_status.contains("dropped"),
        "{}",
        s.projects.autosave_status
    );
    assert_eq!(s.settings[7].gain, 0.55, "the active kit is unchanged");

    // Whatever the worker did, only a complete generation may exist.
    std::thread::sleep(Duration::from_millis(200));
    let copy_root = recovery_root(&s, &project);
    if copy_root.join("project.json").is_file() {
        let copy = Store::new(copy_root).open(OpenChoice::Current).unwrap();
        assert_eq!(copy.document(), &snapshot(&s).unwrap().document);
    }

    // Leaving the panel drops the next pending write too.
    s.settings[7].gain = 0.66;
    poll(&mut s);
    assert!(s.projects.autosave_job.is_some());
    s.projects.visible = true;
    close(&mut s);
    assert!(!s.projects.visible);
    assert!(s.projects.autosave_job.is_none());
    assert!(
        s.projects.autosave_status.contains("dropped"),
        "{}",
        s.projects.autosave_status
    );
    assert_eq!(s.settings[7].gain, 0.66);
}
