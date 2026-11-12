//! Public bundle/CLI boundaries. All default tests execute only the strict fake.
use brownnote_native::{
    adapters::soundcraft::{COMMIT, PIN, Report, Request, SamplerMapping, VERSION, convert},
    edit::EditModel,
    export::{
        adapter::{self, ProjectFrozenHit},
        bundle::{BundleRequest, SystemWriter},
        manifest::{
            AssetEntry, AssetId, AssetKind, AudioRole, EventEntry, EventId, LossRecord,
            MANIFEST_FORMAT_VERSION, Manifest, PartEntry, PartId, PartKind, TimeOrigin, Timing,
        },
    },
    pedals::offline::{Backend, Hash256, Plugin, Stage},
    performance::{
        Channel, InstrumentId, LoopBars, NoteKey, OriginalTime, Pitch, SourceId, TakeId, Target,
        Tempo, Velocity,
    },
    project::{
        self, Asset, AssetRef, Document, MediaKind, PadId, SampleAssignment,
        live::{LiveInstrumentDocument, LiveTakeDocument},
        store::{MediaSource, Store},
    },
    takes::{PassOptions, Takes},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    io::{BufRead, BufReader, Cursor},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
type TestResult = Result<(), Box<dyn Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    request: Request,
    manifest: Manifest,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = std::env::temp_dir().join(format!(
            "brownnote soundcraft {}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        let bundle = root.join("bundle");
        fs::create_dir_all(bundle.join("media"))?;
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut wav = hound::WavWriter::new(
                &mut cursor,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 48_000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )?;
            for i in 0..48_000 {
                wav.write_sample(if i == 0 { 20_000i16 } else { 0 })?;
            }
            wav.finalize()?;
        }
        let bytes = cursor.into_inner();
        let asset = AssetId::of(&bytes);
        fs::write(bundle.join(asset.relative_path()), &bytes)?;
        let mut manifest = Manifest::new(
            TimeOrigin {
                nanos: 9_000_000_000,
            },
            Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        );
        manifest.assets.push(AssetEntry {
            id: asset,
            kind: AssetKind::RetainedRender,
            bytes: bytes.len() as u64,
            path: asset.relative_path(),
        });
        manifest.parts = vec![
            PartEntry {
                id: PartId(1),
                kind: PartKind::PadClips { pad: 3 },
            },
            PartEntry {
                id: PartId(2),
                kind: PartKind::InstrumentNotes { instrument: 7 },
            },
        ];
        for (sequence, start) in [(0, 0), (1, 125_000_000)] {
            manifest.events.push(EventEntry {
                id: EventId { take: 5, sequence },
                part: PartId(1),
                start_nanos: start,
                duration_nanos: 1_000_000_000,
                pad: Some(3),
                pitch: None,
                channel: None,
                velocity: Some(100),
                asset: Some(asset),
                source: Some(asset),
                variant_of: None,
            });
        }
        for (sequence, start, duration, velocity) in [
            (2, 1_000_000_000, 500_000_000, 45),
            (3, 1_125_000_000, 625_000_000, 99),
        ] {
            manifest.events.push(EventEntry {
                id: EventId { take: 5, sequence },
                part: PartId(2),
                start_nanos: start,
                duration_nanos: duration,
                pad: None,
                pitch: Some(60),
                channel: Some(0),
                velocity: Some(velocity),
                asset: None,
                source: None,
                variant_of: None,
            });
        }
        manifest.losses.push(LossRecord::UnsupportedMapping {
            event: manifest.events[1].id,
            reason: "choke_group retained as baked audio".into(),
        });
        fs::write(bundle.join("manifest.json"), manifest.encode()?)?;
        for name in ["soundcraft", "soundcraft-cli"] {
            let path = root.join(name);
            fs::write(&path, include_str!("fixtures/soundcraft_adapter/fake.py"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            }
        }
        fs::write(root.join("config.json"), b"{}")?;
        Ok(Self {
            request: Request {
                bundle,
                app: root.join("soundcraft"),
                cli: root.join("soundcraft-cli"),
                output: root.join("output session"),
                mappings: Vec::new(),
                timeout: Duration::from_secs(5),
            },
            root,
            manifest,
        })
    }
    fn config(&self, value: Value) -> TestResult {
        fs::write(self.root.join("config.json"), serde_json::to_vec(&value)?)?;
        Ok(())
    }
    fn calls(&self) -> Result<Vec<Value>, Box<dyn Error>> {
        let text = fs::read_to_string(self.root.join("calls.jsonl"))?;
        text.lines()
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }
    fn save(&self) -> TestResult {
        fs::write(
            self.request.bundle.join("manifest.json"),
            self.manifest.encode()?,
        )?;
        Ok(())
    }
    fn mapping(&self) -> SamplerMapping {
        SamplerMapping {
            part: PartId(2),
            asset: self.manifest.assets[0].id,
            root_note: 60,
            key_low: 60,
            key_high: 72,
            key_track: true,
            one_shot: true,
            choke_group: None,
        }
    }
    fn no_output(&self) {
        assert!(!self.request.output.exists());
        assert!(
            !self
                .root
                .join(".output session.soundcraft-staging")
                .exists()
        );
    }
}

#[test]
fn cli_invocation_keeps_overlaps_notes_tempo_and_relocatable_media() -> TestResult {
    let fixture = Fixture::new()?;
    let output = Command::new(env!("CARGO_BIN_EXE_brownnote-soundcraft"))
        .arg("--bundle")
        .arg(&fixture.request.bundle)
        .arg("--app")
        .arg(&fixture.request.app)
        .arg("--cli")
        .arg(&fixture.request.cli)
        .arg("--output")
        .arg(&fixture.request.output)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Report = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report.soundcraft_commit, COMMIT);
    assert_eq!(report.audio.len(), 2);
    assert_ne!(report.audio[0].track, report.audio[1].track);
    assert_eq!((report.audio[0].start, report.audio[1].start), (0, 6000));
    assert!(report.audio.iter().all(|clip| clip.length == 48_000));
    let notes = &report.midi[0].notes;
    assert_eq!(
        (
            notes[0].start,
            notes[0].length,
            notes[0].pitch,
            notes[0].velocity
        ),
        (1920, 960, 60, 45)
    );
    assert_eq!(
        (notes[1].start, notes[1].length, notes[1].velocity),
        (2160, 1200, 99)
    );
    assert_eq!(report.bundle_losses, fixture.manifest.losses);
    assert!(
        report
            .records
            .iter()
            .any(|r| r.detail.contains("No mapping inferred"))
    );
    let calls = fixture.calls()?;
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.get("launch_pid").is_some())
            .count(),
        2
    );
    assert!(
        calls
            .iter()
            .any(|c| c["method"] == "event.tempo" && c["params"]["bpm"] == 120.0)
    );
    assert!(
        calls
            .iter()
            .filter(|c| c["method"] == "file.import_audio")
            .all(|c| c["params"]["new_tracks"] == true && c["params"].get("track").is_none())
    );
    let relocated = fixture.root.join("relocated");
    fs::rename(&fixture.request.output, &relocated)?;
    fs::remove_dir_all(&fixture.request.bundle)?;
    assert!(relocated.join("session.scraft").is_file());
    assert!(relocated.join("adapter-report.json").is_file());
    assert!(relocated.join("soundcraft-pin.json").is_file());
    for asset in &fixture.manifest.assets {
        assert_eq!(
            AssetId::of(&fs::read(relocated.join(asset.id.relative_path()))?),
            asset.id
        );
        assert!(
            relocated
                .join("Audio Files")
                .join(format!("{}.wav", asset.id))
                .is_file()
        );
    }
    Ok(())
}

