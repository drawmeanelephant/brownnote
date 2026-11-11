//! Public bundle/CLI boundaries. All default tests execute only the strict fake.
use brownnote_native::{
    adapters::soundcraft::{COMMIT, PIN, Report, Request, SamplerMapping, VERSION, convert},
    edit::EditModel,
    export::{
        adapter::{self, ProjectFrozenHit},
        bundle::{BundleRequest, SystemWriter},
        manifest::{
            AssetEntry, AssetId, AssetKind, AudioRole, EventEntry, EventId, LossRecord,
            MANIFEST_FORMAT_VERSION, Manifest, PartEntry, PartId, PartKind, PluginIdentity,
            TimeOrigin, Timing,
        },
        midi,
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
    collections::{BTreeMap, BTreeSet, VecDeque},
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
    /// The #234 note as the earliest one: 100 us from 500 ms, whose end
    /// rounds onto its onset tick at 120 BPM.
    fn sub_tick(&mut self) {
        self.manifest.events[2].start_nanos = 500_000_000;
        self.manifest.events[2].duration_nanos = 100_000;
    }
    /// A copy of event 2 as event 4 on a live-instrument part.
    fn live_note(&mut self) {
        self.manifest.parts.push(PartEntry {
            id: PartId(3),
            kind: PartKind::LiveInstrumentNotes {
                plugin: PluginIdentity {
                    backend: "clap".into(),
                    id: "org.example.synth".into(),
                    artifact: "ab".repeat(32),
                    abi: "clap-1.2".into(),
                    version: "0.4.0".into(),
                },
            },
        });
        let mut note = self.manifest.events[2].clone();
        note.id.sequence = 4;
        note.part = PartId(3);
        self.manifest.events.push(note);
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
fn reopen_inspection_rejects_changed_clips_notes_media_mutes_or_master() -> TestResult {
    for (corrupt, expected) in [
        ("position", "saved audio clip count/position/length differs"),
        ("notes", "saved MIDI notes differ"),
        ("media", "not relocatable Audio Files/ media"),
        ("mute", "saved MIDI clip differs"),
        ("master", "saved master fader differs"),
    ] {
        let fixture = Fixture::new()?;
        let mut request = fixture.request.clone();
        if corrupt == "master" {
            request.bundle = writer_bundle(&fixture.root, Document::default().master)?;
        }
        fixture.config(json!({"corrupt":corrupt}))?;
        let error = convert(&request).err().ok_or("accepted a changed reopen")?;
        assert!(error.to_string().contains(expected), "{corrupt}: {error}");
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
fn writer_bundle(root: &std::path::Path, master: f32) -> Result<PathBuf, Box<dyn Error>> {
    let decay: Vec<f32> = (0..12_000)
        .map(|i| 0.6 * (1.0 - i as f32 / 12_000.0))
        .collect();
    writer_bundle_with_source(root, float_wav(1, &decay)?, master)
}

/// [`writer_bundle`] with `source` as pad 3's original, unprocessed sample.
fn writer_bundle_with_source(
    root: &std::path::Path,
    source: Vec<u8>,
    master: f32,
) -> Result<PathBuf, Box<dyn Error>> {
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

/// A bundle written by the real export path for a project whose take was
/// played through a live CLAP instrument: the retained take audio plus that
/// take's notes, pinned to the plugin identity. Each note is (channel, pitch,
/// velocity, on, off) at the take's 60 BPM recording tempo.
fn live_take_bundle(
    root: &std::path::Path,
    notes: &[(u8, u8, u8, u64, u64)],
) -> Result<(PathBuf, Plugin), Box<dyn Error>> {
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
    for &(channel, pitch, velocity, on, off) in notes {
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

/// A manifest v4 bundle from the real export path for one live-instrument pass:
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

/// SoundCraft v0.19.0's fader law (`soundcraft_dsp::db_to_gain`): f32
/// 10^(dB/20), with -144 dB or below as silence.
fn soundcraft_gain(volume_db: f64) -> f32 {
    let db = volume_db as f32;
    if db.is_nan() || db <= -144.0 {
        0.0
    } else {
        10f32.powf(db.min(200.0) / 20.0)
    }
}

fn stereo_float(bytes: &[u8]) -> Result<Vec<[f32; 2]>, Box<dyn Error>> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))?;
    let spec = reader.spec();
    if (
        spec.channels,
        spec.sample_rate,
        spec.bits_per_sample,
        spec.sample_format,
    ) != (2, 48_000, 32, hound::SampleFormat::Float)
    {
        return Err(format!("expected 48 kHz float32 stereo, got {spec:?}").into());
    }
    let samples = reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?;
    let (frames, remainder) = samples.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("stereo WAV with a partial frame".into());
    }
    Ok(frames.to_vec())
}

fn reference_mix(
    bundle: &std::path::Path,
    manifest: &Manifest,
) -> Result<Vec<[f32; 2]>, Box<dyn Error>> {
    let reference = manifest
        .audio
        .iter()
        .find(|entry| entry.role == AudioRole::Reference)
        .ok_or("bundle without a reference mix")?;
    stereo_float(&fs::read(bundle.join(reference.asset.relative_path()))?)
}

/// What an unautomated SoundCraft session plays on its main output: every
/// imported clip, read from the session's own `Audio Files/`, summed at its
/// start on a 0 dB centred track, then the master fader. Muted MIDI is silent.
fn render_session(
    output: &std::path::Path,
    manifest: &Manifest,
    report: &Report,
    frames: usize,
) -> Result<Vec<[f32; 2]>, Box<dyn Error>> {
    let mut mix = vec![[0.0f32; 2]; frames];
    for clip in &report.audio {
        let asset = manifest
            .event(clip.event)
            .and_then(|event| event.asset)
            .ok_or("clip without event media")?;
        let audio = stereo_float(&fs::read(
            output.join("Audio Files").join(format!("{asset}.wav")),
        )?)?;
        let start = usize::try_from(clip.start)?;
        let span = mix
            .get_mut(start..start + audio.len())
            .ok_or("clip outlasts the reference")?;
        for (out, frame) in span.iter_mut().zip(&audio) {
            out[0] += frame[0];
            out[1] += frame[1];
        }
    }
    let gain = report
        .master
        .as_ref()
        .map_or(1.0, |master| soundcraft_gain(master.volume_db));
    for value in mix.iter_mut().flatten() {
        *value *= gain;
    }
    Ok(mix)
}

/// Largest per-sample difference; NaN is kept so it can never pass.
fn max_error(actual: &[[f32; 2]], expected: &[[f32; 2]]) -> f32 {
    actual
        .iter()
        .flatten()
        .zip(expected.iter().flatten())
        .map(|(a, e)| (a - e).abs())
        .fold(0.0, |worst, error| {
            if error.is_nan() || error > worst {
                error
            } else {
                worst
            }
        })
}

fn peak(frames: &[[f32; 2]]) -> f32 {
    frames
        .iter()
        .flatten()
        .fold(0.0, |peak, value| peak.max(value.abs()))
}

#[test]
fn real_writer_bundle_imports_every_effective_hit_and_note() -> TestResult {
    let fixture = Fixture::new()?;
    let bundle = writer_bundle(&fixture.root, Document::default().master)?;
    let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
    let mut request = fixture.request.clone();
    request.bundle = bundle.clone();
    let report = convert(&request)?;
    assert_writer_bundle_report(&report);
    // The default 80% master travels as data and lands on one fader.
    assert_eq!(manifest.master_gain, Some(Document::default().master));
    assert_eq!(report.master.as_ref().map(|m| m.gain), manifest.master_gain);
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

/// #231: mixed the way the target mixes it, the converted session reproduces
/// the writer's reference. Hits stay pre-master on separate tracks and exactly
/// one master fader applies the master once, never twice or not at all.
#[test]
fn converted_writer_mix_matches_the_reference_at_master_0_quarter_default_and_unity() -> TestResult
{
    for master in [0.0, 0.25, Document::default().master, 1.0] {
        let fixture = Fixture::new()?;
        let bundle = writer_bundle(&fixture.root, master)?;
        let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
        let mut request = fixture.request.clone();
        request.bundle = bundle.clone();
        let report = convert(&request)?;
        assert_writer_bundle_report(&report);
        assert_eq!(report.master.as_ref().map(|m| m.gain), Some(master));
        // Every clip is the writer's pre-master hit render, byte for byte, with
        // its own gain, velocity and pan already baked in.
        for clip in &report.audio {
            let hit = manifest
                .audio
                .iter()
                .find(|a| a.role == AudioRole::HitWet && a.event == Some(clip.event))
                .ok_or("clip without a HitWet render")?;
            let media = request
                .output
                .join("Audio Files")
                .join(format!("{}.wav", hit.asset));
            assert_eq!(AssetId::of(&fs::read(media)?), hit.asset);
        }
        let expected = reference_mix(&bundle, &manifest)?;
        let actual = render_session(&request.output, &manifest, &report, expected.len())?;
        // The overlapping hits peak near 0.63 before the master and never
        // clip, so the target's unclamped mix must equal the reference.
        let reference_peak = peak(&expected);
        assert!(
            (0.6 * master..=0.65 * master).contains(&reference_peak),
            "master {master}: reference peak {reference_peak}"
        );
        let error = max_error(&actual, &expected);
        assert!(error <= 1e-6, "master {master}: max sample error {error}");
    }
    Ok(())
}

/// A bundle from before v4 states no master gain. The adapter adds no master
/// fader and says so; it never recovers the gain from the writer's prose.
#[test]
fn pre_v4_bundle_without_master_gain_is_reported_and_gets_no_master_fader() -> TestResult {
    let fixture = Fixture::new()?;
    let bundle = writer_bundle(&fixture.root, 0.25)?;
    let path = bundle.join("manifest.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&path)?)?;
    legacy["version"] = json!(3);
    legacy
        .as_object_mut()
        .ok_or("manifest is not an object")?
        .remove("master_gain")
        .ok_or("writer bundle without master_gain")?;
    fs::write(&path, serde_json::to_vec(&legacy)?)?;
    let mut request = fixture.request.clone();
    request.bundle = bundle;
    let report = convert(&request)?;
    assert_writer_bundle_report(&report);
    assert!(report.bundle_losses.iter().any(|loss| matches!(
        loss,
        LossRecord::BakedProcessing { detail, .. } if detail.contains("reference master 0.25")
    )));
    assert!(report.master.is_none());
    assert!(
        !fixture
            .calls()?
            .iter()
            .any(|call| call["params"]["kind"] == "master")
    );
    assert!(
        report
            .records
            .iter()
            .any(|record| record.kind == "unsupported"
                && record.detail.contains("states no master gain"))
    );
    Ok(())
}

#[test]
fn unavailable_live_plugin_is_reported_and_the_retained_take_still_converts() -> TestResult {
    let fixture = Fixture::new()?;
    let ms = 1_000_000;
    let (bundle, plugin) = live_take_bundle(
        &fixture.root,
        &[
            (0, 60, 100, 200 * ms, 300 * ms),
            (0, 64, 90, 500 * ms, 800 * ms),
        ],
    )?;
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
    let bundle = writer_bundle(&fixture.root, Document::default().master)?;
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
/// The 100 ms, 440 Hz mono PCM16 tone from #232.
fn pcm16_tone(rate: u32) -> Result<Vec<u8>, Box<dyn Error>> {
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
        for i in 0..rate / 10 {
            let phase = f64::from(i) * 440.0 * std::f64::consts::TAU / f64::from(rate);
            wav.write_sample((12_000.0 * phase.sin()) as i16)?;
        }
        wav.finalize()?;
    }
    Ok(cursor.into_inner())
}

#[test]
fn retained_source_at_any_native_rate_converts_from_48k_hit_wet() -> TestResult {
    for rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
        let fixture = Fixture::new()?;
        let source = pcm16_tone(rate)?;
        let source_id = AssetId::of(&source);
        let bundle =
            writer_bundle_with_source(&fixture.root, source.clone(), Document::default().master)?;
        let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
        let hits: Vec<_> = manifest
            .events
            .iter()
            .filter(|event| event.source == Some(source_id))
            .collect();
        assert_eq!(hits.len(), 2, "{rate} Hz");
        let mut wet = Vec::new();
        for hit in &hits {
            let entry = manifest
                .audio
                .iter()
                .find(|a| a.role == AudioRole::HitWet && a.event == Some(hit.id))
                .ok_or("hit without writer entry")?;
            assert_eq!(hit.asset, Some(entry.asset), "{rate} Hz");
            let spec = hound::WavReader::new(Cursor::new(fs::read(
                bundle.join(entry.asset.relative_path()),
            )?))?
            .spec();
            assert_eq!(
                (spec.sample_rate, spec.sample_format, spec.bits_per_sample),
                (48_000, hound::SampleFormat::Float, 32),
                "{rate} Hz"
            );
            wet.push(entry);
        }
        let mut request = fixture.request.clone();
        request.bundle = bundle.clone();
        let report = convert(&request).map_err(|error| format!("{rate} Hz source: {error}"))?;
        for entry in wet {
            let clip = report
                .audio
                .iter()
                .find(|clip| Some(clip.event) == entry.event)
                .ok_or("pad hit was not imported")?;
            assert_eq!(
                (clip.start, clip.length),
                (entry.start_frame as i64, entry.frames as i64),
                "{rate} Hz"
            );
            assert!(
                request
                    .output
                    .join("Audio Files")
                    .join(format!("{}.wav", entry.asset))
                    .is_file()
            );
        }
        // Only the 48 kHz renders reach the target (the fake also refuses any
        // other import rate); the source stays byte-identical provenance.
        let source_name = format!("{source_id}.wav");
        assert!(fixture.calls()?.iter().all(|call| {
            call["method"] != "file.import_audio"
                || !call["params"]["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(&source_name))
        }));
        assert_eq!(
            fs::read(request.output.join(source_id.relative_path()))?,
            source
        );
        assert!(
            !request
                .output
                .join("Audio Files")
                .join(&source_name)
                .exists()
        );
    }
    Ok(())
}

/// (onset, gate, pitch, channel, velocity), with times in 960 PPQ ticks.
type SmfNote = (i64, i64, u8, u8, u8);

/// Deliberately independent of export::midi: every note of an SMF, sorted.
/// Note-offs pair FIFO per track, channel and pitch; running status is refused.
fn smf_notes(bytes: &[u8]) -> Result<Vec<SmfNote>, Box<dyn Error>> {
    fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8], Box<dyn Error>> {
        let head = bytes.get(..count).ok_or("truncated SMF")?;
        *bytes = &bytes[count..];
        Ok(head)
    }
    fn quantity(bytes: &mut &[u8]) -> Result<i64, Box<dyn Error>> {
        let mut value = 0;
        for _ in 0..4 {
            let byte = take(bytes, 1)?[0];
            value = (value << 7) | i64::from(byte & 0x7f);
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err("SMF quantity longer than four bytes".into())
    }
    let mut rest = bytes;
    let header = take(&mut rest, 14)?;
    if header[..4] != *b"MThd" || header[12..] != 960_u16.to_be_bytes() {
        return Err("not a 960 PPQ SMF".into());
    }
    let mut notes = Vec::new();
    while !rest.is_empty() {
        let chunk = take(&mut rest, 8)?;
        if chunk[..4] != *b"MTrk" {
            return Err("not an SMF track".into());
        }
        let mut body = take(
            &mut rest,
            u32::from_be_bytes(chunk[4..].try_into()?) as usize,
        )?;
        let mut tick = 0;
        let mut held: BTreeMap<(u8, u8), VecDeque<(i64, u8)>> = BTreeMap::new();
        while !body.is_empty() {
            tick += quantity(&mut body)?;
            let status = take(&mut body, 1)?[0];
            if status == 0xff {
                take(&mut body, 1)?;
                let length = quantity(&mut body)?;
                take(&mut body, usize::try_from(length)?)?;
                continue;
            }
            let data = take(&mut body, 2)?;
            let key = (status & 0x0f, data[0]);
            match status & 0xf0 {
                0x90 if data[1] > 0 => held.entry(key).or_default().push_back((tick, data[1])),
                0x80 | 0x90 => {
                    let (on, velocity) = held
                        .get_mut(&key)
                        .and_then(VecDeque::pop_front)
                        .ok_or("note-off without a note-on")?;
                    notes.push((on, tick - on, key.1, key.0, velocity));
                }
                _ => return Err(format!("unexpected SMF status {status:#04x}").into()),
            }
        }
    }
    notes.sort();
    Ok(notes)
}

/// #234 through public APIs: a saved 48 kHz pad source, two pad hits and a
/// 100 us instrument note at 120 BPM, exported by the real writer.
fn sub_tick_writer_bundle(root: &std::path::Path) -> Result<PathBuf, Box<dyn Error>> {
    let source = float_wav(1, &[0.25; 4_800])?;
    let id = project::AssetId::of(&source);
    let mut document = Document {
        assets: vec![Asset {
            reference: AssetRef::of(&source),
            kind: MediaKind::Source,
        }],
        ..Document::default()
    };
    document.pads[3].sample = Some(SampleAssignment {
        source: id,
        retained: None,
    });
    let project_dir = root.join("sub-tick project");
    let path = root.join("sub-tick source.wav");
    fs::write(&path, &source)?;
    Store::new(project_dir.clone())
        .save(
            &document,
            &[MediaSource {
                id,
                path: path.clone(),
            }],
        )
        .map_err(debug)?;
    fs::remove_file(&path)?;
    let mut takes = Takes::new(LoopBars::try_from(1).map_err(debug)?, TakeId(234));
    takes
        .set_tempo(Tempo::try_from(120_000).map_err(debug)?)
        .map_err(debug)?;
    let take = takes.arm(PassOptions::default()).map_err(debug)?;
    takes.start(take).map_err(debug)?;
    let pad = Target::Pad(PadId::try_from(3).map_err(debug)?);
    let ms = 1_000_000;
    for (target, pitch, velocity, on, off) in [
        (pad, 36, 110, 100 * ms, 130 * ms),
        (pad, 36, 90, 250 * ms, 280 * ms),
        (
            Target::Instrument(InstrumentId(77)),
            60,
            100,
            500 * ms,
            500 * ms + 100_000,
        ),
    ] {
        let key = NoteKey {
            source: SourceId(5),
            channel: Channel::try_from(0).map_err(debug)?,
            pitch: Pitch::try_from(pitch).map_err(debug)?,
        };
        let velocity = Velocity::try_from(velocity).map_err(debug)?;
        takes
            .note_on(take, target, key, velocity, OriginalTime::from_nanos(on))
            .map_err(debug)?;
        takes
            .note_off(take, key, OriginalTime::from_nanos(off))
            .map_err(debug)?;
    }
    takes.finish(take).map_err(debug)?;
    let bundle = root.join("sub-tick bundle");
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
    Ok(bundle)
}

/// #234: the writer gives a 100 us note a legal one-tick SMF gate. The adapter
/// converts it to that same gate and still imports the unrelated pad hits.
#[test]
fn sub_tick_writer_note_converts_to_the_bundle_one_tick_gate_with_its_pad_audio() -> TestResult {
    let fixture = Fixture::new()?;
    let bundle = sub_tick_writer_bundle(&fixture.root)?;
    let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
    let part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::InstrumentNotes { .. }))
        .ok_or("bundle without the instrument part")?;
    let note = manifest
        .events
        .iter()
        .find(|event| event.part == part.id)
        .ok_or("bundle without the instrument note")?;
    assert_eq!(
        (note.start_nanos, note.duration_nanos),
        (500_000_000, 100_000)
    );
    let gate = vec![(960, 1, 60, 0, 100)];
    assert_eq!(smf_notes(&fs::read(bundle.join("notes.mid"))?)?, gate);
    let mut request = fixture.request.clone();
    request.bundle = bundle;
    let report = convert(&request)?;
    let notes: Vec<_> = report
        .midi
        .iter()
        .flat_map(|part| &part.notes)
        .map(|n| (n.start, n.length, n.pitch, n.channel, n.velocity))
        .collect();
    assert_eq!(notes, gate);
    let record = report
        .records
        .iter()
        .find(|record| record.detail.contains("one-tick minimum gate"))
        .ok_or("the lengthened gate was not reported")?;
    assert_eq!(
        (record.event, record.part, record.kind.as_str()),
        (Some(note.id), Some(part.id), "conversion")
    );
    assert!(record.detail.contains("100000 ns"), "{}", record.detail);
    let hits: BTreeSet<_> = manifest
        .events
        .iter()
        .filter(|event| event.pad == Some(3))
        .map(|event| event.id)
        .collect();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        report
            .audio
            .iter()
            .map(|clip| clip.event)
            .collect::<BTreeSet<_>>(),
        hits
    );
    assert!(request.output.join("session.scraft").is_file());
    Ok(())
}

/// Independent expectations at 120 BPM and 960 PPQ: one tick is 1e9 / 1,920
/// ns, so an exact half tick needs an odd multiple of 781,250 ns.
#[test]
fn note_ticks_round_onsets_and_absolute_ends_with_a_one_tick_minimum_gate() -> TestResult {
    const EXACT: &[&str] = &[];
    const ROUNDED: &[&str] = &["MIDI onset/end rounded"];
    const MINIMUM: &[&str] = &["MIDI onset/end rounded", "one-tick minimum gate"];
    // (start ns, duration ns, pitch, channel, velocity), the expected (start,
    // length) ticks, and the conversion records expected for the note.
    let cases = [
        // 960 to 960.192: the #234 gate rounds to zero and gets one tick.
        ((500_000_000, 100_000, 60, 0, 100), (960, 1), MINIMUM),
        // An end 0.49999872 tick after an exact onset rounds onto it.
        ((1_000_000_000, 260_416, 61, 1, 1), (1_920, 1), MINIMUM),
        // 0.50000064 tick rounds up: one tick without lengthening.
        ((2_000_000_000, 260_417, 62, 2, 127), (3_840, 1), ROUNDED),
        // An exact tie at 5,761.5 rounds later.
        ((3_000_000_000, 781_250, 63, 9, 64), (5_760, 2), ROUNDED),
        // 7,680.4 and 7,681.6 give two ticks; 1.2 ticks alone would be one.
        ((4_000_208_333, 625_000, 64, 15, 90), (7_680, 2), ROUNDED),
        // 9,600.6 and 9,602.4 give one tick; 1.8 ticks alone would be two.
        ((5_000_312_500, 937_500, 65, 3, 80), (9_601, 1), ROUNDED),
        // 0.3 tick alone rounds to zero, but its end crosses a half tick.
        ((6_000_208_333, 156_250, 66, 4, 70), (11_520, 1), ROUNDED),
        // Exact onset and end: nothing to report.
        ((7_000_000_000, 1_562_500, 67, 5, 60), (13_440, 3), EXACT),
        // An onset tie at 15,361.5 rounds later, and its end rounds onto it.
        ((8_000_781_250, 100_000, 68, 6, 50), (15_362, 1), MINIMUM),
    ];
    let mut fixture = Fixture::new()?;
    fixture.manifest.events.truncate(2);
    for (sequence, ((start, duration, pitch, channel, velocity), _, _)) in (10..).zip(cases) {
        fixture.manifest.events.push(EventEntry {
            id: EventId { take: 5, sequence },
            part: PartId(2),
            start_nanos: start,
            duration_nanos: duration,
            pad: None,
            pitch: Some(pitch),
            channel: Some(channel),
            velocity: Some(velocity),
            asset: None,
            source: None,
            variant_of: None,
        });
    }
    fixture.save()?;
    let report = convert(&fixture.request)?;
    assert_eq!(report.audio.len(), 2);
    let mut expected: Vec<_> = cases
        .iter()
        .map(|&((_, _, pitch, channel, velocity), (start, length), _)| {
            (start, length, pitch, channel, velocity)
        })
        .collect();
    expected.sort();
    let notes: Vec<_> = report.midi[0]
        .notes
        .iter()
        .map(|n| (n.start, n.length, n.pitch, n.channel, n.velocity))
        .collect();
    assert_eq!(notes, expected);
    // The bundle's own SMF writer agrees on every boundary.
    assert_eq!(
        smf_notes(&midi::encode(&fixture.manifest)?.bytes)?,
        expected
    );
    for (sequence, (_, _, records)) in (10..).zip(cases) {
        let id = EventId { take: 5, sequence };
        let details: Vec<_> = report
            .records
            .iter()
            .filter(|record| record.event == Some(id) && record.kind == "conversion")
            .map(|record| record.detail.as_str())
            .collect();
        assert_eq!(details.len(), records.len(), "{sequence}: {details:?}");
        for (detail, record) in details.iter().zip(records) {
            assert!(detail.contains(record), "{sequence}: {detail}");
        }
    }
    Ok(())
}

/// Rewrite every reference to `old` so the bundle stays hash-consistent.
fn replace_asset(
    bundle: &std::path::Path,
    manifest: &mut Manifest,
    old: AssetId,
    bytes: &[u8],
) -> Result<AssetId, Box<dyn Error>> {
    let new = AssetId::of(bytes);
    fs::write(bundle.join(new.relative_path()), bytes)?;
    let entry = manifest
        .assets
        .iter_mut()
        .find(|asset| asset.id == old)
        .ok_or("asset not in manifest")?;
    (entry.id, entry.bytes, entry.path) = (new, bytes.len() as u64, new.relative_path());
    for event in &mut manifest.events {
        for slot in [&mut event.asset, &mut event.source] {
            if *slot == Some(old) {
                *slot = Some(new);
            }
        }
    }
    for entry in &mut manifest.audio {
        if entry.asset == old {
            entry.asset = new;
        }
    }
    for loss in &mut manifest.losses {
        if let LossRecord::BakedProcessing { asset, .. } = loss
            && *asset == old
        {
            *asset = new;
        }
    }
    Ok(new)
}

#[test]
fn imported_media_outside_the_slice_or_any_truncated_wav_fails_before_launch() -> TestResult {
    for bad in [
        "source_as_hit",
        "mapped_source",
        "truncated_hit",
        "truncated_source",
    ] {
        let fixture = Fixture::new()?;
        let source = pcm16_tone(96_000)?;
        let source_id = AssetId::of(&source);
        let bundle = writer_bundle_with_source(&fixture.root, source, Document::default().master)?;
        let path = bundle.join("manifest.json");
        let mut manifest = Manifest::decode(&fs::read(&path)?)?;
        let hit = manifest
            .events
            .iter()
            .find(|event| event.source == Some(source_id))
            .and_then(|event| event.asset)
            .ok_or("no pad hit")?;
        let instrument = manifest
            .parts
            .iter()
            .find(|part| matches!(part.kind, PartKind::InstrumentNotes { .. }))
            .ok_or("no instrument part")?
            .id;
        let mut request = fixture.request.clone();
        request.bundle = bundle.clone();
        let truncate = |id: AssetId| -> Result<Vec<u8>, Box<dyn Error>> {
            let bytes = fs::read(bundle.join(id.relative_path()))?;
            Ok(bytes[..bytes.len() - 4].to_vec())
        };
        let expected = match bad {
            // One asset both retained as source and imported keeps the gate.
            "source_as_hit" => {
                for event in &mut manifest.events {
                    if event.asset == Some(hit) {
                        event.asset = Some(source_id);
                    }
                }
                for entry in &mut manifest.audio {
                    if entry.asset == hit {
                        entry.asset = source_id;
                    }
                }
                "outside proven"
            }
            "mapped_source" => {
                request.mappings.push(SamplerMapping {
                    part: instrument,
                    asset: source_id,
                    ..fixture.mapping()
                });
                "outside proven"
            }
            "truncated_hit" => {
                replace_asset(&bundle, &mut manifest, hit, &truncate(hit)?)?;
                "not a complete"
            }
            "truncated_source" => {
                replace_asset(&bundle, &mut manifest, source_id, &truncate(source_id)?)?;
                "not a complete"
            }
            _ => unreachable!(),
        };
        fs::write(&path, manifest.encode()?)?;
        let error = convert(&request).err().ok_or(format!("accepted {bad}"))?;
        assert!(error.to_string().contains(expected), "{bad}: {error}");
        assert!(!fixture.root.join("calls.jsonl").exists(), "{bad}");
        fixture.no_output();
    }
    Ok(())
}

/// Live-instrument notes never become target ticks, so a gate under half a
/// tick cannot block the take's authoritative retained audio.
#[test]
fn sub_tick_live_instrument_note_never_blocks_the_retained_take() -> TestResult {
    let fixture = Fixture::new()?;
    let ms = 1_000_000;
    let (bundle, plugin) = live_take_bundle(
        &fixture.root,
        &[
            (0, 60, 100, 200 * ms, 200 * ms + 100_000),
            (0, 64, 90, 500 * ms, 800 * ms),
        ],
    )?;
    let manifest = Manifest::decode(&fs::read(bundle.join("manifest.json"))?)?;
    let part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::LiveInstrumentNotes { .. }))
        .ok_or("bundle without the live take's notes")?;
    let gates: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| event.part == part.id)
        .map(|event| event.duration_nanos)
        .collect();
    // Under half of a 960 PPQ tick at the bundle's 120 BPM.
    assert!(
        gates
            .iter()
            .any(|&gate| gate > 0 && gate * 1_920 < 500_000_000),
        "{gates:?}"
    );
    let mut request = fixture.request.clone();
    request.bundle = bundle;
    let report = convert(&request)?;
    assert!(report.midi.is_empty());
    let audio: Vec<_> = report.audio.iter().map(|c| (c.start, c.length)).collect();
    assert_eq!(audio, [(0, 2_400)]);
    let records: Vec<_> = report
        .records
        .iter()
        .filter(|record| record.part == Some(part.id))
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].kind, "unsupported");
    assert!(records[0].detail.contains("2 live instrument note(s)"));
    assert!(records[0].detail.contains(&plugin.id));
    assert!(request.output.join("session.scraft").is_file());
    Ok(())
}

