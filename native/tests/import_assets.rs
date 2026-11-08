// Bootstrap until the integrator exports these modules from the real library.
// All domain, decoder, command, queue and mixer types are the shipped types.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use brownnote_native::{engine, kit, project};
#[path = "../src/assets.rs"]
mod assets;
#[path = "../src/import.rs"]
mod import;

use assets::{AssetRegistry, ChokeIdentity, RegistryError};
use engine::{Command, Meters, Mixer, Returned, SLOTS, Sample, Slots};
use import::{ImportFailure, ImportService, Imported, MediaPublisher, ProjectMedia};
use project::{Asset, AssetId, AssetRef, MediaKind, PadId, SampleAssignment};
use std::{
    fs,
    io::{self, Cursor},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
        mpsc,
    },
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "brownnote-import-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wav(rate: u32, channels: u16, float: bool, frames: usize) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: if float { 32 } else { 16 },
            sample_format: if float {
                hound::SampleFormat::Float
            } else {
                hound::SampleFormat::Int
            },
        },
    )
    .unwrap();
    for _ in 0..frames {
        for channel in 0..channels {
            if float {
                writer
                    .write_sample(if channel == 0 { 0.5_f32 } else { -0.25 })
                    .unwrap();
            } else {
                writer
                    .write_sample(if channel == 0 { 16384_i16 } else { -8192 })
                    .unwrap();
            }
        }
    }
    writer.finalize().unwrap();
    bytes.into_inner()
}

fn finish(mut job: import::ImportJob) -> Result<Imported, import::ImportError> {
    // Await the real completion channel, with a bounded test deadline.
    job.wait_timeout(Duration::from_secs(20))
        .expect("import worker deadline")
}

fn prepared(tag: u8, value: f32, frames: usize) -> Imported {
    let sample = Sample {
        data: vec![value; frames].into(),
        rate: 8_000,
    };
    Imported {
        asset: Asset {
            reference: AssetRef::of(&[tag]),
            kind: MediaKind::Source,
        },
        waveform: kit::waveform(&sample),
        sample,
        label: format!("{tag}.wav"),
    }
}
fn pad(index: u8) -> PadId {
    PadId::try_from(index).unwrap()
}
fn identity(imported: &Imported) -> ChokeIdentity {
    ChokeIdentity::Asset(imported.asset.reference.id)
}
fn replace(
    registry: &mut AssetRegistry,
    p: u8,
    imported: &Imported,
    send: impl FnMut(Command) -> bool,
) {
    let ticket = registry.begin(pad(p));
    registry
        .replace_source(ticket, imported, identity(imported), send)
        .unwrap();
}
fn trigger(registry: &AssetRegistry, p: u8) -> Command {
    let (slot, group) = registry.playback(pad(p)).unwrap();
    Command::Trigger {
        pad: p as usize,
        slot,
        group,
        velocity: 1.0,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    }
}

#[test]
fn all_wav_variants_copy_exact_authoritative_bytes_and_survive_source_removal() {
    let root = Root::new();
    let service = ImportService::new(Arc::new(ProjectMedia::new(root.0.clone()))).unwrap();
    for rate in [8_000, 44_100, 48_000, 192_000] {
        for channels in [1, 2] {
            for float in [false, true] {
                let bytes = wav(rate, channels, float, 1);
                let path = root.0.join("source.wav");
                fs::write(&path, &bytes).unwrap();
                let imported = finish(service.import_sample(Some(path.clone())).unwrap()).unwrap();
                assert_eq!(imported.asset.reference, AssetRef::of(&bytes));
                assert_eq!(imported.sample.rate, rate);
                assert_eq!(
                    &*imported.sample.data,
                    &[if channels == 1 { 0.5 } else { 0.125 }]
                );
                assert_eq!(imported.label, "source.wav");
                assert_eq!(imported.waveform, kit::waveform(&imported.sample));
                fs::remove_file(path).unwrap();
                assert_eq!(
                    fs::read(root.0.join(imported.asset.reference.id.relative_path())).unwrap(),
                    bytes
                );
                project::media::resolve_verified(&root.0, &imported.asset.reference).unwrap();
            }
        }
    }
    // Exact duration boundary, including the maximum-rate/stereo/float case.
    let bytes = wav(192_000, 2, true, 192_000 * 30);
    let path = root.0.join("maximum.wav");
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        finish(service.import_sample(Some(path)).unwrap())
            .unwrap()
            .sample
            .data
            .len(),
        192_000 * 30
    );
}