#[test]
fn malformed_manifest_notes_and_media_fail_before_executing_anything() -> TestResult {
    for bad in ["json", "version", "pitch", "duration", "media", "missing"] {
        let mut fixture = Fixture::new()?;
        match bad {
            "json" => fs::write(fixture.request.bundle.join("manifest.json"), b"garbage")?,
            "version" => fs::write(
                fixture.request.bundle.join("manifest.json"),
                b"{\"version\":2}",
            )?,
            "pitch" => {
                fixture.manifest.events[2].pitch = Some(128);
                fixture.save()?;
            }
            "duration" => {
                fixture.manifest.events[2].duration_nanos = u64::MAX;
                fixture.save()?;
            }
            "media" => fs::write(
                fixture
                    .request
                    .bundle
                    .join(fixture.manifest.assets[0].path.clone()),
                b"corrupt",
            )?,
            "missing" => fs::remove_file(
                fixture
                    .request
                    .bundle
                    .join(fixture.manifest.assets[0].path.clone()),
            )?,
            _ => unreachable!(),
        }
        assert!(convert(&fixture.request).is_err(), "{bad}");
        assert!(!fixture.root.join("calls.jsonl").exists(), "{bad}");
        fixture.no_output();
    }
    Ok(())
}

#[test]
fn rejects_each_unknown_executable_version_without_launching_gui() -> TestResult {
    for role in ["soundcraft", "soundcraft-cli"] {
        let fixture = Fixture::new()?;
        fixture.config(json!({format!("{role}_version"): "0.18.0"}))?;
        let error = convert(&fixture.request)
            .err()
            .ok_or("accepted mismatched pin")?;
        assert!(error.to_string().contains("expected"));
        assert!(
            fixture
                .calls()?
                .iter()
                .all(|c| c.get("launch_pid").is_none())
        );
        fixture.no_output();
    }
    Ok(())
}

#[test]
fn explicit_sampler_maps_are_optional_approximations_and_choke_is_unsupported() -> TestResult {
    for choke in [None, Some(1)] {
        let mut fixture = Fixture::new()?;
        let mut mapping = fixture.mapping();
        mapping.choke_group = choke;
        fixture.request.mappings.push(mapping);
        let report = convert(&fixture.request)?;
        let calls = fixture.calls()?;
        let loads: Vec<_> = calls
            .iter()
            .filter(|c| c["method"] == "mix.sampler_load")
            .collect();
        assert_eq!(loads.len(), usize::from(choke.is_none()));
        assert!(
            report
                .records
                .iter()
                .any(|r| r.detail.contains(if choke.is_none() {
                    "Explicit single-sample"
                } else {
                    "no choke_group support"
                }))
        );
        if let Some(load) = loads.first() {
            assert_eq!(
                load["params"]["params"],
                json!({"root_note":60,"key_low":60,"key_high":72,"key_track":1,"play_mode":1})
            );
        }
        assert_eq!(report.audio.len(), 2);
    }
    Ok(())
}

#[test]
fn partial_process_protocol_and_timeout_failures_never_publish() -> TestResult {
    for (kind, method) in [
        ("fail", "midi.note_add"),
        ("fail", "session.save_as"),
        ("fail", "session.open"),
        ("malformed", "file.import_audio"),
        ("envelope_error", "event.tempo"),
        ("hang", "event.tempo"),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.config(json!({kind: method}))?;
        if kind == "hang" {
            fixture.request.timeout = Duration::from_millis(500);
        }
        let error = convert(&fixture.request)
            .err()
            .ok_or("accepted injected failure")?;
        if kind == "fail" {
            assert!(error.to_string().contains("injected partial failure"));
        }
        fixture.no_output();
    }
    Ok(())
}

#[test]
fn reopen_inspection_rejects_changed_clips_notes_media_or_mutes() -> TestResult {
    for corrupt in ["position", "notes", "media", "mute"] {
        let fixture = Fixture::new()?;
        fixture.config(json!({"corrupt":corrupt}))?;
        assert!(convert(&fixture.request).is_err(), "{corrupt}");
        fixture.no_output();
    }
    Ok(())
}

#[test]
fn existing_output_and_staging_are_never_adopted_or_deleted() -> TestResult {
    for name in ["output session", ".output session.soundcraft-staging"] {
        let fixture = Fixture::new()?;
        let existing = fixture.root.join(name);
        fs::create_dir(&existing)?;
        fs::write(existing.join("user work"), b"preserve")?;
        assert!(convert(&fixture.request).is_err());
        assert_eq!(fs::read(existing.join("user work"))?, b"preserve");
    }
    Ok(())
}

#[test]
fn rounded_timing_and_full_asset_duration_are_reported_not_hidden() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.manifest.events[0].start_nanos = 1;
    fixture.manifest.events[0].duration_nanos = 50_000_000;
    fixture.manifest.events[2].start_nanos += 1;
    fixture.save()?;
    let report = convert(&fixture.request)?;
    assert!(
        report
            .records
            .iter()
            .any(|r| r.detail.contains("Audio onset rounded"))
    );
    assert!(
        report
            .records
            .iter()
            .any(|r| r.detail.contains("MIDI onset/end rounded"))
    );
    assert!(
        report
            .records
            .iter()
            .any(|r| r.detail.contains("not trimmed"))
    );
    assert_eq!(report.audio[0].length, 48_000);
    Ok(())
}

fn debug(error: impl std::fmt::Debug) -> Box<dyn Error> {
    format!("{error:?}").into()
}

fn float_wav(channels: u16, values: &[f32]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut wav = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?;
        for value in values {
            wav.write_sample(*value)?;
        }
        wav.finalize()?;
    }
    Ok(cursor.into_inner())
}

/// A bundle written by the real #109 project/takes/edit path, not a hand-built
/// manifest: two overlapping hits on one pad (a third deleted, one nudged) and
/// three overlapping instrument notes, two with frozen project renders.
fn writer_bundle(root: &std::path::Path) -> Result<PathBuf, Box<dyn Error>> {
    writer_bundle_with_master(root, 0.8)
}