#[test]
fn malformed_sub_tick_and_live_notes_still_fail_before_executing_anything() -> TestResult {
    // Without a defect the same bundle converts, so each failure is the note's.
    let mut fixture = Fixture::new()?;
    fixture.sub_tick();
    fixture.live_note();
    fixture.save()?;
    convert(&fixture.request)?;
    let bounds = "MIDI note needs pitch";
    for (index, bad, expected) in [
        (2, "zero", "zero event duration"),
        (4, "zero", "zero event duration"),
        (2, "end", "event end overflows"),
        (4, "end", "event end overflows"),
        (2, "velocity", bounds),
        (4, "velocity", bounds),
        (2, "channel", bounds),
        (4, "pitch", bounds),
        (4, "missing", bounds),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.sub_tick();
        fixture.live_note();
        let event = &mut fixture.manifest.events[index];
        match bad {
            "zero" => event.duration_nanos = 0,
            "end" => event.start_nanos = u64::MAX - 50_000,
            "velocity" => event.velocity = Some(0),
            "channel" => event.channel = Some(16),
            "pitch" => event.pitch = Some(128),
            "missing" => event.channel = None,
            _ => unreachable!(),
        }
        fixture.save()?;
        let error = convert(&fixture.request)
            .err()
            .ok_or("accepted a malformed note")?;
        assert!(
            error.to_string().contains(expected),
            "{index} {bad}: {error}"
        );
        assert!(!fixture.root.join("calls.jsonl").exists(), "{index} {bad}");
        fixture.no_output();
    }
    Ok(())
}

