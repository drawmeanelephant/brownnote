//! Mounted service/runtime behavior, using real encoded media and output queues.
use crate::{
    ui::{self, State},
    ui_test_support::{self as fixture, pad, slot},
};
use brownnote_native::{
    assets::RegistryError,
    engine::{Command, Returned, SLOTS, Sample},
    import::{ImportService, ProjectMedia, StartError},
    kit::{self, Kit},
    project::{self, Document},
};
use rune::{Env, Key, Runtime};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "brownnote-mounted-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn wav(&self, name: &str, value: f32, frames: usize) -> PathBuf {
        let path = self.0.join(name);
        fs::write(
            &path,
            kit::encode_wav(&Sample {
                data: vec![value; frames].into(),
                rate: 8_000,
            })
            .unwrap(),
        )
        .unwrap();
        path
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runtime() -> Runtime<State> {
    let mut state = State::new(fixture::kit(0.1, 8_000, 80_000), true, true);
    state.pedals.auto_load = false;
    state.master = 1.0;
    state.choke = false;
    state.output.send(Command::SetMaster(1.0));
    state.output.send(Command::SetChoke(false));
    for settings in &mut state.settings {
        settings.pitch = 0.0;
    }
    let mut rt = Runtime::new(state, ui::view, (1120.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.01);
    rt
}

fn finish(rt: &mut Runtime<State>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while rt.state.media.busy() && Instant::now() < deadline {
        rt.frame(0.001);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!rt.state.media.busy(), "mounted import deadline");
    rt.frame(0.001);
}

#[test]
fn mounted_import_preserves_old_voice_and_settings_but_next_key_and_pointer_use_new_media() {
    let root = Root::new();
    let path = root.wav("custom.wav", 0.4, 80_000);
    let expected = project::AssetRef::of(&fs::read(&path).unwrap());
    let mut rt = runtime();
    let before = slot(&rt.state, 0);
    let shared = slot(&rt.state, 1);
    let original = rt.state.assets.slots().get(before).unwrap().data.clone();
    rt.key(Key::Text("1".into()).into());
    rt.frame(0.0);
    rt.frame(0.01);
    assert_eq!(rt.state.counts[0], 1);
    rt.state.settings[0].pan = 0.0;
    rt.state.import_sample(0, Some(path)).unwrap();
    finish(&mut rt);
    assert_eq!(
        rt.state
            .assets
            .source(pad(0))
            .unwrap()
            .imported
            .asset
            .reference,
        expected
    );
    assert_eq!(rt.state.pad_name(0), "custom.wav");
    assert_eq!(rt.state.pad_audio(0), (10.0, 8_000));
    assert_eq!(rt.state.settings[0].gain, 1.0);
    assert_eq!(rt.state.counts[0], 1, "old voice was cut");
    assert_ne!(slot(&rt.state, 0), before);
    assert_eq!(slot(&rt.state, 1), shared, "shared pad changed");
    assert!(Arc::ptr_eq(
        &original,
        &rt.state.assets.slots().get(before).unwrap().data
    ));
    rt.state.output.meters.peak.store(0, Relaxed);
    rt.key(Key::Text("1".into()).into());
    rt.frame(0.0);
    rt.frame(0.01);
    assert_eq!(rt.state.counts[0], 2);
    assert!(
        (rt.state.peak - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.001,
        "actual mix did not combine old 0.1 and new 0.4: {}",
        rt.state.peak
    );
    let (x, y) = rt.find("pad-0").unwrap();
    rt.pointer_down(x, y);
    rt.frame(0.01);
    rt.pointer_up(x, y);
    rt.frame(0.001);
    assert_eq!(rt.state.counts[0], 3);
    let label = rt
        .elements()
        .into_iter()
        .find(|el| el.key.as_deref() == Some("pad-0"))
        .unwrap()
        .label;
    assert!(label.as_deref().unwrap_or_default().contains("custom.wav"));
}

#[test]
fn mounted_invalid_and_cancelled_imports_keep_current_sound_and_other_pads_playable() {
    let root = Root::new();
    let corrupt = root.0.join("corrupt.wav");
    fs::write(&corrupt, b"not a wav").unwrap();
    let mut rt = runtime();
    let before = slot(&rt.state, 0);
    rt.state.import_sample(0, Some(corrupt)).unwrap();
    finish(&mut rt);
    assert!(rt.state.pad_error(0).unwrap().contains("corrupt.wav"));
    assert_eq!(slot(&rt.state, 0), before);
    rt.key(Key::Text("1".into()).into());
    rt.key(Key::Text("q".into()).into());
    rt.frame(0.0);
    rt.frame(0.01);
    assert_eq!(rt.state.counts[0], 1);
    assert_eq!(rt.state.counts[4], 1);
    rt.state
        .import_sample(0, Some(root.wav("cancel.wav", 0.7, 8_000)))
        .unwrap();
    rt.state.import_sample(0, None).unwrap();
    finish(&mut rt);
    assert_eq!(slot(&rt.state, 0), before);
}

#[test]
fn mounted_queue_refusal_and_full_slots_keep_assignment_until_retry() {
    let root = Root::new();
    let mut rt = runtime();
    let before = slot(&rt.state, 0);
    while rt.state.output.send(Command::SetChoke(false)) {}
    rt.state
        .import_sample(0, Some(root.wav("queue.wav", 0.4, 80_000)))
        .unwrap();
    // Poll completion without advancing the output, so its queue stays full.
    let deadline = Instant::now() + Duration::from_secs(10);
    while rt.state.pad_name(0) != "queue.wav" && Instant::now() < deadline {
        ui::media::poll(&mut rt.state);
        if rt.state.output.dropped > 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(slot(&rt.state, 0), before);
    assert!(rt.state.media.busy());
    assert!(
        rt.state.output.dropped > 1,
        "Load never reached the full queue"
    );
    finish(&mut rt);
    assert_ne!(slot(&rt.state, 0), before);

    fixture::fill_slots(&mut rt.state, 0);
    let before = slot(&rt.state, 0);
    rt.state
        .import_sample(0, Some(root.wav("full.wav", 0.6, 80_000)))
        .unwrap();
    for _ in 0..100 {
        ui::media::poll(&mut rt.state);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(slot(&rt.state, 0), before);
    assert!(rt.state.media.busy());
    assert!((0..SLOTS).all(|index| !rt.state.assets.slots().is_empty(index)));
    rt.state.output.send(Command::Panic);
    finish(&mut rt);
    assert_eq!(rt.state.pad_name(0), "full.wav");
}

#[test]
fn encoded_kit_snapshot_is_authoritative_and_session_media_adopts_into_real_store() {
    let root = Root::new();
    for (index, name) in kit::FILES.iter().enumerate() {
        root.wav(name, 0.1 + index as f32 * 0.01, 8_000);
    }
    let original = fs::read(root.0.join(kit::FILES[0])).unwrap();
    let mut kit = Kit::load(&root.0);
    fs::write(root.0.join(kit::FILES[0]), b"replaced external file").unwrap();
    // Existing callers do not opt in. Identity and audio both use the captured
    // encoded snapshot, not a separately mutated legacy decoded view.
    kit.samples[0].data = vec![0.9; 8_000].into();
    let state = State::new(kit, true, true);
    assert_eq!(
        state
            .assets
            .source(pad(0))
            .unwrap()
            .imported
            .asset
            .reference,
        project::AssetRef::of(&original)
    );
    assert_eq!(
        state.assets.source(pad(0)).unwrap().imported.sample.data[0],
        0.1
    );

    let project = Root::new();
    state
        .media
        .owned
        .adopt(&ProjectMedia::new(project.0.clone()))
        .unwrap();
    let mut document = Document::default();
    for index in 0..16 {
        let runtime = state.assets.pad(pad(index));
        document.pads[index].sample = runtime.assignment.clone();
    }
    for index in [0, 4, 8, 10, 12] {
        let asset = state
            .assets
            .source(pad(index))
            .unwrap()
            .imported
            .asset
            .clone();
        document.assets.push(asset);
    }
    let mut store = project::store::Store::new(project.0.clone());
    store.save(&document, &[]).unwrap();
    let reopened = store.inspect_committed().unwrap().unwrap();
    assert_eq!(reopened.document, document);
    assert_eq!(
        fs::read(
            project
                .0
                .join(project::AssetRef::of(&original).id.relative_path())
        )
        .unwrap(),
        original
    );
}

#[test]
fn overlapping_imports_busy_refusal_and_cancellation_do_not_land_stale_completions() {
    use std::sync::{Mutex, mpsc};
    let root = Root::new();
    let slow = root.wav("slow.wav", 0.7, 8_000);
    let latest = root.wav("latest.wav", 0.3, 8_000);
    let mut rt = runtime();
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let reader = Arc::new(
        move |path: &std::path::Path, _: &brownnote_native::import::Cancellation| {
            if path.file_name().unwrap() == "slow.wav" {
                entered.send(()).unwrap();
                gate.lock().unwrap().recv().unwrap();
            }
            fs::read(path)
        },
    );
    let service = ImportService::with_reader(rt.state.media.owned.clone(), reader).unwrap();
    rt.state.media = ui::media::Media::with_service(rt.state.media.owned.clone(), Ok(service));
    let unchanged = slot(&rt.state, 1);
    rt.state.import_sample(0, Some(slow)).unwrap();
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    rt.state.import_sample(0, Some(latest.clone())).unwrap();
    assert_eq!(
        rt.state.import_sample(1, Some(latest)),
        Err(StartError::Busy)
    );
    assert_eq!(slot(&rt.state, 1), unchanged);
    release.send(()).unwrap();
    finish(&mut rt);
    assert_eq!(rt.state.pad_name(0), "latest.wav");
    assert_eq!(
        rt.state.assets.source(pad(0)).unwrap().imported.sample.data[0],
        0.3
    );
    assert!(rt.state.pad_error(0).is_none());
    assert_eq!(slot(&rt.state, 1), unchanged);
}

#[test]
fn shared_imports_share_choke_identity_but_not_pad_settings() {
    let root = Root::new();
    let path = root.wav("shared.wav", 0.3, 80_000);
    let mut rt = runtime();
    rt.state.import_sample(0, Some(path.clone())).unwrap();
    finish(&mut rt);
    rt.state.import_sample(4, Some(path)).unwrap();
    finish(&mut rt);
    assert_eq!(slot(&rt.state, 0), slot(&rt.state, 4));
    assert_eq!(
        rt.state.assets.playback(pad(0)).unwrap().1,
        rt.state.assets.playback(pad(4)).unwrap().1
    );
    rt.state.output.send(Command::SetChoke(true));
    rt.state.settings[4].gain = 0.0;
    rt.state.trigger(0, 1.0);
    rt.frame(0.01);
    assert_eq!(rt.state.counts[0], 1);
    rt.state.trigger(4, 1.0);
    rt.frame(0.01);
    assert_eq!(
        rt.state.counts[0], 0,
        "shared source did not choke across pads"
    );
    assert_eq!(rt.state.settings[0].gain, 1.0);
    assert_eq!(rt.state.settings[4].gain, 0.0);
}

#[test]
fn mounted_replacement_retires_once_after_the_stealing_tail_ends() {
    let mut rt = runtime();
    fixture::replace(&mut rt.state, 0, &fixture::source(0.23, 80_000));
    let previous = slot(&rt.state, 0);
    rt.state.trigger(0, 1.0);
    rt.state.output.advance(0.001);
    // Steal this voice into a fading tail before replacing its source.
    for _ in 0..32 {
        rt.state.trigger(1, 1.0);
    }
    fixture::replace(&mut rt.state, 0, &fixture::source(0.31, 80_000));
    rt.state.output.advance(0.001);
    rt.state.poll_assets();
    assert!(
        !rt.state.assets.slots().is_empty(previous),
        "stealing tail lost its sample"
    );
    rt.state.output.advance(0.01);
    let returned = rt.state.output.take_retired();
    assert_eq!(returned.len(), 1);
    let Returned::Retired {
        slot: actual,
        sample,
    } = &returned[0]
    else {
        panic!("not retired");
    };
    assert_eq!(*actual, previous);
    let duplicate = Returned::Retired {
        slot: previous,
        sample: sample.clone(),
    };
    for item in returned {
        assert_eq!(rt.state.assets.returned(item), Ok(previous));
    }
    assert_eq!(
        rt.state.assets.returned(duplicate),
        Err(RegistryError::UnexpectedReturn)
    );
    assert!(rt.state.assets.slots().is_empty(previous));
    rt.state.output.advance(0.1);
    assert!(rt.state.output.take_retired().is_empty(), "retired twice");
}

#[test]
fn pending_unloads_retry_and_fresh_scene_outputs_readmit_each_retirement() {
    for scene in ["no-audio", "output-missing"] {
        let mut rt = runtime();
        fixture::render(&mut rt.state, 0, 0.17);
        rt.state.output.advance(0.001);
        let previous = slot(&rt.state, 0);
        while rt.state.output.send(Command::SetChoke(false)) {}
        rt.state
            .assets
            .reset_to_source(pad(0), |command| rt.state.output.send(command));
        rt.state.poll_assets();
        assert!(
            !rt.state.assets.slots().is_empty(previous),
            "queue refusal freed a slot early"
        );
        rt.state.output.advance(0.001);
        rt.state.poll_assets(); // Normal pending-unload retry, now admitted.
        let apply = ui::SCENES
            .iter()
            .find(|(name, _)| *name == scene)
            .unwrap()
            .1;
        apply(&mut rt.state); // Discard the old queue after admission.
        rt.frame(0.01);
        rt.frame(0.001);
        assert!(
            rt.state.assets.slots().is_empty(previous),
            "fresh output leaked its retirement"
        );
        rt.state.trigger(0, 1.0);
        rt.frame(0.01);
        assert_eq!(
            rt.state.counts[0], 1,
            "source did not survive output replacement"
        );
    }
}

#[test]
fn a_chain_edit_cancels_an_older_import_and_a_new_import_invalidates_an_older_render() {
    use brownnote_native::{
        import::Cancellation,
        pedals::{
            catalog::Catalog,
            render::{Chain, Rendered, Stage},
        },
    };
    use std::sync::{Mutex, mpsc};
    let root = Root::new();
    let slow = root.wav("slow.wav", 0.7, 8_000);
    let next = root.wav("next.wav", 0.4, 8_000);
    let mut rt = runtime();
    rt.state.pedals.catalog = ui::CatalogState::Ready(Arc::new(
        Catalog::parse(include_str!("../tests/fixtures/bench-list.txt")).unwrap(),
    ));
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let reader = Arc::new(move |path: &std::path::Path, _: &Cancellation| {
        if path.file_name().unwrap() == "slow.wav" {
            entered.send(()).unwrap();
            gate.lock().unwrap().recv().unwrap();
        }
        fs::read(path)
    });
    let service = ImportService::with_reader(rt.state.media.owned.clone(), reader).unwrap();
    rt.state.media = ui::media::Media::with_service(rt.state.media.owned.clone(), Ok(service));
    rt.state.pedals.render_fn = Arc::new(|job| {
        Ok(Rendered {
            sample: job.source.clone(),
            cached: false,
            clipped: None,
        })
    });
    let original = rt
        .state
        .assets
        .source(pad(0))
        .unwrap()
        .imported
        .asset
        .reference
        .id;
    rt.state.import_sample(0, Some(slow)).unwrap();
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    ui::pedals::add_stage(&mut rt.state, 0, "Reverb");
    release.send(()).unwrap();
    // A queued import on another pad fences completion of the cancelled read.
    rt.state.import_sample(1, Some(next.clone())).unwrap();
    finish(&mut rt);
    assert_eq!(
        rt.state
            .assets
            .source(pad(0))
            .unwrap()
            .imported
            .asset
            .reference
            .id,
        original
    );

    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    rt.state.pedals.render_fn = Arc::new(move |job| {
        entered.send(()).unwrap();
        gate.lock().unwrap().recv().unwrap();
        Ok(Rendered {
            sample: job.source.clone(),
            cached: false,
            clipped: None,
        })
    });
    rt.state.pedals.pads[0].chain = Chain {
        stages: vec![Stage {
            effect: "Rat Sketch".into(),
            pots: vec![],
            mix: None,
        }],
    };
    ui::pedals::request(&mut rt.state, 0);
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    // Empty the chain without requesting another render: this specifically
    // exercises an import superseding the in-flight worker.
    rt.state.pedals.pads[0].chain = Chain::default();
    rt.state.import_sample(0, Some(next)).unwrap();
    release.send(()).unwrap();
    finish(&mut rt);
    assert_eq!(rt.state.pad_name(0), "next.wav");
    assert!(
        rt.state
            .assets
            .pad(pad(0))
            .assignment
            .as_ref()
            .unwrap()
            .retained
            .is_none()
    );
    assert_eq!(
        rt.state.assets.source(pad(0)).unwrap().imported.sample.data[0],
        0.4
    );
    assert_eq!(rt.state.pedals.pads[0].status, ui::Status::Base);
}

#[test]
fn required_pedal_media_survives_cache_removal_output_rebuild_and_store_adoption() {
    use brownnote_native::pedals::{catalog::Catalog, render::Rendered};
    let cache = Root::new();
    let cache_path = cache.0.clone();
    let mut rt = runtime();
    rt.state.pedals.catalog = ui::CatalogState::Ready(Arc::new(
        Catalog::parse(include_str!("../tests/fixtures/bench-list.txt")).unwrap(),
    ));
    rt.state.pedals.render_fn = Arc::new(move |_| {
        let sample = Sample {
            data: vec![0.37; 16_000].into(),
            rate: 8_000,
        };
        fs::write(
            cache_path.join("evictable.wav"),
            kit::encode_wav(&sample).unwrap(),
        )
        .unwrap();
        Ok(Rendered {
            sample,
            cached: true,
            clipped: Some(1),
        })
    });
    ui::pedals::add_stage(&mut rt.state, 0, "Reverb");
    let deadline = Instant::now() + Duration::from_secs(10);
    while matches!(rt.state.pedals.pads[0].status, ui::Status::Rendering)
        && Instant::now() < deadline
    {
        rt.frame(0.001);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        rt.state.pedals.pads[0].status,
        ui::Status::Ready { clipped: true, .. }
    ));
    let retained = rt
        .state
        .assets
        .audible(pad(0))
        .unwrap()
        .imported
        .asset
        .clone();
    assert_eq!(retained.kind, project::MediaKind::RetainedRender);
    drop(cache);
    let no_audio = ui::SCENES
        .iter()
        .find(|(name, _)| *name == "no-audio")
        .unwrap()
        .1;
    no_audio(&mut rt.state);
    rt.state.trigger(0, 1.0);
    rt.frame(0.01);
    assert_eq!(rt.state.counts[0], 1);
    assert!((rt.state.peak - 0.37 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.001);

    let destination = Root::new();
    rt.state
        .media
        .owned
        .adopt(&ProjectMedia::new(destination.0.clone()))
        .unwrap();
    let mut document = Document::default();
    for index in 0..16 {
        document.pads[index].sample = rt.state.assets.pad(pad(index)).assignment.clone();
        document.pads[index].chain = rt.state.pedals.pads[index].chain.clone();
        for asset in [
            rt.state.assets.source(pad(index)),
            rt.state.assets.audible(pad(index)),
        ]
        .into_iter()
        .flatten()
        {
            if !document
                .assets
                .iter()
                .any(|item| item.reference.id == asset.imported.asset.reference.id)
            {
                document.assets.push(asset.imported.asset.clone());
            }
        }
    }
    let mut store = project::store::Store::new(destination.0.clone());
    store.save(&document, &[]).unwrap();
    assert_eq!(
        store.inspect_committed().unwrap().unwrap().document,
        document
    );
    let encoded = fs::read(destination.0.join(retained.reference.id.relative_path())).unwrap();
    assert_eq!(project::AssetRef::of(&encoded), retained.reference);
    let decoded =
        kit::decode_wav(encoded.as_slice(), std::path::Path::new("retained.wav")).unwrap();
    assert_eq!(decoded.data[0], 0.37);
    assert_eq!(decoded.data.len(), 16_000);
}