fn writer_bundle_with_master(
    root: &std::path::Path,
    master: f32,
) -> Result<PathBuf, Box<dyn Error>> {
    let decay: Vec<f32> = (0..12_000)
        .map(|i| 0.6 * (1.0 - i as f32 / 12_000.0))
        .collect();
    let source = float_wav(1, &decay)?;
    let frozen = float_wav(2, &[0.2, -0.1].repeat(24_000))?;
    let media: BTreeMap<_, _> = [source.clone(), frozen.clone()]
        .into_iter()
        .map(|bytes| (project::AssetId::of(&bytes), bytes))
        .collect();
    let source_id = project::AssetId::of(&source);
    let frozen_id = project::AssetId::of(&frozen);
    let mut document = Document {
        assets: media
            .iter()
            .map(|(id, bytes)| Asset {
                reference: AssetRef {
                    id: *id,
                    bytes: bytes.len() as u64,
                },
                kind: if *id == source_id {
                    MediaKind::Source
                } else {
                    MediaKind::RetainedRender
                },
            })
            .collect(),
        master,
        ..Document::default()
    };
    document.pads[3].sample = Some(SampleAssignment {
        source: source_id,
        retained: None,
    });
    // Without choke both full hits overlap; choke baking is covered by #109.
    document.choke = false;
    let project_dir = root.join("project");
    let originals = root.join("original media");
    fs::create_dir_all(&originals)?;
    let mut sources = Vec::new();
    for (id, bytes) in &media {
        let path = originals.join(format!("{id}.wav"));
        fs::write(&path, bytes)?;
        sources.push(MediaSource { id: *id, path });
    }
    Store::new(project_dir.clone())
        .save(&document, &sources)
        .map_err(debug)?;
    fs::remove_dir_all(&originals)?;

    let mut takes = Takes::new(
        LoopBars::try_from(1).map_err(debug)?,
        TakeId(u128::from(u64::MAX) + 9),
    );
    takes
        .set_tempo(Tempo::try_from(120_000).map_err(debug)?)
        .map_err(debug)?;
    let take = takes.arm(PassOptions::default()).map_err(debug)?;
    takes.start(take).map_err(debug)?;
    let pad = Target::Pad(PadId::try_from(3).map_err(debug)?);
    let instrument = Target::Instrument(InstrumentId(77));
    let ms = 1_000_000;
    let specs = [
        (pad, 36, 127, 100 * ms, 130 * ms),
        (pad, 36, 90, 150 * ms, 180 * ms),
        (pad, 36, 80, 300 * ms, 330 * ms), // deleted
        (instrument, 60, 91, 500 * ms, 1_000 * ms),
        (instrument, 60, 72, 625 * ms, 1_250 * ms),
        (instrument, 64, 88, 750 * ms, 937_500_000), // no frozen render
    ];
    let key = |pitch| -> Result<NoteKey, Box<dyn Error>> {
        Ok(NoteKey {
            source: SourceId(5),
            channel: Channel::try_from(0).map_err(debug)?,
            pitch: Pitch::try_from(pitch).map_err(debug)?,
        })
    };
    let mut actions: Vec<(u64, bool, usize)> = Vec::new();
    for (index, spec) in specs.iter().enumerate() {
        actions.push((spec.3, false, index));
        actions.push((spec.4, true, index));
    }
    actions.sort();
    let mut ids = vec![None; specs.len()];
    for (time, off, index) in actions {
        let (target, pitch, velocity, _, _) = specs[index];
        let time = OriginalTime::from_nanos(time);
        if off {
            takes.note_off(take, key(pitch)?, time).map_err(debug)?;
        } else {
            let velocity = Velocity::try_from(velocity).map_err(debug)?;
            ids[index] = Some(
                takes
                    .note_on(take, target, key(pitch)?, velocity, time)
                    .map_err(debug)?,
            );
        }
    }
    takes.finish(take).map_err(debug)?;
    let ids = ids
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or("missing note id")?;
    let mut edits = EditModel::new();
    edits.nudge(&takes, ids[1], 5_000_000).map_err(debug)?;
    edits.delete(&takes, ids[2]).map_err(debug)?;
    let frozen_hits = ids[3..5]
        .iter()
        .map(|id| {
            (
                *id,
                ProjectFrozenHit {
                    wet: frozen_id,
                    dry: None,
                    detail: "frozen instrument render".into(),
                },
            )
        })
        .collect();
    let bundle = root.join("writer bundle");
    adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &BundleRequest {
            source_root: project_dir,
            destination: bundle.clone(),
            origin: TimeOrigin { nanos: 0 },
            timing: Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        },
        &takes,
        &edits,
        &frozen_hits,
    )
    .map_err(debug)?;
    Ok(bundle)
}

/// A 100 ms mono PCM16 source at any native-supported rate.
fn pcm16_wav(rate: u32, frames: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut wav = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )?;
        for i in 0..frames {
            let value = (12_000.0
                * (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / rate as f32).sin())
                as i16;
            wav.write_sample(value)?;
        }
        wav.finalize()?;
    }
    Ok(cursor.into_inner())
}

/// A real Store -> Takes -> writer bundle with one pad source at `rate`, two
/// hits, and the project's static master setting.
fn writer_pad_bundle(
    root: &std::path::Path,
    rate: u32,
    master: f32,
) -> Result<PathBuf, Box<dyn Error>> {
    let source = pcm16_wav(rate, (rate / 10) as usize)?;
    let id = project::AssetId::of(&source);
    let mut document = Document {
        assets: vec![Asset {
            reference: AssetRef {
                id,
                bytes: source.len() as u64,
            },
            kind: MediaKind::Source,
        }],
        master,
        ..Document::default()
    };
    document.pads[3].sample = Some(SampleAssignment {
        source: id,
        retained: None,
    });
    document.choke = false;
    let project_dir = root.join(format!("rate-{rate}-project"));
    let originals = root.join(format!("rate-{rate}-originals"));
    fs::create_dir_all(&originals)?;
    let path = originals.join("source.wav");
    fs::write(&path, &source)?;
    Store::new(project_dir.clone())
        .save(&document, &[MediaSource { id, path }])
        .map_err(debug)?;
    fs::remove_dir_all(&originals)?;

    let mut takes = Takes::new(
        LoopBars::try_from(1).map_err(debug)?,
        TakeId(u128::from(u64::MAX) + 21),
    );
    takes
        .set_tempo(Tempo::try_from(120_000).map_err(debug)?)
        .map_err(debug)?;
    let take = takes.arm(PassOptions::default()).map_err(debug)?;
    takes.start(take).map_err(debug)?;
    let key = NoteKey {
        source: SourceId(5),
        channel: Channel::try_from(0).map_err(debug)?,
        pitch: Pitch::try_from(36).map_err(debug)?,
    };
    let velocity = Velocity::try_from(127).map_err(debug)?;
    for start in [0, 62_500_000] {
        takes
            .note_on(
                take,
                Target::Pad(PadId::try_from(3).map_err(debug)?),
                key,
                velocity,
                OriginalTime::from_nanos(start),
            )
            .map_err(debug)?;
        takes
            .note_off(take, key, OriginalTime::from_nanos(start + 10_000_000))
            .map_err(debug)?;
    }
    takes.finish(take).map_err(debug)?;
    let bundle = root.join(format!("rate-{rate}-master-{master}-bundle"));
    adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &BundleRequest {
            source_root: project_dir,
            destination: bundle.clone(),
            origin: TimeOrigin { nanos: 0 },
            timing: Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        },
        &takes,
        &EditModel::new(),
        &BTreeMap::new(),
    )
    .map_err(debug)?;
    Ok(bundle)
}

/// A real Store -> Takes -> writer bundle with a zero-length one-shot pad hit
/// and instrument notes whose gates are zero and sub-tick.
fn writer_gesture_bundle(root: &std::path::Path) -> Result<PathBuf, Box<dyn Error>> {
    let decay: Vec<f32> = (0..12_000)
        .map(|i| 0.6 * (1.0 - i as f32 / 12_000.0))
        .collect();
    let source = float_wav(1, &decay)?;
    let id = project::AssetId::of(&source);
    let mut document = Document {
        assets: vec![Asset {
            reference: AssetRef {
                id,
                bytes: source.len() as u64,
            },
            kind: MediaKind::Source,
        }],
        ..Document::default()
    };
    document.pads[3].sample = Some(SampleAssignment {
        source: id,
        retained: None,
    });
    document.choke = false;
    let project_dir = root.join("gesture project");
    let originals = root.join("gesture originals");
    fs::create_dir_all(&originals)?;
    let path = originals.join("source.wav");
    fs::write(&path, &source)?;
    Store::new(project_dir.clone())
        .save(&document, &[MediaSource { id, path }])
        .map_err(debug)?;
    fs::remove_dir_all(&originals)?;

    let mut takes = Takes::new(
        LoopBars::try_from(1).map_err(debug)?,
        TakeId(u128::from(u64::MAX) + 22),
    );
    takes
        .set_tempo(Tempo::try_from(120_000).map_err(debug)?)
        .map_err(debug)?;
    let take = takes.arm(PassOptions::default()).map_err(debug)?;
    takes.start(take).map_err(debug)?;
    let key = |pitch| -> Result<NoteKey, Box<dyn Error>> {
        Ok(NoteKey {
            source: SourceId(6),
            channel: Channel::try_from(0).map_err(debug)?,
            pitch: Pitch::try_from(pitch).map_err(debug)?,
        })
    };
    let velocity = Velocity::try_from(100).map_err(debug)?;
    let pad_key = key(36)?;
    takes
        .note_on(
            take,
            Target::Pad(PadId::try_from(3).map_err(debug)?),
            pad_key,
            velocity,
            OriginalTime::from_nanos(100_000_000),
        )
        .map_err(debug)?;
    takes
        .note_off(take, pad_key, OriginalTime::from_nanos(100_000_000))
        .map_err(debug)?;
    for (pitch, on, off) in [
        (60, 500_000_000, 500_000_000),
        (64, 625_000_000, 625_100_000),
    ] {
        let key = key(pitch)?;
        takes
            .note_on(
                take,
                Target::Instrument(InstrumentId(77)),
                key,
                velocity,
                OriginalTime::from_nanos(on),
            )
            .map_err(debug)?;
        takes
            .note_off(take, key, OriginalTime::from_nanos(off))
            .map_err(debug)?;
    }
    takes.finish(take).map_err(debug)?;
    let bundle = root.join("gesture bundle");
    adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &BundleRequest {
            source_root: project_dir,
            destination: bundle.clone(),
            origin: TimeOrigin { nanos: 0 },
            timing: Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        },
        &takes,
        &EditModel::new(),
        &BTreeMap::new(),
    )
    .map_err(debug)?;
    Ok(bundle)
}