#[test]
fn failures_after_a_one_tick_gate_reaches_the_target_never_publish() -> TestResult {
    for (config, expected) in [
        (json!({"fail": "midi.note_add"}), "injected partial failure"),
        // The fake shortens the earliest saved note on reopen: 1 -> 0 ticks.
        (json!({"corrupt": "notes"}), "saved MIDI notes differ"),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.sub_tick();
        fixture.save()?;
        fixture.config(config)?;
        let error = convert(&fixture.request)
            .err()
            .ok_or("published after an injected failure")?;
        assert!(error.to_string().contains(expected), "{error}");
        assert!(fixture.calls()?.iter().any(|call| {
            call["method"] == "midi.note_add"
                && call["params"]["start_ticks"] == 960
                && call["params"]["length_ticks"] == 1
        }));
        fixture.no_output();
    }
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
    let bundle = writer_bundle(&fixture.root, Document::default().master)?;
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

/// #232 against the actual pinned app: writer bundles whose pad source sits
/// outside the import slice convert, then survive GUI edits, save, reopen and
/// source-free relocation with relative `Audio Files/` media.
#[test]
#[ignore = "requires explicit BROWNNOTE_SOUNDCRAFT_APP/CLI/PYTHON; launches the actual pinned GUI"]
fn pinned_app_converts_writer_bundles_with_off_slice_sources() -> TestResult {
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
    for rate in [8_000, 96_000, 192_000] {
        let mut fixture = Fixture::new()?;
        let source = pcm16_tone(rate)?;
        fixture.request.bundle =
            writer_bundle_with_source(&fixture.root, source.clone(), Document::default().master)?;
        fixture.request.app = app.clone().into();
        fixture.request.cli = cli.clone().into();
        fixture.request.timeout = Duration::from_secs(45);
        let report = convert(&fixture.request).map_err(|error| format!("{rate} Hz: {error}"))?;
        assert_eq!(report.audio.len(), 4, "{rate} Hz");
        assert_eq!(
            fs::read(
                fixture
                    .request
                    .output
                    .join(AssetId::of(&source).relative_path())
            )?,
            source
        );
        let status = Command::new(&python)
            .arg(root.join("tests/fixtures/soundcraft_adapter/edit_output.py"))
            .arg(&fixture.request.app)
            .arg(&fixture.request.cli)
            .arg(&fixture.request.output)
            .arg(&fixture.request.bundle)
            .arg(fixture.root.join("adapter-edit.json"))
            .status()?;
        assert!(
            status.success(),
            "{rate} Hz: actual-app edit/save/reopen failed"
        );
    }
    Ok(())
}

/// #171's open decision, answered by the actual pinned app instead of the fake:
/// a current v4 bundle built by the live-take export path converts without
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

/// #231 through the actual pinned app: each converted writer bundle, reopened
/// and bounced by the real GUI, matches the writer's reference mix at master
/// 0, 0.25, the default 0.8 and unity.
#[test]
#[ignore = "requires explicit BROWNNOTE_SOUNDCRAFT_APP/CLI/PYTHON; launches the actual pinned GUI"]
fn pinned_app_bounce_matches_the_writer_reference_at_each_master_gain() -> TestResult {
    if std::env::var_os("BROWNNOTE_SOUNDCRAFT_RUN_PINNED").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err("set BROWNNOTE_SOUNDCRAFT_RUN_PINNED=1 explicitly".into());
    }
    let app = std::env::var_os("BROWNNOTE_SOUNDCRAFT_APP").ok_or("set BROWNNOTE_SOUNDCRAFT_APP")?;
    let cli = std::env::var_os("BROWNNOTE_SOUNDCRAFT_CLI").ok_or("set BROWNNOTE_SOUNDCRAFT_CLI")?;
    let python =
        std::env::var_os("BROWNNOTE_SOUNDCRAFT_PYTHON").ok_or("set BROWNNOTE_SOUNDCRAFT_PYTHON")?;
    let pin: Value = serde_json::from_str(PIN)?;
    let app_hash = Hash256::of(&fs::read(&app)?).to_string();
    let cli_hash = Hash256::of(&fs::read(&cli)?).to_string();
    assert_eq!(pin["binary_sha256"]["SoundCraft"], app_hash);
    assert_eq!(pin["binary_sha256"]["soundcraft-cli"], cli_hash);
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/soundcraft_adapter/bounce_output.py");

    let fixture = Fixture::new()?;
    let mut results = Vec::new();
    for master in [0.0, 0.25, Document::default().master, 1.0] {
        let dir = fixture.root.join(format!("master {master}"));
        fs::create_dir(&dir)?;
        let mut request = fixture.request.clone();
        request.app = app.clone().into();
        request.cli = cli.clone().into();
        request.timeout = Duration::from_secs(45);
        request.bundle = writer_bundle(&dir, master)?;
        request.output = dir.join("output");
        let manifest = Manifest::decode(&fs::read(request.bundle.join("manifest.json"))?)?;
        let report = convert(&request)?;
        assert_writer_bundle_report(&report);
        let fader = report.master.as_ref().ok_or("no master fader")?;
        let record = report
            .records
            .iter()
            .find(|record| record.detail.starts_with("Project master gain"))
            .ok_or("master fader was not reported")?;
        let expected = reference_mix(&request.bundle, &manifest)?;
        let bounce = dir.join("bounce.wav");
        let status = Command::new(&python)
            .arg(&script)
            .arg(&app)
            .arg(&cli)
            .arg(request.output.join("session.scraft"))
            .arg(expected.len().to_string())
            .arg(&bounce)
            .arg(dir.join("bounce-probe.json"))
            .status()?;
        assert!(
            status.success(),
            "actual-app bounce at master {master} failed"
        );
        let actual = stereo_float(&fs::read(&bounce)?)?;
        results.push(json!({
            "master_gain": master,
            "fader": {"gain": fader.gain, "volume_db": fader.volume_db},
            "record": record.detail,
            "reference_frames": expected.len(),
            "bounce_frames": actual.len(),
            "reference_peak": peak(&expected),
            "bounce_peak": peak(&actual),
            "max_abs_error": max_error(&actual, &expected),
        }));
    }

    let evidence = std::env::var_os("BROWNNOTE_SOUNDCRAFT_MASTER_EVIDENCE")
        .map_or_else(|| fixture.root.join("master-bounce.json"), PathBuf::from);
    let payload = json!({
        "check": "#231 static master gain: actual-app bounces of converted writer bundles against the writer's reference mix",
        "pin": {"tag": pin["tag"], "commit": COMMIT, "version": VERSION},
        "binaries": {
            "SoundCraft": {"sha256": app_hash, "matches_pin": true},
            "soundcraft-cli": {"sha256": cli_hash, "matches_pin": true},
        },
        "launch": ["SoundCraft", "--no-audio", "--control", "0"],
        "bounce": {"method": "file.bounce_mix", "source": "main", "start": 0,
                   "end": "reference frames", "bit_depth": "32f", "dither": false,
                   "normalize": false},
        "tolerance": 1e-6,
        "results": results,
        "app_exit": 0,
        "app_exit_source": "convert requires a clean exit from both adapter GUI runs; \
                            bounce_output.py asserts a clean exit from the bounce run",
        "listening": "none: no audio device was opened",
    });
    fs::write(&evidence, serde_json::to_vec_pretty(&payload)?)?;
    println!("Actual-app master bounce evidence: {}", evidence.display());
    for result in &results {
        assert_eq!(
            result["bounce_frames"], result["reference_frames"],
            "{result}"
        );
        assert!(
            result["max_abs_error"]
                .as_f64()
                .is_some_and(|error| error <= 1e-6),
            "{result}"
        );
    }
    Ok(())
}
