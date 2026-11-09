//! Public bundle/CLI boundaries. All default tests execute only the strict fake.
use brownnote_native::{
    adapters::soundcraft::{COMMIT, Report, Request, SamplerMapping, convert},
    edit::EditModel,
    export::{
        adapter::{self, ProjectFrozenHit},
        bundle::{BundleRequest, SystemWriter},
        manifest::{
            AssetEntry, AssetId, AssetKind, AudioRole, EventEntry, EventId, LossRecord, Manifest,
            PartEntry, PartId, PartKind, TimeOrigin, Timing,
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
    io::Cursor,
    path::PathBuf,
    process::Command,
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
            .any(|r| r.detail.contains("MIDI onset/duration rounded"))
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