fn wav_peak(path: &std::path::Path) -> Result<f32, Box<dyn Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let peak = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .map(|s| s.map(f32::abs))
            .try_fold(0.0_f32, |peak, value| value.map(|v| peak.max(v)))?,
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| (f32::from(v) / 32768.0).abs()))
            .try_fold(0.0_f32, |peak, value| value.map(|v| peak.max(v)))?,
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| (v as f32 / 2_147_483_648.0).abs()))
            .try_fold(0.0_f32, |peak, value| value.map(|v| peak.max(v)))?,
        _ => return Err("unexpected WAV format".into()),
    };
    Ok(peak)
}

/// Peak of the two imported clips summed at their saved frame positions and
/// scaled by the target's saved track volumes.
fn imported_peak(output: &std::path::Path, session: &Value) -> Result<f32, Box<dyn Error>> {
    let mut mix = Vec::<f32>::new();
    for track in session["tracks"]
        .as_array()
        .ok_or("session tracks missing")?
    {
        let gain =
            10_f64.powf(track["volume_db"].as_f64().ok_or("track volume missing")? / 20.0) as f32;
        for clip in track["clips"].as_array().ok_or("clips missing")? {
            let start = clip["start"].as_u64().ok_or("clip start")? as usize;
            let length = clip["length"].as_u64().ok_or("clip length")? as usize;
            let source = clip["source"].as_u64().ok_or("clip source")?;
            let file = session["sources"]
                .as_array()
                .ok_or("sources missing")?
                .iter()
                .find(|entry| entry["id"].as_u64() == Some(source))
                .and_then(|entry| entry["path"].as_str())
                .ok_or("clip source missing")?;
            let mut reader = hound::WavReader::open(output.join(file))?;
            let channels = reader.spec().channels as usize;
            let samples: Vec<f32> = reader.samples::<f32>().collect::<Result<_, _>>()?;
            let frames = samples.len() / channels;
            let peak_end = (start + length.min(frames)) * 2;
            if mix.len() < peak_end {
                mix.resize(peak_end, 0.0);
            }
            for (index, sample) in samples.chunks(channels).enumerate() {
                let frame = start + index;
                for channel in 0..2 {
                    let value = if channels == 1 {
                        sample[0]
                    } else {
                        sample[channel]
                    };
                    mix[frame * 2 + channel] += value * gain;
                }
            }
        }
    }
    Ok(mix
        .into_iter()
        .fold(0.0_f32, |peak, value| peak.max(value.abs())))
}

fn control_call(
    cli: &Path,
    port: u16,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn Error>> {
    let output = Command::new(cli)
        .args([
            "app",
            "--port",
            &port.to_string(),
            method,
            &params.to_string(),
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "{method} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    if value.get("ok").is_some() {
        if value["ok"] != true {
            return Err(format!("{method}: {}", value["error"]).into());
        }
        return Ok(value["result"].clone());
    }
    Ok(value)
}

fn start_pinned_app(app: &Path, profile: &Path) -> Result<(Child, u16), Box<dyn Error>> {
    fs::create_dir_all(profile)?;
    let mut child = Command::new(app)
        .args(["--no-audio", "--control", "0"])
        .env("SOUNDCRAFT_NO_PREFS", "1")
        .env("SOUNDCRAFT_CONFIG_DIR", profile)
        .env_remove("SOUNDCRAFT_CONTROL_PORT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("missing app stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err("app exited before announcing its control port".into());
        }
        if let Some(port) = line.trim().strip_prefix("SOUNDCRAFT_CONTROL_PORT=") {
            return Ok((child, port.parse()?));
        }
    }
}

/// Offline bounce of the adapter's saved session through the pinned GUI.
fn bounce_with_pinned_app(
    app: &Path,
    cli: &Path,
    session: &Path,
    destination: &Path,
    frames: u64,
    profile: &Path,
) -> Result<(), Box<dyn Error>> {
    let (mut child, port) = start_pinned_app(app, profile)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        control_call(cli, port, "session.open", json!({"path": session}))?;
        control_call(
            cli,
            port,
            "file.bounce_mix",
            json!({
                "path": destination,
                "start": 0,
                "end": frames,
                "bit_depth": 32,
                "dither": false
            }),
        )?;
        Ok(())
    })();
    let quit = control_call(cli, port, "app.quit", json!({}));
    let status = child.wait()?;
    result?;
    quit?;
    if !status.success() {
        return Err("pinned app exited unsuccessfully".into());
    }
    Ok(())
}

/// A bundle written by the real export path for a project whose take was
/// played through a live CLAP instrument: the retained take audio plus that
/// take's notes, pinned to the plugin identity.
fn live_take_bundle(root: &std::path::Path) -> Result<(PathBuf, Plugin), Box<dyn Error>> {
    let take: Vec<f32> = (0..2_400)
        .flat_map(|frame| {
            let value = 0.3 * (frame as f32 / 48_000.0 * 300.0 * std::f32::consts::TAU).sin();
            [value, -value]
        })
        .collect();
    let audio = float_wav(2, &take)?;
    let id = project::AssetId::of(&audio);
    let plugin = Plugin {
        backend: Backend::Clap,
        id: "org.brownnote.fixture.live".into(),
        artifact: Hash256::of(b"fixture live bundle"),
        abi: "clap-1.2".into(),
        version: "0.4.0".into(),
    };
    let stage = Stage {
        plugin: plugin.clone(),
        location: PathBuf::from("/machine/never-installed.clap"),
        params: Vec::new(),
        state: Some(vec![1, 2, 3].into()),
    };
    let mut live = LiveInstrumentDocument::from_stage(&stage, "Fixture Live".into(), true);
    live.takes.push(LiveTakeDocument {
        take: "12".into(),
        asset: id,
        rate: 48_000,
        frames: 2_400,
    });
    let document = Document {
        assets: vec![Asset {
            reference: AssetRef::of(&audio),
            kind: MediaKind::LiveTakeRender,
        }],
        live_instrument: Some(live),
        ..Document::default()
    };
    let project_dir = root.join("live project");
    let originals = root.join("live original media");
    fs::create_dir_all(&originals)?;
    let path = originals.join("take.wav");
    fs::write(&path, &audio)?;
    Store::new(project_dir.clone())
        .save(&document, &[MediaSource { id, path }])
        .map_err(debug)?;
    fs::remove_dir_all(&originals)?;

    let key = |channel, pitch| -> Result<NoteKey, Box<dyn Error>> {
        Ok(NoteKey {
            source: SourceId(11),
            channel: Channel::try_from(channel).map_err(debug)?,
            pitch: Pitch::try_from(pitch).map_err(debug)?,
        })
    };
    let mut takes = Takes::new(LoopBars::try_from(4).map_err(debug)?, TakeId(12));
    takes
        .set_tempo(Tempo::try_from(60_000).map_err(debug)?)
        .map_err(debug)?;
    let pass = takes.arm(PassOptions::default()).map_err(debug)?;
    takes.start(pass).map_err(debug)?;
    let ms = 1_000_000;
    for (channel, pitch, velocity, on, off) in [
        (0, 60, 100, 200 * ms, 300 * ms),
        (0, 64, 90, 500 * ms, 800 * ms),
    ] {
        takes
            .note_on(
                pass,
                Target::Instrument(InstrumentId(1)),
                key(channel, pitch)?,
                Velocity::try_from(velocity).map_err(debug)?,
                OriginalTime::from_nanos(on),
            )
            .map_err(debug)?;
        takes
            .note_off(pass, key(channel, pitch)?, OriginalTime::from_nanos(off))
            .map_err(debug)?;
    }
    takes.finish(pass).map_err(debug)?;
    takes
        .set_tempo(Tempo::try_from(120_000).map_err(debug)?)
        .map_err(debug)?;
    let bundle = root.join("live writer bundle");
    adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &BundleRequest {
            source_root: project_dir,
            destination: bundle.clone(),
            origin: TimeOrigin { nanos: 0 },
            timing: Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        },
        &takes,
        &EditModel::default(),
        &BTreeMap::new(),
    )
    .map_err(debug)?;
    Ok((bundle, plugin))
}