#[test]
fn invalid_missing_oversized_cancelled_and_publisher_errors_name_the_file() {
    let root = Root::new();
    let service = ImportService::new(Arc::new(ProjectMedia::new(root.0.clone()))).unwrap();
    let mut invalid = vec![
        b"corrupt".to_vec(),
        wav(8_000, 1, false, 0),
        wav(8_000, 1, false, 240_001),
        wav(7_999, 1, false, 1),
        wav(192_001, 1, false, 1),
        wav(44_100, 3, false, 1),
    ];
    let mut unsupported = wav(44_100, 1, false, 1);
    unsupported[34..36].copy_from_slice(&8_u16.to_le_bytes());
    invalid.push(unsupported);
    let mut nonfinite = wav(44_100, 1, true, 1);
    let end = nonfinite.len();
    nonfinite[end - 4..].copy_from_slice(&f32::NAN.to_le_bytes());
    invalid.push(nonfinite);
    let mut truncated = wav(44_100, 2, false, 2);
    truncated.pop();
    invalid.push(truncated);
    for bytes in invalid {
        let path = root.0.join("bad.wav");
        fs::write(&path, bytes).unwrap();
        let error = finish(service.import_sample(Some(path.clone())).unwrap()).unwrap_err();
        assert_eq!(error.path, path);
        assert!(matches!(error.failure, ImportFailure::Decode(_)), "{error}");
        assert!(!root.0.join("media").exists());
    }
    let missing = root.0.join("missing.wav");
    assert!(
        matches!(finish(service.import_sample(Some(missing.clone())).unwrap()).unwrap_err(),
        import::ImportError { path, failure: ImportFailure::Read(_) } if path == missing)
    );
    assert_eq!(
        finish(service.import_sample(None).unwrap())
            .unwrap_err()
            .failure,
        ImportFailure::Cancelled
    );
    struct Fails;
    impl MediaPublisher for Fails {
        fn publish(&self, _: &AssetRef, _: &[u8]) -> io::Result<()> {
            Err(io::Error::other("injected copy failure"))
        }
    }
    let path = root.0.join("valid.wav");
    fs::write(&path, wav(8_000, 1, false, 1)).unwrap();
    let service = ImportService::new(Arc::new(Fails)).unwrap();
    assert!(matches!(
        finish(service.import_sample(Some(path)).unwrap())
            .unwrap_err()
            .failure,
        ImportFailure::Publish(_)
    ));
}

#[test]
fn immutable_publish_deduplicates_and_never_overwrites_corrupt_or_symlink_media() {
    let root = Root::new();
    let media = ProjectMedia::new(root.0.clone());
    let bytes = wav(8_000, 1, false, 1);
    let reference = AssetRef::of(&bytes);
    media.publish(&reference, &bytes).unwrap();
    media.publish(&reference, &bytes).unwrap();
    let path = root.0.join(reference.id.relative_path());
    fs::write(&path, b"other sound").unwrap();
    assert!(media.publish(&reference, &bytes).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"other sound");
    assert!(media.publish(&AssetRef::of(b"wrong"), &bytes).is_err());
    assert_eq!(fs::read_dir(root.0.join("media")).unwrap().count(), 1);
    #[cfg(unix)]
    {
        fs::remove_file(&path).unwrap();
        let external = Root::new();
        let outside = external.0.join("outside.wav");
        fs::write(&outside, &bytes).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(media.publish(&reference, &bytes).is_err());
    }
}

