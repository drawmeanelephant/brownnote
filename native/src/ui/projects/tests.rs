#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use brownnote_native::{
    engine::{Command, Sample},
    import::{MediaPublisher, retain},
    kit::{self, Kit},
    pedals::render::{Chain, Stage},
    project::{Asset, AssetRef, MediaKind, SampleAssignment, store::Store},
};
use rune::{Env, Key, Runtime};
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
    state
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
        s.projects.open_output = Rc::new(move |slots, _| {
            if !full {
                return Err("injected output refusal".into());
            }
            let mut candidate = Output::virtual_driver(slots, "candidate".into(), false);
            // Leave exactly one admission slot: master succeeds, choke fails.
            for _ in 0..brownnote_native::engine::QUEUE_CAPACITY - 1 {
                assert!(candidate.send(Command::Panic));
            }
            Ok(candidate)
        });
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
    let mut s = state();
    s.projects.visible = true;
    set_path(&mut s, &root.0.join("from-controls"));
    let mut rt = Runtime::new(s, |s, _| panel(s), (1120.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.frame(0.01);
    assert!(rt.find("projects-path").is_some());
    let (x, y) = rt.find("projects-save").unwrap();
    rt.pointer_down(x, y);
    rt.frame(0.001);
    rt.pointer_up(x, y);
    rt.frame(0.001);
    finish(&mut rt.state);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    assert!(root.0.join("from-controls/project.json").is_file());
    rt.key(Key::Text("1".into()).into());
    rt.frame(0.01);
    assert_eq!(
        rt.state.output.meters.counts(),
        [0; 16],
        "path entry played a pad"
    );
    rt.state.master = 0.11;
    let (x, y) = rt.find("projects-open").unwrap();
    rt.pointer_down(x, y);
    rt.frame(0.001);
    rt.pointer_up(x, y);
    rt.frame(0.001);
    finish(&mut rt.state);
    assert!(rt.state.projects.error.is_none());
    assert_eq!(rt.state.master, 0.8);
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