/// A manifest v3 bundle from the real export path for one live-instrument pass:
/// the take's retained audio, the notes that pass played and the pad hit it
/// played from the same take. The pad's own signal is the pad mixer's, not the
/// plugin's capture, so it is a pad clip beside the take, never a note.
fn live_take_with_hit_bundle(root: &std::path::Path) -> Result<(PathBuf, Plugin), Box<dyn Error>> {
    let take: Vec<f32> = (0..2_400)
        .flat_map(|frame| {
            let value = 0.3 * (frame as f32 / 48_000.0 * 300.0 * std::f32::consts::TAU).sin();
            [value, -value]
        })
        .collect();
    let audio = float_wav(2, &take)?;
    let pad: Vec<f32> = (0..24_000)
        .map(|frame| 0.25 * (frame as f32 / 48_000.0 * 220.0 * std::f32::consts::TAU).sin())
        .collect();
    let pad_source = float_wav(1, &pad)?;
    let held: Vec<f32> = (0..36_000)
        .map(|frame| 0.5 * (frame as f32 / 48_000.0 * 110.0 * std::f32::consts::TAU).sin())
        .collect();
    let pad_retained = float_wav(1, &held)?;
    let id = project::AssetId::of(&audio);
    let pad_source_id = project::AssetId::of(&pad_source);
    let pad_retained_id = project::AssetId::of(&pad_retained);
    let plugin = Plugin {
        backend: Backend::Clap,
        id: "org.brownnote.fixture.live".into(),
        artifact: Hash256::of(b"fixture live take with a pad hit"),
        abi: "clap-1.2".into(),
        version: "0.4.0".into(),
    };
    let stage = Stage {
        plugin: plugin.clone(),
        location: PathBuf::from("/machine/never-installed.clap"),
        params: Vec::new(),
        state: Some(vec![1, 2, 3].into()),
    };
    let mut live = LiveInstrumentDocument::from_stage(&stage, "Fixture Live".into(), true);
    live.takes.push(LiveTakeDocument {
        take: "12".into(),
        asset: id,
        rate: 48_000,
        frames: 2_400,
    });
    let mut document = Document {
        assets: vec![
            Asset {
                reference: AssetRef::of(&audio),
                kind: MediaKind::LiveTakeRender,
            },
            Asset {
                reference: AssetRef::of(&pad_source),
                kind: MediaKind::Source,
            },
            Asset {
                reference: AssetRef::of(&pad_retained),
                kind: MediaKind::RetainedRender,
            },
        ],
        live_instrument: Some(live),
        ..Document::default()
    };
    document.pads[3].sample = Some(SampleAssignment {
        source: pad_source_id,
        retained: Some(pad_retained_id),
    });
    document.validate().map_err(debug)?;
    let project_dir = root.join("live hit project");
    let originals = root.join("live hit original media");
    fs::create_dir_all(&originals)?;
    let mut sources = Vec::new();
    for (asset, name, bytes) in [
        (id, "take.wav", &audio),
        (pad_source_id, "pad source.wav", &pad_source),
        (pad_retained_id, "pad retained.wav", &pad_retained),
    ] {
        let path = originals.join(name);
        fs::write(&path, bytes)?;
        sources.push(MediaSource { id: asset, path });
    }
    Store::new(project_dir.clone())
        .save(&document, &sources)
        .map_err(debug)?;
    fs::remove_dir_all(&originals)?;

    let key = |channel, pitch| -> Result<NoteKey, Box<dyn Error>> {
        Ok(NoteKey {
            source: SourceId(11),
            channel: Channel::try_from(channel).map_err(debug)?,
            pitch: Pitch::try_from(pitch).map_err(debug)?,
        })
    };
    let mut takes = Takes::new(LoopBars::try_from(4).map_err(debug)?, TakeId(12));
    takes
        .set_tempo(Tempo::try_from(60_000).map_err(debug)?)
        .map_err(debug)?;
    let pass = takes.arm(PassOptions::default()).map_err(debug)?;
    takes.start(pass).map_err(debug)?;
    let ms = 1_000_000;
    for (target, pitch, velocity, on, off) in [
        (
            Target::Instrument(InstrumentId(1)),
            60,
            100,
            200 * ms,
            300 * ms,
        ),
        (
            Target::Pad(PadId::try_from(3).map_err(debug)?),
            36,
            110,
            900 * ms,
            1_100 * ms,
        ),
    ] {
        takes
            .note_on(
                pass,
                target,
                key(0, pitch)?,
                Velocity::try_from(velocity).map_err(debug)?,
                OriginalTime::from_nanos(on),
            )
            .map_err(debug)?;
        takes
            .note_off(pass, key(0, pitch)?, OriginalTime::from_nanos(off))
            .map_err(debug)?;
    }
    takes.finish(pass).map_err(debug)?;
    takes
        .set_tempo(Tempo::try_from(120_000).map_err(debug)?)
        .map_err(debug)?;
    let bundle = root.join("live hit writer bundle");
    adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &BundleRequest {
            source_root: project_dir,
            destination: bundle.clone(),
            origin: TimeOrigin { nanos: 0 },
            timing: Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        },
        &takes,
        &EditModel::default(),
        &BTreeMap::new(),
    )
    .map_err(debug)?;
    Ok((bundle, plugin))
}

/// Independent expectations at 48 kHz / 120 BPM / 960 PPQ (1 s = 1,920 ticks).
fn assert_writer_bundle_report(report: &Report) {
    let mut audio: Vec<_> = report.audio.iter().map(|c| (c.start, c.length)).collect();
    audio.sort();
    // 100 ms, 155 ms (nudged) pad hits of 12,000 frames; 500/625 ms frozen renders.
    assert_eq!(
        audio,
        [
            (4_800, 12_000),
            (7_440, 12_000),
            (24_000, 24_000),
            (30_000, 24_000)
        ]
    );
    let tracks: BTreeSet<_> = report.audio.iter().map(|c| c.track).collect();
    assert_eq!(tracks.len(), 4);
    assert_eq!(report.midi.len(), 1);
    let notes: Vec<_> = report.midi[0]
        .notes
        .iter()
        .map(|n| (n.start, n.length, n.pitch, n.velocity))
        .collect();
    assert_eq!(
        notes,
        [
            (960, 960, 60, 91),
            (1_200, 1_200, 60, 72),
            (1_440, 360, 64, 88)
        ]
    );
    assert_eq!(
        report
            .records
            .iter()
            .filter(|r| r.detail.contains("No retained audio"))
            .count(),
        1
    );
    assert!(report.records.iter().any(|r| {
        r.detail
            .contains("reference mix stay in the retained bundle")
    }));
}

#[test]
fn real_writer_bundle_imports_every_effective_hit_and_note() -> TestResult {
    let fixture = Fixture::new()?;
    let bundle = writer_bundle(&fixture.root)?;
    let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
    let mut request = fixture.request.clone();
    request.bundle = bundle.clone();
    let report = convert(&request)?;
    assert_writer_bundle_report(&report);
    for clip in &report.audio {
        let hit = manifest
            .audio
            .iter()
            .find(|a| a.role == AudioRole::HitWet && a.event == Some(clip.event))
            .ok_or("hit without writer entry")?;
        assert_eq!(
            (hit.start_frame as i64, hit.frames as i64),
            (clip.start, clip.length)
        );
    }
    let midi = manifest.midi.as_ref().ok_or("writer bundle without MIDI")?;
    assert_eq!(
        fs::read(request.output.join(&midi.path))?,
        fs::read(bundle.join(&midi.path))?
    );
    assert_eq!(report.bundle_losses, manifest.losses);
    Ok(())
}