#[test]
fn bounded_worker_snapshot_and_cancellation_cover_partial_and_overlapping_work() {
    let root = Root::new();
    let bytes = wav(8_000, 1, false, 1);
    let snapshot = bytes.clone();
    let (entered, entering) = mpsc::sync_channel(1);
    let (release, releasing) = mpsc::sync_channel(1);
    let releasing = Mutex::new(releasing);
    let main = std::thread::current().id();
    let service = ImportService::with_reader(
        Arc::new(ProjectMedia::new(root.0.clone())),
        Arc::new(move |_, _| {
            assert_ne!(std::thread::current().id(), main);
            entered.send(()).unwrap();
            releasing
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .unwrap();
            Ok(snapshot.clone())
        }),
    )
    .unwrap();
    let mut first = service
        .import_sample(Some(PathBuf::from("snapshot.wav")))
        .unwrap();
    entering.recv_timeout(Duration::from_secs(20)).unwrap();
    let second = service
        .import_sample(Some(PathBuf::from("cancel-before-read.wav")))
        .unwrap();
    assert!(matches!(
        service.import_sample(Some(PathBuf::from("overflow.wav"))),
        Err(import::StartError::Busy)
    ));
    first.cancel();
    assert_eq!(
        first.take().unwrap().unwrap_err().failure,
        ImportFailure::Cancelled
    );
    assert!(first.take().is_none());
    second.cancel();
    release.send(()).unwrap();
    assert_eq!(
        finish(second).unwrap_err().failure,
        ImportFailure::Cancelled
    );
    // The injected snapshot, not the path, supplies hash/decode/publish.
    let snapshot = bytes.clone();
    let service = ImportService::with_reader(
        Arc::new(ProjectMedia::new(root.0.clone())),
        Arc::new(move |_, _| Ok(snapshot.clone())),
    )
    .unwrap();
    let imported = finish(
        service
            .import_sample(Some(PathBuf::from("not-on-disk.wav")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(imported.asset.reference.id, AssetId::of(&bytes));
    assert_eq!(
        fs::read(root.0.join(imported.asset.reference.id.relative_path())).unwrap(),
        bytes
    );
}

#[test]
fn replacement_preserves_long_current_voice_next_hit_and_exactly_once_retirement() {
    let old = prepared(1, 0.25, 16_000);
    let new = prepared(2, -0.5, 16_000);
    let mut registry = AssetRegistry::default();
    let (mut tx, mut rx) = rtrb::RingBuffer::new(256);
    let (returns, mut retired) = rtrb::RingBuffer::new(SLOTS);
    let meters = Arc::new(Meters::default());
    let mut mixer = Mixer::new(Slots::empty(), 8_000, meters.clone(), returns);
    replace(&mut registry, 0, &old, |c| tx.push(c).is_ok());
    mixer.drain(&mut rx);
    mixer.command(trigger(&registry, 0));
    mixer.render(&mut [0.0; 200], 2);
    let old_slot = registry.playback(pad(0)).unwrap().0;
    replace(&mut registry, 0, &new, |c| tx.push(c).is_ok());
    mixer.drain(&mut rx);
    assert_ne!(registry.playback(pad(0)).unwrap().0, old_slot);
    let mut audio = [0.0; 200];
    mixer.render(&mut audio, 2);
    assert!(audio.iter().all(|v| *v > 0.1)); // old voice still sounds
    assert!(retired.pop().is_err());
    mixer.command(trigger(&registry, 0));
    mixer.render(&mut audio, 2);
    assert!(audio.iter().all(|v| *v < -0.1)); // new voice joins old
    assert_eq!(meters.counts()[0], 2);
    for _ in 0..170 {
        mixer.render(&mut audio, 2);
    }
    let item = retired.pop().unwrap();
    assert!(matches!(&item, Returned::Retired {slot, ..} if *slot == old_slot));
    registry.returned(item).unwrap();
    assert!(retired.pop().is_err());
    assert!(registry.slots().is_empty(old_slot));
    assert_eq!(
        registry.returned(Returned::Retired {
            slot: old_slot,
            sample: old.sample
        }),
        Err(RegistryError::UnexpectedReturn)
    );
}

#[test]
fn shared_assets_keep_refs_and_explicit_choke_through_retained_and_source_replacement() {
    let a = prepared(1, 0.5, 8_000);
    let b = prepared(2, 0.25, 8_000);
    let mut render = prepared(3, 0.1, 8_000);
    render.asset.kind = MediaKind::RetainedRender;
    let mut registry = AssetRegistry::default();
    let mut commands = Vec::new();
    replace(&mut registry, 0, &a, |c| {
        commands.push(c);
        true
    });
    replace(&mut registry, 1, &a, |c| {
        commands.push(c);
        true
    });
    assert_eq!(
        commands
            .iter()
            .filter(|c| matches!(c, Command::Load { .. }))
            .count(),
        1
    );
    assert_eq!(registry.playback(pad(0)), registry.playback(pad(1)));
    let ticket = registry.begin(pad(0));
    registry
        .replace_retained(ticket, &render, |c| {
            commands.push(c);
            true
        })
        .unwrap();
    let (render_slot, group) = registry.playback(pad(0)).unwrap();
    assert_eq!(group, registry.playback(pad(1)).unwrap().1);
    assert_ne!(render_slot, registry.playback(pad(1)).unwrap().0);
    assert!(
        commands
            .iter()
            .all(|c| !matches!(c, Command::Unload { .. }))
    );
    replace(&mut registry, 0, &b, |c| {
        commands.push(c);
        true
    });
    assert!(registry.source(pad(1)).is_some());
    assert_eq!(
        commands
            .iter()
            .filter(|c| matches!(c, Command::Unload {slot} if *slot == render_slot))
            .count(),
        1
    );
    let ticket = registry.begin(pad(0));
    registry
        .replace_source(ticket, &a, ChokeIdentity::Group(77), |c| {
            commands.push(c);
            true
        })
        .unwrap();
    assert_eq!(
        registry.playback(pad(0)).unwrap().0,
        registry.playback(pad(1)).unwrap().0
    );
    assert_ne!(
        registry.playback(pad(0)).unwrap().1,
        registry.playback(pad(1)).unwrap().1
    );
    // Real mixer observes different groups, then intentional cross-pad choke.
    let (returns, _) = rtrb::RingBuffer::new(SLOTS);
    let meters = Arc::new(Meters::default());
    let mut mixer = Mixer::new(registry.slots().clone(), 8_000, meters.clone(), returns);
    mixer.command(trigger(&registry, 0));
    mixer.command(trigger(&registry, 1));
    mixer.render(&mut [0.0; 100], 2);
    assert_eq!(&meters.counts()[..2], &[1, 1]);
    let ticket = registry.begin(pad(0));
    registry
        .replace_source(ticket, &a, identity(&a), |_| true)
        .unwrap();
    mixer.command(trigger(&registry, 0));
    mixer.render(&mut [0.0; 100], 2);
    assert_eq!(&meters.counts()[..2], &[2, 0]);
}

#[test]
fn full_queue_registry_stale_cancel_and_pending_unloads_rollback_without_overwrite() {
    let a = prepared(1, 0.5, 1);
    let b = prepared(2, 0.25, 1);
    let mut registry = AssetRegistry::default();
    replace(&mut registry, 0, &a, |_| true);
    replace(&mut registry, 1, &a, |_| true);
    let before = registry.playback(pad(0));
    let old = registry.begin(pad(0));
    let newest = registry.begin(pad(0));
    assert_eq!(
        registry.replace_source(old, &b, identity(&b), |_| panic!("stale sent")),
        Err(RegistryError::Stale)
    );
    registry.cancel(newest);
    assert_eq!(
        registry.replace_source(newest, &b, identity(&b), |_| panic!("cancel sent")),
        Err(RegistryError::Stale)
    );
    let ticket = registry.begin(pad(0));
    assert_eq!(
        registry.replace_source(ticket, &b, identity(&b), |_| false),
        Err(RegistryError::QueueFull)
    );
    assert_eq!(registry.playback(pad(0)), before);
    assert_eq!(registry.playback(pad(1)), before);
    assert!(registry.asset(b.asset.reference.id).is_none());
    assert!(registry.report_error(ticket, "file-specific failure".into()));
    assert_eq!(
        registry.pad(pad(0)).error.as_deref(),
        Some("file-specific failure")
    );
    assert_eq!(registry.playback(pad(0)), before);
    let (mut tx, mut rx) = rtrb::RingBuffer::new(1);
    replace(&mut registry, 0, &b, |c| tx.push(c).is_ok());
    assert!(matches!(rx.pop().unwrap(), Command::Load { .. }));
    // Last reference to A goes away while Load uses the only queue entry.
    let c = prepared(3, 0.1, 1);
    replace(&mut registry, 1, &c, |c| tx.push(c).is_ok());
    registry.flush_unloads(|c| tx.push(c).is_ok());
    assert!(matches!(rx.pop().unwrap(), Command::Load { .. }));
    registry.flush_unloads(|c| tx.push(c).is_ok());
    assert!(matches!(rx.pop().unwrap(), Command::Unload { .. }));
    registry.flush_unloads(|_| panic!("duplicate unload"));
    assert!(!registry.slots().is_empty(before.unwrap().0));
    let mut full = AssetRegistry::new(Slots::from_samples((0..SLOTS).map(|_| a.sample.clone())));
    let ticket = full.begin(pad(0));
    assert_eq!(
        full.replace_source(ticket, &b, identity(&b), |_| panic!("full sent")),
        Err(RegistryError::Full)
    );
    assert!(full.pad(pad(0)).assignment.is_none());
}

#[test]
fn bootstrap_adopts_real_slots_and_reset_retained_releases_only_the_render() {
    let a = prepared(1, 0.5, 1);
    let mut render = prepared(2, 0.25, 1);
    render.asset.kind = MediaKind::RetainedRender;
    let mut registry = AssetRegistry::new(Slots::from_samples([a.sample.clone()]));
    registry.register_existing(0, a.clone()).unwrap();
    registry
        .bind_existing(
            pad(0),
            SampleAssignment {
                source: a.asset.reference.id,
                retained: None,
            },
            identity(&a),
        )
        .unwrap();
    assert_eq!(registry.playback(pad(0)).unwrap().0, 0);
    let ticket = registry.begin(pad(0));
    registry
        .replace_retained(ticket, &render, |_| true)
        .unwrap();
    let slot = registry.playback(pad(0)).unwrap().0;
    let mut unloads = Vec::new();
    registry.reset_to_source(pad(0), |c| {
        unloads.push(c);
        true
    });
    assert_eq!(registry.playback(pad(0)).unwrap().0, 0);
    assert!(matches!(&unloads[..], [Command::Unload {slot: actual}] if *actual == slot));
    assert_eq!(registry.audible(pad(0)).unwrap().imported.label, "1.wav");
}

#[test]
fn cancellation_after_publish_does_not_assign_or_remove_shared_media() {
    struct Gate {
        media: ProjectMedia,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl MediaPublisher for Gate {
        fn publish(&self, reference: &AssetRef, bytes: &[u8]) -> io::Result<()> {
            self.media.publish(reference, bytes)?;
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .unwrap();
            Ok(())
        }
    }
    let root = Root::new();
    let (entered, entering) = mpsc::sync_channel(1);
    let (release, releasing) = mpsc::sync_channel(1);
    let bytes = wav(8_000, 1, false, 1);
    let reference = AssetRef::of(&bytes);
    let service = ImportService::with_reader(
        Arc::new(Gate {
            media: ProjectMedia::new(root.0.clone()),
            entered,
            release: Mutex::new(releasing),
        }),
        Arc::new(move |_, _| Ok(bytes.clone())),
    )
    .unwrap();
    let mut job = service
        .import_sample(Some(PathBuf::from("published.wav")))
        .unwrap();
    entering.recv_timeout(Duration::from_secs(20)).unwrap();
    assert!(job.take().is_none());
    assert!(job.wait_timeout(Duration::ZERO).is_none());
    job.cancel();
    assert_eq!(
        job.take().unwrap().unwrap_err().failure,
        ImportFailure::Cancelled
    );
    release.send(()).unwrap();
    assert!(job.take().is_none());
    project::media::resolve_verified(&root.0, &reference).unwrap();
}

#[test]
fn repeated_import_before_retirement_gets_new_slot_but_the_same_choke_identity() {
    let a = prepared(1, 0.5, 8_000);
    let b = prepared(2, 0.25, 8_000);
    let mut registry = AssetRegistry::default();
    let (mut tx, mut rx) = rtrb::RingBuffer::new(256);
    let (returns, mut retired) = rtrb::RingBuffer::new(SLOTS);
    let mut mixer = Mixer::new(Slots::empty(), 8_000, Arc::default(), returns);
    replace(&mut registry, 0, &a, |c| tx.push(c).is_ok());
    let first = registry.playback(pad(0)).unwrap();
    mixer.drain(&mut rx);
    mixer.command(trigger(&registry, 0));
    let stale_render = registry.begin(pad(0));
    replace(&mut registry, 0, &b, |c| tx.push(c).is_ok());
    replace(&mut registry, 0, &a, |c| tx.push(c).is_ok());
    let again = registry.playback(pad(0)).unwrap();
    assert_ne!(again.0, first.0);
    assert_eq!(again.1, first.1);
    assert_eq!(
        registry.replace_source(stale_render, &b, identity(&b), |_| panic!("stale load")),
        Err(RegistryError::Stale)
    );
    let mut render = b.clone();
    render.asset.kind = MediaKind::RetainedRender;
    assert_eq!(
        registry.replace_retained(stale_render, &render, |_| panic!("stale render")),
        Err(RegistryError::Stale)
    );
    mixer.drain(&mut rx);
    mixer.command(trigger(&registry, 0)); // explicit shared identity chokes old A
    mixer.render(&mut [0.0; 100], 2);
    let mut slots = Vec::new();
    while let Ok(item) = retired.pop() {
        slots.push(registry.returned(item).unwrap());
    }
    slots.sort_unstable();
    assert_eq!(slots, vec![0, 1]);
    assert!(!registry.slots().is_empty(again.0));
    // Successful tickets are single-use, including same-asset replacements.
    let ticket = registry.begin(pad(0));
    registry
        .replace_source(ticket, &a, identity(&a), |_| panic!("shared reload"))
        .unwrap();
    assert_eq!(
        registry.replace_source(ticket, &a, identity(&a), |_| panic!("replayed load")),
        Err(RegistryError::Stale)
    );
}

#[test]
fn actual_full_command_queue_and_full_registry_preserve_existing_assignments() {
    let a = prepared(1, 0.5, 1);
    let b = prepared(2, 0.25, 1);
    let mut registry =
        AssetRegistry::new(Slots::from_samples((0..SLOTS).map(|_| a.sample.clone())));
    registry.register_existing(0, a.clone()).unwrap();
    for index in [0, 1] {
        registry
            .bind_existing(
                pad(index),
                SampleAssignment {
                    source: a.asset.reference.id,
                    retained: None,
                },
                identity(&a),
            )
            .unwrap();
    }
    let before = registry.playback(pad(0));
    let ticket = registry.begin(pad(0));
    assert_eq!(
        registry.replace_source(ticket, &b, identity(&b), |_| panic!(
            "full registry admitted"
        )),
        Err(RegistryError::Full)
    );
    assert_eq!(registry.playback(pad(0)), before);
    assert_eq!(registry.playback(pad(1)), before);
    let mut registry = AssetRegistry::default();
    replace(&mut registry, 0, &a, |_| true);
    let (mut tx, mut rx) = rtrb::RingBuffer::new(1);
    tx.push(Command::Panic).unwrap();
    let ticket = registry.begin(pad(0));
    assert_eq!(
        registry.replace_source(ticket, &b, identity(&b), |c| tx.push(c).is_ok()),
        Err(RegistryError::QueueFull)
    );
    assert_eq!(
        registry.source(pad(0)).unwrap().imported.asset.reference.id,
        a.asset.reference.id
    );
    assert!(matches!(rx.pop().unwrap(), Command::Panic));
    registry
        .replace_source(ticket, &b, identity(&b), |c| tx.push(c).is_ok())
        .unwrap();
}

#[test]
fn fresh_output_requeues_outstanding_retirement_without_reloading_or_leaking_slots() {
    let a = prepared(1, 0.5, 1);
    let b = prepared(2, 0.25, 1);
    let mut registry = AssetRegistry::default();
    replace(&mut registry, 0, &a, |_| true);
    replace(&mut registry, 0, &b, |_| true);
    registry.flush_unloads(|_| panic!("already admitted on old output"));
    let (returns, mut retired) = rtrb::RingBuffer::new(SLOTS);
    let mut mixer = Mixer::new(registry.slots().clone(), 8_000, Arc::default(), returns);
    registry.output_rebuilt();
    registry.flush_unloads(|c| {
        mixer.command(c);
        true
    });
    registry.flush_unloads(|_| panic!("duplicate new-output unload"));
    mixer.render(&mut [], 2);
    registry.returned(retired.pop().unwrap()).unwrap();
    assert!(retired.pop().is_err());
    assert!(registry.slots().is_empty(0));
    assert_eq!(
        registry.source(pad(0)).unwrap().imported.asset.reference.id,
        b.asset.reference.id
    );
}

#[test]
fn concurrent_publishers_share_one_verified_immutable_object() {
    let root = Root::new();
    let bytes = Arc::new(wav(8_000, 1, false, 1));
    let reference = AssetRef::of(&bytes);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let root = root.0.clone();
            let bytes = bytes.clone();
            let reference = reference.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                ProjectMedia::new(root).publish(&reference, &bytes)
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    project::media::resolve_verified(&root.0, &reference).unwrap();
    assert_eq!(fs::read_dir(root.0.join("media")).unwrap().count(), 1);
}

#[test]
fn failed_import_completion_keeps_old_sound_and_unrelated_pad_playable() {
    let root = Root::new();
    let service = ImportService::new(Arc::new(ProjectMedia::new(root.0.clone()))).unwrap();
    let a = prepared(1, 0.5, 8_000);
    let b = prepared(2, 0.25, 8_000);
    let mut registry = AssetRegistry::default();
    let (mut tx, mut rx) = rtrb::RingBuffer::new(256);
    let (returns, _) = rtrb::RingBuffer::new(SLOTS);
    let meters = Arc::new(Meters::default());
    let mut mixer = Mixer::new(Slots::empty(), 8_000, meters.clone(), returns);
    replace(&mut registry, 0, &a, |c| tx.push(c).is_ok());
    replace(&mut registry, 1, &b, |c| tx.push(c).is_ok());
    mixer.drain(&mut rx);
    let old = registry.playback(pad(0));
    let unrelated = registry.playback(pad(1));
    let ticket = registry.begin(pad(0));
    let path = root.0.join("corrupt.wav");
    fs::write(&path, b"not a WAV").unwrap();
    let error = finish(service.import_sample(Some(path)).unwrap()).unwrap_err();
    assert!(registry.report_error(ticket, error.to_string()));
    assert_eq!(registry.playback(pad(0)), old);
    assert_eq!(registry.playback(pad(1)), unrelated);
    assert!(
        registry
            .pad(pad(0))
            .error
            .as_ref()
            .unwrap()
            .contains("corrupt.wav")
    );
    assert!(registry.pad(pad(1)).error.is_none());
    mixer.command(trigger(&registry, 0));
    mixer.command(trigger(&registry, 1));
    mixer.render(&mut [0.0; 100], 2);
    assert_eq!(&meters.counts()[..2], &[1, 1]);
    let newest = registry.begin(pad(0));
    assert!(!registry.report_error(ticket, "stale failure".into()));
    registry.cancel(newest);
}

#[test]
fn repeated_real_admissions_fill_registry_and_reuse_only_returned_slots() {
    let mut registry = AssetRegistry::default();
    let (mut tx, mut rx) = rtrb::RingBuffer::new(256);
    let (returns, mut retired) = rtrb::RingBuffer::new(SLOTS);
    let mut mixer = Mixer::new(Slots::empty(), 8_000, Arc::default(), returns);
    let shared = prepared(0, 0.5, 8_000);
    replace(&mut registry, 0, &shared, |c| tx.push(c).is_ok());
    replace(&mut registry, 1, &shared, |c| tx.push(c).is_ok());
    for tag in 1..SLOTS {
        let sample = prepared(tag as u8, 0.25, 8_000);
        replace(&mut registry, 0, &sample, |c| tx.push(c).is_ok());
        assert_eq!(registry.playback(pad(0)).unwrap().0, tag);
    }
    let before = registry.playback(pad(0));
    let extra = prepared(SLOTS as u8, 0.1, 8_000);
    let ticket = registry.begin(pad(0));
    assert_eq!(
        registry.replace_source(ticket, &extra, identity(&extra), |c| tx.push(c).is_ok()),
        Err(RegistryError::Full)
    );
    assert_eq!(registry.playback(pad(0)), before);
    assert_eq!(registry.playback(pad(1)).unwrap().0, 0);
    mixer.drain(&mut rx);
    mixer.render(&mut [], 2);
    let mut returned = Vec::new();
    while let Ok(item) = retired.pop() {
        returned.push(registry.returned(item).unwrap());
    }
    assert_eq!(returned, (1..SLOTS - 1).collect::<Vec<_>>());
    registry
        .replace_source(ticket, &extra, identity(&extra), |c| tx.push(c).is_ok())
        .unwrap();
    assert_eq!(registry.playback(pad(0)).unwrap().0, 1);
    assert_eq!(registry.playback(pad(1)).unwrap().0, 0);
    mixer.drain(&mut rx);
    mixer.command(trigger(&registry, 0));
    mixer.command(trigger(&registry, 1));
    let mut audio = [0.0; 100];
    mixer.render(&mut audio, 2);
    assert!(audio.iter().all(|v| *v > 0.3));
}
