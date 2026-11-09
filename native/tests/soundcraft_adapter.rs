//! Public bundle/CLI boundaries. All default tests execute only the strict fake.
use brownnote_native::{
    adapters::soundcraft::{COMMIT, Report, Request, SamplerMapping, convert},
    export::manifest::{
        AssetEntry, AssetId, AssetKind, EventEntry, EventId, LossRecord, Manifest, PartEntry,
        PartId, PartKind, TimeOrigin, Timing,
    },
};
use serde_json::{Value, json};
use std::{
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
    let status = Command::new(python)
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
    Ok(())
}