#[test]
fn writer_zero_length_one_shot_and_sub_tick_notes_convert() -> TestResult {
    let fixture = Fixture::new()?;
    let bundle = writer_gesture_bundle(&fixture.root)?;
    let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
    let pad = manifest
        .events
        .iter()
        .find(|event| event.pad.is_some())
        .ok_or("writer bundle without a pad hit")?;
    assert_eq!(pad.duration_nanos, 0);
    let mut request = fixture.request.clone();
    request.bundle = bundle;
    let report = convert(&request)?;
    // A zero-length one-shot still imports its full retained asset.
    assert_eq!(report.audio.len(), 1);
    assert_eq!(
        (report.audio[0].start, report.audio[0].length),
        (4_800, 12_000)
    );
    // Zero and positive sub-tick gates both use the writer's one-tick minimum.
    assert_eq!(report.midi.len(), 1);
    let notes: Vec<_> = report.midi[0]
        .notes
        .iter()
        .map(|note| (note.start, note.length, note.pitch))
        .collect();
    assert_eq!(notes, [(960, 1, 60), (1_200, 1, 64)]);
    assert!(
        report
            .records
            .iter()
            .any(|record| { record.detail.contains("one-tick minimum gate") })
    );
    assert!(
        report
            .records
            .iter()
            .any(|record| record.detail.contains("not trimmed"))
    );
    Ok(())
}

#[test]
fn source_only_media_accepts_every_native_rate_and_keeps_provenance() -> TestResult {
    for rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
        let fixture = Fixture::new()?;
        let bundle = writer_pad_bundle(&fixture.root, rate, 0.8)?;
        let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
        let source = manifest
            .events
            .iter()
            .find_map(|event| event.source)
            .ok_or("writer bundle lost its source provenance")?;
        let asset = manifest.asset(source).ok_or("missing source asset")?;
        let reader = hound::WavReader::open(bundle.join(&asset.path))?;
        assert_eq!(reader.spec().sample_rate, rate);
        let imported = manifest
            .events
            .iter()
            .find_map(|event| event.asset)
            .ok_or("writer bundle has no imported asset")?;
        let reader = hound::WavReader::open(bundle.join(&manifest.asset(imported).unwrap().path))?;
        assert_eq!(reader.spec().sample_rate, 48_000);
        let mut request = fixture.request.clone();
        request.bundle = bundle;
        let report = convert(&request)?;
        assert_eq!(report.audio.len(), 2, "source {rate}");
    }
    Ok(())
}

#[test]
fn unsupported_event_asset_rate_is_rejected_but_source_only_rates_are_not() -> TestResult {
    let fixture = Fixture::new()?;
    let bad = pcm16_wav(96_000, 9_600)?;
    let id = AssetId::of(&bad);
    fs::write(fixture.request.bundle.join(id.relative_path()), &bad)?;
    let mut manifest = fixture.manifest.clone();
    for event in &mut manifest.events {
        if event.asset.is_some() {
            event.asset = Some(id);
            event.source = Some(id);
        }
    }
    manifest.assets.push(AssetEntry {
        id,
        kind: AssetKind::RetainedRender,
        bytes: bad.len() as u64,
        path: id.relative_path(),
    });
    fs::write(
        fixture.request.bundle.join("manifest.json"),
        manifest.encode()?,
    )?;
    let error = convert(&fixture.request)
        .err()
        .ok_or("accepted a target-imported 96 kHz asset")?;
    assert!(error.to_string().contains("44.1/48k"), "{error}");
    fixture.no_output();
    Ok(())
}

#[test]
fn writer_master_gain_is_applied_once_and_matches_the_reference() -> TestResult {
    for master in [0.0_f32, 0.25, 0.8, 1.0] {
        let fixture = Fixture::new()?;
        let bundle = writer_pad_bundle(&fixture.root, 48_000, master)?;
        let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
        assert_eq!(manifest.master_gain, master);
        let mut request = fixture.request.clone();
        request.bundle = bundle.clone();
        let report = convert(&request)?;
        assert_eq!(report.audio.len(), 2);
        let expected_db = if master == 0.0 {
            -144.0
        } else {
            20.0 * f64::from(master).log10()
        };
        let calls = fixture.calls()?;
        let volumes: Vec<_> = calls
            .iter()
            .filter(|call| call["method"] == "mix.volume")
            .collect();
        if master == 1.0 {
            assert!(volumes.is_empty());
        } else {
            assert_eq!(volumes.len(), 2);
            for call in volumes {
                let db = call["params"]["db"].as_f64().ok_or("volume db")?;
                assert!((db - expected_db).abs() < 1e-6, "{db} vs {expected_db}");
            }
            let record = report
                .records
                .iter()
                .find(|record| record.detail.contains("Static master gain"))
                .ok_or("master gain change was not reported")?;
            assert!(record.detail.contains(&master.to_string()));
            if master == 0.0 {
                assert!(record.detail.contains("-144 dB"));
            }
        }
        // The target's saved per-track gains, applied to the imported clips,
        // reproduce the writer's reference peak once.
        let session: Value =
            serde_json::from_slice(&fs::read(fixture.request.output.join("session.scraft"))?)?;
        let actual = imported_peak(&fixture.request.output, &session)?;
        let reference = manifest
            .audio
            .iter()
            .find(|entry| entry.role == AudioRole::Reference)
            .ok_or("writer bundle without a reference")?;
        let expected = wav_peak(&bundle.join(&manifest.asset(reference.asset).unwrap().path))?;
        assert!(
            (actual - expected).abs() <= 1e-6,
            "master {master}: target {actual} vs reference {expected}"
        );
    }
    Ok(())
}

#[test]
fn unavailable_live_plugin_is_reported_and_the_retained_take_still_converts() -> TestResult {
    let fixture = Fixture::new()?;
    let (bundle, plugin) = live_take_bundle(&fixture.root)?;
    let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
    let mut request = fixture.request.clone();
    request.bundle = bundle.clone();
    let report = convert(&request)?;
    let part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::LiveInstrumentNotes { .. }))
        .ok_or("bundle without the live take's notes")?;
    let PartKind::LiveInstrumentNotes { plugin: identity } = &part.kind else {
        unreachable!()
    };
    assert_eq!(
        (identity.id.as_str(), identity.version.as_str()),
        (plugin.id.as_str(), plugin.version.as_str())
    );
    // A destination that cannot load the plugin names it instead of dropping
    // the notes or pretending the sampled instrument played them.
    let record = report
        .records
        .iter()
        .find(|record| record.part == Some(part.id) && record.kind == "unsupported")
        .ok_or("unavailable plugin mapping was not reported")?;
    assert!(record.detail.contains(&plugin.id), "{}", record.detail);
    assert!(record.detail.contains(&plugin.version), "{}", record.detail);
    assert!(record.detail.contains(&identity.artifact[..12]));
    assert!(record.detail.contains("2 live instrument note(s)"));
    assert!(record.detail.contains("retained audio is the fallback"));
    // Never a sampler/MIDI guess: no instrument track claims those notes.
    assert!(report.midi.is_empty());
    // The retained take audio still exports and is imported as its own clip.
    assert_eq!(report.audio.len(), 1);
    assert_eq!((report.audio[0].start, report.audio[0].length), (0, 2_400));
    assert!(report.records.iter().any(|record| {
        record
            .detail
            .contains("Independent full-length audio clip retained")
    }));
    // Identity, not a machine path, even though the plugin is not installed.
    let text = serde_json::to_string(&report)?;
    assert!(!text.contains("/machine/"), "{text}");
    assert!(!text.contains(&*fixture.root.to_string_lossy()));
    Ok(())
}

#[test]
fn disagreeing_writer_hit_alignment_is_rejected_before_publication() -> TestResult {
    let fixture = Fixture::new()?;
    let bundle = writer_bundle(&fixture.root)?;
    let path = bundle.join("manifest.json");
    let mut manifest = Manifest::decode(&fs::read(&path)?)?;
    let hit = manifest
        .audio
        .iter_mut()
        .find(|a| a.role == AudioRole::HitWet)
        .ok_or("no hit")?;
    hit.start_frame += 48;
    fs::write(&path, manifest.encode()?)?;
    let mut request = fixture.request.clone();
    request.bundle = bundle;
    let error = convert(&request).err().ok_or("accepted moved hit")?;
    assert!(error.to_string().contains("disagrees"), "{error}");
    fixture.no_output();
    Ok(())
}
#[test]
#[ignore = "requires explicit BROWNNOTE_SOUNDCRAFT_APP/CLI/PYTHON; launches the actual pinned GUI"]
fn pinned_app_recipe_and_adapter() -> TestResult {
    if std::env::var_os("BROWNNOTE_SOUNDCRAFT_RUN_PINNED").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err("set BROWNNOTE_SOUNDCRAFT_RUN_PINNED=1 explicitly".into());
    }
    let app = std::env::var_os("BROWNNOTE_SOUNDCRAFT_APP").ok_or("set BROWNNOTE_SOUNDCRAFT_APP")?;
    let cli = std::env::var_os("BROWNNOTE_SOUNDCRAFT_CLI").ok_or("set BROWNNOTE_SOUNDCRAFT_CLI")?;
    let python =
        std::env::var_os("BROWNNOTE_SOUNDCRAFT_PYTHON").ok_or("set BROWNNOTE_SOUNDCRAFT_PYTHON")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new(&python)
        .arg(root.join("tests/fixtures/soundcraft_adapter/pinned_recipe.py"))
        .arg(&app)
        .arg(&cli)
        .status()?;
    assert!(status.success(), "unchanged #95 probe failed");
    let mut fixture = Fixture::new()?;
    fixture.request.app = app.into();
    fixture.request.cli = cli.into();
    fixture.request.timeout = Duration::from_secs(45);
    fixture.request.mappings.push(fixture.mapping());
    let report = convert(&fixture.request)?;
    assert_eq!((report.audio.len(), report.midi[0].notes.len()), (2, 2));

    // A real #109 writer bundle, converted, then edited in the actual GUI.
    let bundle = writer_bundle(&fixture.root)?;
    let mut request = fixture.request.clone();
    request.bundle = bundle.clone();
    request.mappings.clear();
    request.output = fixture.root.join("writer output");
    let report = convert(&request)?;
    assert_writer_bundle_report(&report);
    let evidence = std::env::var_os("BROWNNOTE_SOUNDCRAFT_EDIT_EVIDENCE")
        .map_or_else(|| fixture.root.join("adapter-edit.json"), PathBuf::from);
    let status = Command::new(&python)
        .arg(root.join("tests/fixtures/soundcraft_adapter/edit_output.py"))
        .arg(&request.app)
        .arg(&request.cli)
        .arg(&request.output)
        .arg(&bundle)
        .arg(&evidence)
        .status()?;
    assert!(
        status.success(),
        "actual-app edit/save/reopen of adapter output failed"
    );
    Ok(())
}

/// The signal gate from #231: after conversion, an offline bounce of the
/// editable session must match the bundle's reference at every master setting,
/// including the default 0.8.
#[test]
#[ignore = "requires explicit BROWNNOTE_SOUNDCRAFT_APP/CLI; launches the actual pinned GUI"]
fn pinned_app_master_gain_bounce_matches_the_reference() -> TestResult {
    if std::env::var_os("BROWNNOTE_SOUNDCRAFT_RUN_PINNED").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err("set BROWNNOTE_SOUNDCRAFT_RUN_PINNED=1 explicitly".into());
    }
    let app = PathBuf::from(std::env::var_os("BROWNNOTE_SOUNDCRAFT_APP").ok_or("set APP")?);
    let cli = PathBuf::from(std::env::var_os("BROWNNOTE_SOUNDCRAFT_CLI").ok_or("set CLI")?);
    let pin: Value = serde_json::from_str(PIN)?;
    assert_eq!(
        pin["binary_sha256"]["SoundCraft"],
        Hash256::of(&fs::read(&app)?).to_string()
    );
    assert_eq!(
        pin["binary_sha256"]["soundcraft-cli"],
        Hash256::of(&fs::read(&cli)?).to_string()
    );

    for master in [0.0_f32, 0.25, 0.8, 1.0] {
        let fixture = Fixture::new()?;
        let bundle = writer_pad_bundle(&fixture.root, 48_000, master)?;
        let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
        let mut request = fixture.request.clone();
        request.app = app.clone();
        request.cli = cli.clone();
        request.timeout = Duration::from_secs(45);
        request.bundle = bundle.clone();
        let report = convert(&request)?;
        assert_eq!(report.audio.len(), 2);
        let reference = manifest
            .audio
            .iter()
            .find(|entry| entry.role == AudioRole::Reference)
            .ok_or("writer bundle without a reference")?;
        let expected = wav_peak(&bundle.join(&manifest.asset(reference.asset).unwrap().path))?;
        let bounce = fixture.root.join(format!("master-{master}-bounce.wav"));
        bounce_with_pinned_app(
            &app,
            &cli,
            &request.output.join("session.scraft"),
            &bounce,
            reference.frames,
            &fixture.root.join(format!("master-{master}-profile")),
        )?;
        let actual = wav_peak(&bounce)?;
        assert!(
            (actual - expected).abs() <= 2e-5,
            "master {master}: bounced {actual} vs reference {expected}"
        );
    }
    Ok(())
}

/// The pinned GUI accepts the writer's zero-length one-shot, its one-tick
/// minimum gates, and source-only provenance outside the target's import slice.
#[test]
#[ignore = "requires explicit BROWNNOTE_SOUNDCRAFT_APP/CLI; launches the actual pinned GUI"]
fn pinned_app_accepts_writer_minimum_gates_and_source_only_rates() -> TestResult {
    if std::env::var_os("BROWNNOTE_SOUNDCRAFT_RUN_PINNED").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err("set BROWNNOTE_SOUNDCRAFT_RUN_PINNED=1 explicitly".into());
    }
    let app = PathBuf::from(std::env::var_os("BROWNNOTE_SOUNDCRAFT_APP").ok_or("set APP")?);
    let cli = PathBuf::from(std::env::var_os("BROWNNOTE_SOUNDCRAFT_CLI").ok_or("set CLI")?);
    let pin: Value = serde_json::from_str(PIN)?;
    assert_eq!(
        pin["binary_sha256"]["SoundCraft"],
        Hash256::of(&fs::read(&app)?).to_string()
    );
    assert_eq!(
        pin["binary_sha256"]["soundcraft-cli"],
        Hash256::of(&fs::read(&cli)?).to_string()
    );

    let fixture = Fixture::new()?;
    let mut request = fixture.request.clone();
    request.app = app.clone();
    request.cli = cli.clone();
    request.timeout = Duration::from_secs(45);
    request.bundle = writer_gesture_bundle(&fixture.root)?;
    let report = convert(&request)?;
    assert_eq!(report.audio.len(), 1);
    assert_eq!(
        report.midi[0]
            .notes
            .iter()
            .map(|note| (note.start, note.length))
            .collect::<Vec<_>>(),
        [(960, 1), (1_200, 1)]
    );

    for rate in [8_000, 96_000, 192_000] {
        let fixture = Fixture::new()?;
        let mut request = fixture.request.clone();
        request.app = app.clone();
        request.cli = cli.clone();
        request.timeout = Duration::from_secs(45);
        request.bundle = writer_pad_bundle(&fixture.root, rate, 0.8)?;
        let report = convert(&request)?;
        assert_eq!(report.audio.len(), 2, "source {rate}");
    }
    Ok(())
}

/// #171's open decision, answered by the actual pinned app instead of the fake:
/// a manifest v3 bundle built by the live-take export path converts without
/// losing the take, names the plugin it cannot load, and never invents MIDI.
#[test]
#[ignore = "requires explicit BROWNNOTE_SOUNDCRAFT_APP/CLI/PYTHON; launches the actual pinned GUI"]
fn pinned_app_reports_v3_live_take_notes_and_imports_the_retained_audio() -> TestResult {
    if std::env::var_os("BROWNNOTE_SOUNDCRAFT_RUN_PINNED").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err("set BROWNNOTE_SOUNDCRAFT_RUN_PINNED=1 explicitly".into());
    }
    let app = std::env::var_os("BROWNNOTE_SOUNDCRAFT_APP").ok_or("set BROWNNOTE_SOUNDCRAFT_APP")?;
    let cli = std::env::var_os("BROWNNOTE_SOUNDCRAFT_CLI").ok_or("set BROWNNOTE_SOUNDCRAFT_CLI")?;
    // Check the bytes again here: a substituted app must never reach the GUI.
    let pin: Value = serde_json::from_str(PIN)?;
    let app_hash = Hash256::of(&fs::read(&app)?).to_string();
    let cli_hash = Hash256::of(&fs::read(&cli)?).to_string();
    assert_eq!(pin["binary_sha256"]["SoundCraft"], app_hash);
    assert_eq!(pin["binary_sha256"]["soundcraft-cli"], cli_hash);

    let mut fixture = Fixture::new()?;
    let (bundle, plugin) = live_take_with_hit_bundle(&fixture.root)?;
    let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
    assert_eq!(manifest.version, MANIFEST_FORMAT_VERSION);
    let live_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::LiveInstrumentNotes { .. }))
        .ok_or("bundle without the live take's notes")?;
    let PartKind::LiveInstrumentNotes { plugin: identity } = &live_part.kind else {
        unreachable!()
    };
    assert_eq!(
        (
            identity.backend.as_str(),
            identity.id.as_str(),
            identity.version.as_str(),
            identity.abi.as_str()
        ),
        (
            "clap",
            plugin.id.as_str(),
            plugin.version.as_str(),
            plugin.abi.as_str()
        )
    );
    assert!(plugin.artifact.to_string().starts_with(&identity.artifact));
    let note_events: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| event.part == live_part.id)
        .collect();
    assert_eq!(note_events.len(), 1);

    fixture.request.app = app.clone().into();
    fixture.request.cli = cli.clone().into();
    fixture.request.timeout = Duration::from_secs(45);
    fixture.request.bundle = bundle.clone();
    fixture.request.output = fixture.root.join("live take output");
    // No sampler mapping: the take's retained audio is the only fallback.
    assert!(fixture.request.mappings.is_empty());
    let report = convert(&fixture.request)?;
    assert_eq!(
        (
            report.soundcraft_version.as_str(),
            report.soundcraft_commit.as_str()
        ),
        (VERSION, COMMIT)
    );

    // An explicit unsupported mapping, naming the plugin by identity only.
    let record = report
        .records
        .iter()
        .find(|record| record.part == Some(live_part.id) && record.kind == "unsupported")
        .ok_or("unavailable live plugin mapping was not reported")?;
    assert_eq!(
        record.detail,
        format!(
            "{} live instrument note(s) kept in the manifest for {}; this target cannot load \
             that plugin or carry its identity into a track, so the notes are not imported as \
             MIDI. The take's retained audio is the fallback.",
            note_events.len(),
            identity.describe()
        )
    );
    assert!(record.detail.contains("clap"), "{}", record.detail);
    assert!(record.detail.contains(&plugin.id), "{}", record.detail);
    assert!(record.detail.contains(&plugin.version), "{}", record.detail);
    assert!(record.detail.contains(&plugin.abi), "{}", record.detail);
    // The short artifact hash, never the whole digest.
    assert!(record.detail.contains(&identity.artifact[..12]));
    assert!(!record.detail.contains(&identity.artifact));

    // The live notes never become a sampled-instrument MIDI track.
    assert!(report.midi.is_empty());

    // The retained take audio and the pad hit are separate audio clips.
    let take_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::SampleClips { .. }))
        .ok_or("bundle without the retained take clips")?;
    let take_event = manifest
        .events
        .iter()
        .find(|event| event.part == take_part.id)
        .ok_or("bundle without the take's audio clip")?;
    let pad_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::PadClips { pad: 3 }))
        .ok_or("bundle without the pad hit")?;
    let pad_event = manifest
        .events
        .iter()
        .find(|event| event.part == pad_part.id)
        .ok_or("bundle without the pad hit's event")?;
    assert_eq!((pad_event.pitch, pad_event.velocity), (Some(36), Some(110)));
    assert_eq!(report.audio.len(), 2);
    let retained = report
        .audio
        .iter()
        .find(|clip| clip.event == take_event.id)
        .ok_or("the retained take audio was not imported")?;
    assert_eq!((retained.start, retained.length), (0, 2_400));
    // 900 ms of the 60 BPM pass halved to 450 ms at 48 kHz, then the pad's
    // whole retained render: the adapter never trims a hit to its event.
    let hit = report
        .audio
        .iter()
        .find(|clip| clip.event == pad_event.id)
        .ok_or("the pad hit was not imported as its own audio clip")?;
    assert_eq!((hit.start, hit.length), (21_600, 36_000));
    assert_ne!(retained.track, hit.track);
    assert_ne!(retained.clip, hit.clip);

    // Identity, not a machine path, in every string the report carries.
    let text = serde_json::to_string(&report)?;
    assert!(!text.contains("/machine/"), "{text}");
    assert!(!text.contains("/Users/"), "{text}");
    assert!(!text.contains(&*fixture.root.to_string_lossy()), "{text}");

    let evidence = std::env::var_os("BROWNNOTE_SOUNDCRAFT_LIVE_EVIDENCE")
        .map_or_else(|| fixture.root.join("live-take.json"), PathBuf::from);
    let record_kinds: BTreeSet<_> = report.records.iter().map(|r| r.kind.clone()).collect();
    let payload = json!({
        "check": "manifest v3 live-instrument take and pad hit through the pinned app",
        "date": "2026-11-09",
        "pin": {"tag": pin["tag"], "commit": COMMIT, "version": VERSION},
        "binaries": {
            "SoundCraft": {"sha256": app_hash, "matches_pin": true},
            "soundcraft-cli": {"sha256": cli_hash, "matches_pin": true},
        },
        "launch": ["SoundCraft", "--no-audio", "--control", "0"],
        "bundle": {
            "manifest_version": manifest.version,
            "parts": manifest.parts.len(),
            "events": manifest.events.len(),
            "assets": manifest.assets.len(),
            "losses": manifest.losses.len(),
        },
        "report": {
            "soundcraft_version": report.soundcraft_version,
            "soundcraft_commit": report.soundcraft_commit,
            "audio_clips": report.audio.len(),
            "midi_parts": report.midi.len(),
            "record_kinds": record_kinds,
        },
        "live_notes": {
            "events": note_events.len(),
            "imported_as_midi": false,
            "plugin": identity,
            "record": record.detail,
        },
        "retained_take": {"track": retained.track, "clip": retained.clip,
                          "start": retained.start, "length": retained.length},
        "pad_hit": {"track": hit.track, "clip": hit.clip,
                    "start": hit.start, "length": hit.length},
        "app_exit": 0,
        "app_exit_source": "process::App::finish requires a successful exit status for both \
                            no-audio GUI runs; convert returns only after both quit cleanly",
        "listening": "none: no audio device was opened",
    });
    fs::write(&evidence, serde_json::to_vec_pretty(&payload)?)?;
    println!("Actual-app live-take evidence: {}", evidence.display());
    Ok(())
}
