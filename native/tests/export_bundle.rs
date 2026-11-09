#![allow(clippy::unwrap_used)]
//! Independent SMF/WAV readers exercise real project/takes/edit export as well
//! as synthetic transaction faults. Expected timing comes from the fixture,
//! not from the writers' conversion helpers.

use brownnote_native::export::{
    adapter::{self, ProjectFrozenHit},
    audio::{AudioError, AudioSettings, PadSettings},
    bundle::{self, BundleError, BundleRequest, BundleWriter, SystemWriter, staging_path},
    fixture::{
        FixtureAsset, FixtureEdit, FixtureEvent, FixtureExport, FixtureMediaKind, FixturePad,
        FixturePart, FixturePartKind, FixtureTake, FixtureTarget,
    },
    manifest::{
        AssetId, AssetKind, AudioRole, EventId, LossRecord, MANIFEST_FORMAT_VERSION, MANIFEST_NAME,
        MEDIA_DIR, Manifest, ManifestError, PartEntry, PartId, PartKind, PluginIdentity,
        TimeOrigin, Timing,
    },
    midi,
};
use brownnote_native::{
    edit::EditModel,
    pedals::offline::{Backend, Plugin, Stage},
    performance::{
        self, Channel, InstrumentId, LoopBars, NoteKey, OriginalTime, Pitch, SourceId, TakeId,
        Target, Tempo, Velocity,
    },
    project::{
        self, Asset, AssetRef, Document, MediaKind, PadId, SampleAssignment,
        live::{LiveInstrumentDocument, LiveTakeDocument},
        store::{MediaSource, Store},
    },
    takes::{PassOptions, Takes},
};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const ORIGIN: u64 = 1_000_000_000;
const SOURCE_ROOT: &str = "/project";

fn media(label: &str) -> Vec<u8> {
    let (frames, amplitude) = match label {
        "source" => (24_000, 0.25),
        "retained" => (36_000, 0.5),
        _ => (4_800, -0.25),
    };
    encode_fixture(48_000, 1, &vec![amplitude; frames])
}

fn encode_fixture(rate: u32, channels: u16, values: &[f32]) -> Vec<u8> {
    let mut cursor = io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for value in values {
            writer.write_sample(*value).unwrap();
        }
        writer.finalize().unwrap();
    }
    cursor.into_inner()
}

struct TwoPartFixture {
    export: FixtureExport,
    source: Vec<u8>,
    retained: Vec<u8>,
    variant: Vec<u8>,
    source_id: AssetId,
    retained_id: AssetId,
    variant_id: AssetId,
    hit: EventId,
    collision: EventId,
    variant_hit: EventId,
    edited: EventId,
    note: EventId,
}

/// Two parts: pad-hit clips on pad 3 (same-pad collision, per-hit variant,
/// edited onset, retained render with baked processing) and instrument notes.
fn two_part_fixture() -> TwoPartFixture {
    let source = media("source");
    let retained = media("retained");
    let variant = media("variant");
    let source_id = AssetId::of(&source);
    let retained_id = AssetId::of(&retained);
    let variant_id = AssetId::of(&variant);
    let hit = EventId {
        take: 1,
        sequence: 0,
    };
    let collision = EventId {
        take: 1,
        sequence: 1,
    };
    let variant_hit = EventId {
        take: 1,
        sequence: 2,
    };
    let edited = EventId {
        take: 1,
        sequence: 3,
    };
    let note = EventId {
        take: 1,
        sequence: 4,
    };
    let pad_hit =
        |id, start_nanos, duration_nanos, velocity, asset, variant_of, edit| FixtureEvent {
            id,
            target: FixtureTarget::Pad(3),
            start_nanos,
            duration_nanos,
            velocity: Some(velocity),
            pitch: None,
            channel: None,
            asset,
            variant_of,
            edit,
        };
    let export = FixtureExport {
        takes: vec![FixtureTake {
            id: 1,
            recording_tempo_milli_bpm: 116_000,
            events: vec![
                pad_hit(hit, ORIGIN, 500_000_000, 100, None, None, None),
                // Same-pad collision: the browser exportLoop would let the
                // later layer overwrite this; the neutral bundle keeps both.
                pad_hit(
                    collision,
                    ORIGIN + 200_000_000,
                    300_000_000,
                    88,
                    None,
                    None,
                    None,
                ),
                // Per-hit rendered variant of the first hit.
                pad_hit(
                    variant_hit,
                    ORIGIN + 2_000_000_000,
                    200_000_000,
                    96,
                    Some(variant_id),
                    Some(hit),
                    None,
                ),
                // Effective edit moved the onset (quantize to sixteenth).
                pad_hit(
                    edited,
                    ORIGIN + 2_900_000_000,
                    100_000_000,
                    70,
                    None,
                    None,
                    Some(FixtureEdit {
                        effective_start_nanos: ORIGIN + 3_000_000_000,
                        detail: "quantize to sixteenth".into(),
                    }),
                ),
                FixtureEvent {
                    id: note,
                    target: FixtureTarget::Instrument(7),
                    start_nanos: ORIGIN + 1_000_000_000,
                    duration_nanos: 250_000_000,
                    velocity: Some(90),
                    pitch: Some(60),
                    channel: Some(0),
                    asset: None,
                    variant_of: None,
                    edit: None,
                },
            ],
        }],
        pads: vec![FixturePad {
            id: 3,
            source: Some(source_id),
            retained: Some(retained_id),
            baked_chain: vec!["Tape Echo".into()],
            choke: true,
        }],
        assets: vec![
            FixtureAsset {
                id: source_id,
                bytes: source.len() as u64,
                kind: FixtureMediaKind::Source,
            },
            FixtureAsset {
                id: retained_id,
                bytes: retained.len() as u64,
                kind: FixtureMediaKind::RetainedRender,
            },
            FixtureAsset {
                id: variant_id,
                bytes: variant.len() as u64,
                kind: FixtureMediaKind::RetainedRender,
            },
        ],
        parts: vec![
            FixturePart {
                id: PartId(0),
                kind: FixturePartKind::PadClips { pad: 3 },
                event_ids: vec![hit, collision, variant_hit, edited],
            },
            FixturePart {
                id: PartId(1),
                kind: FixturePartKind::InstrumentNotes { instrument: 7 },
                event_ids: vec![note],
            },
        ],
        losses: Vec::new(),
    };
    TwoPartFixture {
        export,
        source,
        retained,
        variant,
        source_id,
        retained_id,
        variant_id,
        hit,
        collision,
        variant_hit,
        edited,
        note,
    }
}

fn request(source_root: &Path, destination: &Path) -> BundleRequest {
    BundleRequest {
        source_root: source_root.to_path_buf(),
        destination: destination.to_path_buf(),
        origin: TimeOrigin { nanos: ORIGIN },
        timing: Timing {
            tempo_milli_bpm: 116_000,
            sample_rate_hz: 48_000,
        },
    }
}

enum MemEntry {
    Dir,
    File(Vec<u8>),
}

/// In-memory BundleWriter: fault tests never touch disk.
#[derive(Default)]
struct MemoryWriter {
    entries: BTreeMap<PathBuf, MemEntry>,
    fail_write_after: Option<usize>,
    writes: usize,
}

impl MemoryWriter {
    fn new() -> Self {
        Self::default()
    }
    fn seed_media(&mut self, root: &Path, id: AssetId, bytes: &[u8]) {
        self.entries.insert(
            root.join(id.relative_path()),
            MemEntry::File(bytes.to_vec()),
        );
    }
    fn seed_source(&mut self, fixture: &TwoPartFixture) {
        let root = Path::new(SOURCE_ROOT);
        self.entries.insert(root.to_path_buf(), MemEntry::Dir);
        self.entries.insert(root.join(MEDIA_DIR), MemEntry::Dir);
        self.seed_media(root, fixture.source_id, &fixture.source);
        self.seed_media(root, fixture.retained_id, &fixture.retained);
        self.seed_media(root, fixture.variant_id, &fixture.variant);
    }
    fn has(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }
    fn file_bytes(&self, path: &Path) -> Option<&[u8]> {
        match self.entries.get(path) {
            Some(MemEntry::File(bytes)) => Some(bytes),
            _ => None,
        }
    }
}

impl BundleWriter for MemoryWriter {
    fn exists(&mut self, path: &Path) -> io::Result<bool> {
        Ok(self.has(path))
    }
    fn ensure_dir(&mut self, path: &Path) -> io::Result<()> {
        self.entries.insert(path.to_path_buf(), MemEntry::Dir);
        Ok(())
    }
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.writes += 1;
        if self
            .fail_write_after
            .is_some_and(|limit| self.writes > limit)
        {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        self.entries
            .insert(path.to_path_buf(), MemEntry::File(bytes.to_vec()));
        Ok(())
    }
    fn sync_dir(&mut self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
    fn read(&mut self, path: &Path) -> io::Result<Vec<u8>> {
        match self.entries.get(path) {
            Some(MemEntry::File(bytes)) => Ok(bytes.clone()),
            Some(MemEntry::Dir) => Err(io::Error::from(io::ErrorKind::InvalidInput)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        let keys: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|key| key.starts_with(from))
            .cloned()
            .collect();
        for key in keys {
            let entry = self.entries.remove(&key).unwrap();
            let rest = key.strip_prefix(from).unwrap();
            self.entries.insert(to.join(rest), entry);
        }
        Ok(())
    }
    fn remove_dir_all(&mut self, path: &Path) -> io::Result<()> {
        let keys: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|key| key.starts_with(path))
            .cloned()
            .collect();
        for key in keys {
            self.entries.remove(&key);
        }
        Ok(())
    }
}

#[test]
fn manifest_joins_resolve_to_stable_ids_for_two_part_fixture() {
    let fixture = two_part_fixture();
    let manifest = bundle::build_manifest(
        &request(Path::new(SOURCE_ROOT), Path::new("/out/bundle")),
        &fixture.export,
    )
    .unwrap();

    // Deterministic traversal: parts by id, events by (onset, id) per part.
    let part_ids: Vec<PartId> = manifest.parts.iter().map(|part| part.id).collect();
    assert_eq!(part_ids, vec![PartId(0), PartId(1)]);
    let part0_events: Vec<EventId> = manifest
        .events
        .iter()
        .filter(|event| event.part == PartId(0))
        .map(|event| event.id)
        .collect();
    assert_eq!(
        part0_events,
        vec![
            fixture.hit,
            fixture.collision,
            fixture.variant_hit,
            fixture.edited
        ]
    );

    // Every event joins to a stable part; asset and variant references resolve.
    assert_eq!(manifest.events.len(), 5);
    for event in &manifest.events {
        assert!(manifest.part(event.part).is_some());
        if let Some(asset) = event.asset {
            assert!(manifest.asset(asset).is_some());
        }
        if let Some(variant_of) = event.variant_of {
            assert!(manifest.event(variant_of).is_some());
        }
    }

    // Loss records resolve to stable IDs and cover the fixture's loss classes.
    assert!(manifest.losses.iter().any(|loss| matches!(
        loss,
        LossRecord::BakedProcessing { part, asset, .. }
            if *part == PartId(0) && *asset == fixture.retained_id
    )));
    assert!(manifest.losses.iter().any(|loss| matches!(
        loss,
        LossRecord::UnsupportedMapping { event, .. } if *event == fixture.collision
    )));
    assert!(manifest.losses.iter().any(|loss| matches!(
        loss,
        LossRecord::ConversionLoss { event, .. } if *event == fixture.edited
    )));
    for (index, loss) in manifest.losses.iter().enumerate() {
        match loss {
            LossRecord::BakedProcessing { part, asset, .. } => {
                assert!(manifest.part(*part).is_some(), "loss {index} part dangles");
                assert!(
                    manifest.asset(*asset).is_some(),
                    "loss {index} asset dangles"
                );
            }
            LossRecord::UnsupportedMapping { event, .. }
            | LossRecord::ConversionLoss { event, .. } => {
                assert!(
                    manifest.event(*event).is_some(),
                    "loss {index} event dangles"
                );
            }
        }
    }

    // The same-pad collision is kept as its own event, never collapsed.
    assert_eq!(
        manifest
            .events
            .iter()
            .filter(|event| event.pad == Some(3))
            .count(),
        4
    );
    // The edited hit exports its effective onset relative to the one origin.
    let edited = manifest.event(fixture.edited).unwrap();
    assert_eq!(edited.start_nanos, 3_000_000_000);
    assert_eq!(edited.duration_nanos, 100_000_000);
    // The per-hit variant references its base hit and its own render.
    let variant = manifest.event(fixture.variant_hit).unwrap();
    assert_eq!(variant.variant_of, Some(fixture.hit));
    assert_eq!(variant.asset, Some(fixture.variant_id));
    // Retained render is bundle media at its content-hash path (D3/D4).
    let retained = manifest.asset(fixture.retained_id).unwrap();
    assert_eq!(retained.path, fixture.retained_id.relative_path());
    assert!(retained.path.starts_with(MEDIA_DIR));
    assert_eq!(retained.bytes as usize, fixture.retained.len());

    // The instrument-note part carries its note with pitch/channel/velocity
    // and no baked asset.
    let note = manifest.event(fixture.note).unwrap();
    assert_eq!(note.part, PartId(1));
    assert_eq!(note.pitch, Some(60));
    assert_eq!(note.channel, Some(0));
    assert_eq!(note.velocity, Some(90));
    assert_eq!(note.asset, None);
    assert_eq!(note.start_nanos, 1_000_000_000);
}

#[test]
fn transaction_stages_manifest_and_media_then_commits_atomically() {
    let fixture = two_part_fixture();
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    let destination = PathBuf::from("/out/bundle");
    let receipt = bundle::write_bundle(
        &mut writer,
        &request(Path::new(SOURCE_ROOT), &destination),
        &fixture.export,
    )
    .unwrap();

    assert_eq!(receipt.destination, destination);
    assert_eq!(receipt.event_count, 5);
    assert!(receipt.asset_count > 3);

    // Published bundle: manifest plus content-hash media; staging is gone.
    assert!(writer.has(&destination));
    assert!(!writer.has(&staging_path(&destination).unwrap()));
    let manifest_bytes = writer
        .file_bytes(&destination.join(MANIFEST_NAME))
        .unwrap()
        .to_vec();
    assert_eq!(AssetId::of(&manifest_bytes), receipt.manifest_id);
    let decoded = Manifest::decode(&manifest_bytes).unwrap();
    assert_eq!(decoded, receipt.manifest);

    let expected = [
        (fixture.source_id, &fixture.source),
        (fixture.retained_id, &fixture.retained),
        (fixture.variant_id, &fixture.variant),
    ];
    for (id, bytes) in expected {
        let copied = writer
            .file_bytes(&destination.join(id.relative_path()))
            .unwrap();
        assert_eq!(copied, bytes.as_slice());
    }

    // The transaction is deterministic: rebuilding produces identical bytes.
    let rebuilt = bundle::write_bundle(
        &mut writer,
        &request(Path::new(SOURCE_ROOT), Path::new("/out/repeat")),
        &fixture.export,
    )
    .unwrap()
    .manifest
    .encode()
    .unwrap();
    assert_eq!(rebuilt, manifest_bytes);
}

#[test]
fn injected_write_failure_never_presents_a_partial_bundle() {
    let fixture = two_part_fixture();
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    // The first media write succeeds; the next staged write fails mid-way.
    writer.fail_write_after = Some(1);
    let destination = PathBuf::from("/out/bundle");
    let result = bundle::write_bundle(
        &mut writer,
        &request(Path::new(SOURCE_ROOT), &destination),
        &fixture.export,
    );

    match &result {
        Err(BundleError::Io { context, source }) => {
            assert!(matches!(*context, "write media" | "write manifest"));
            assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
        }
        other => panic!("expected structured io error, got {other:?}"),
    }
    // No partial bundle is presented as complete: no destination, no staging,
    // no manifest reachable at the destination.
    assert!(!writer.has(&destination));
    assert!(!writer.has(&staging_path(&destination).unwrap()));
    assert!(
        writer
            .file_bytes(&destination.join(MANIFEST_NAME))
            .is_none()
    );
}

#[test]
fn missing_retained_media_aborts_without_a_bundle() {
    let fixture = two_part_fixture();
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    // The per-hit variant render vanished from the project. Retained media is
    // project media (D4): missing is an error, never a cache fallback.
    writer
        .entries
        .remove(&Path::new(SOURCE_ROOT).join(fixture.variant_id.relative_path()));
    let destination = PathBuf::from("/out/bundle");
    let result = bundle::write_bundle(
        &mut writer,
        &request(Path::new(SOURCE_ROOT), &destination),
        &fixture.export,
    );

    assert!(matches!(result, Err(BundleError::MissingMedia(id)) if id == fixture.variant_id));
    assert!(!writer.has(&destination));
    assert!(!writer.has(&staging_path(&destination).unwrap()));
}

#[test]
fn corrupted_source_media_is_a_structured_hash_mismatch() {
    let fixture = two_part_fixture();
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    let mut tampered = fixture.retained.clone();
    tampered[0] ^= 0xff;
    writer.seed_media(Path::new(SOURCE_ROOT), fixture.retained_id, &tampered);
    let destination = PathBuf::from("/out/bundle");
    let result = bundle::write_bundle(
        &mut writer,
        &request(Path::new(SOURCE_ROOT), &destination),
        &fixture.export,
    );

    assert!(matches!(
        result,
        Err(BundleError::MediaHashMismatch { expected, .. }) if expected == fixture.retained_id
    ));
    assert!(!writer.has(&destination));
    assert!(!writer.has(&staging_path(&destination).unwrap()));
}

#[test]
fn existing_destination_is_refused_not_overwritten() {
    let fixture = two_part_fixture();
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    let destination = PathBuf::from("/out/bundle");
    writer.ensure_dir(&destination).unwrap();
    writer
        .write_file(&destination.join(MANIFEST_NAME), b"previous bundle")
        .unwrap();

    let result = bundle::write_bundle(
        &mut writer,
        &request(Path::new(SOURCE_ROOT), &destination),
        &fixture.export,
    );
    assert!(matches!(result, Err(BundleError::DestinationExists)));
    // The pre-existing bundle is untouched.
    assert_eq!(
        writer.file_bytes(&destination.join(MANIFEST_NAME)).unwrap(),
        b"previous bundle"
    );
}

#[test]
fn events_before_the_time_origin_are_rejected() {
    let mut fixture = two_part_fixture();
    fixture.export.takes[0].events[0].start_nanos = ORIGIN - 1;
    let result = bundle::build_manifest(
        &request(Path::new(SOURCE_ROOT), Path::new("/out/bundle")),
        &fixture.export,
    );
    assert!(matches!(
        result,
        Err(BundleError::EventBeforeOrigin { event, .. }) if event == fixture.hit
    ));
}

#[test]
fn manifest_version_round_trip_rejects_unknown_and_newer_versions() {
    let fixture = two_part_fixture();
    let manifest = bundle::build_manifest(
        &request(Path::new(SOURCE_ROOT), Path::new("/out/bundle")),
        &fixture.export,
    )
    .unwrap();
    let bytes = manifest.encode().unwrap();

    // Round trip: serialize/deserialize preserves the manifest exactly.
    let decoded = Manifest::decode(&bytes).unwrap();
    assert_eq!(decoded, manifest);
    assert_eq!(decoded.version, MANIFEST_FORMAT_VERSION);

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // A newer version is a typed error, not a silent partial read.
    value["version"] = serde_json::json!(MANIFEST_FORMAT_VERSION + 1);
    let newer = Manifest::decode(&serde_json::to_vec(&value).unwrap());
    assert!(matches!(
        newer,
        Err(ManifestError::NewerVersion { found, supported })
            if found == MANIFEST_FORMAT_VERSION + 1 && supported == MANIFEST_FORMAT_VERSION
    ));

    // An unknown version is a typed error too.
    value["version"] = serde_json::json!(0);
    let unknown = Manifest::decode(&serde_json::to_vec(&value).unwrap());
    assert!(matches!(unknown, Err(ManifestError::UnknownVersion(0))));

    // Garbage and unknown fields are rejected explicitly.
    assert!(matches!(
        Manifest::decode(b"not json at all"),
        Err(ManifestError::MalformedJson)
    ));
    value["version"] = serde_json::json!(MANIFEST_FORMAT_VERSION);
    value["surprise"] = serde_json::json!(true);
    assert!(matches!(
        Manifest::decode(&serde_json::to_vec(&value).unwrap()),
        Err(ManifestError::MalformedJson)
    ));
}

#[test]
fn live_part_identity_is_a_versioned_checked_record() {
    let fixture = two_part_fixture();
    let manifest = bundle::build_manifest(
        &request(Path::new(SOURCE_ROOT), Path::new("/out/identity")),
        &fixture.export,
    )
    .unwrap();
    // The part kinds written before this slice still read at their version.
    let mut previous: serde_json::Value =
        serde_json::from_slice(&manifest.encode().unwrap()).unwrap();
    previous["version"] = serde_json::json!(2);
    assert_eq!(
        Manifest::decode(&serde_json::to_vec(&previous).unwrap())
            .unwrap()
            .version,
        2
    );

    let identity = PluginIdentity {
        backend: "clap".into(),
        id: "org.brownnote.fixture.instrument".into(),
        artifact: AssetId::of(b"fixture identity").to_string(),
        abi: "clap-1.2".into(),
        version: "0.4.0".into(),
    };
    let mut live = Manifest::new(
        TimeOrigin { nanos: 0 },
        Timing {
            tempo_milli_bpm: 116_000,
            sample_rate_hz: 48_000,
        },
    );
    live.parts.push(PartEntry {
        id: PartId(9),
        kind: PartKind::LiveInstrumentNotes {
            plugin: identity.clone(),
        },
    });
    assert_eq!(live.encode(), Ok(serde_json::to_vec_pretty(&live).unwrap()));
    assert!(
        identity
            .describe()
            .contains("org.brownnote.fixture.instrument")
    );
    assert!(identity.describe().contains(&identity.artifact[..12]));

    // An identity a destination could not act on is refused, never written.
    for broken in [
        PluginIdentity {
            artifact: "not-a-hash".into(),
            ..identity.clone()
        },
        PluginIdentity {
            artifact: identity.artifact.to_uppercase(),
            ..identity.clone()
        },
        PluginIdentity {
            id: String::new(),
            ..identity.clone()
        },
        PluginIdentity {
            version: "0.4.0\npath".into(),
            ..identity.clone()
        },
        PluginIdentity {
            abi: "x".repeat(513),
            ..identity.clone()
        },
    ] {
        let mut bad = live.clone();
        bad.parts[0].kind = PartKind::LiveInstrumentNotes { plugin: broken };
        assert_eq!(
            bad.validate(),
            Err(ManifestError::InvalidPluginIdentity(PartId(9)))
        );
        assert_eq!(
            bad.encode(),
            Err(ManifestError::InvalidPluginIdentity(PartId(9)))
        );
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

fn temporary(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "brownnote-export-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ))
}

#[derive(Debug)]
struct ReadNote {
    track: usize,
    id: EventId,
    channel: u8,
    pitch: u8,
    velocity: u8,
    start: u64,
    end: u64,
}

/// Deliberately independent from export::midi: parse chunks, delta times and
/// FIFO note lifecycles directly, including running status.
fn read_smf(bytes: &[u8]) -> (u16, Vec<u32>, Vec<ReadNote>) {
    fn byte(bytes: &[u8], at: &mut usize) -> u8 {
        let value = bytes[*at];
        *at += 1;
        value
    }
    fn var(bytes: &[u8], at: &mut usize) -> u64 {
        let mut value = 0;
        for _ in 0..4 {
            let next = byte(bytes, at);
            value = (value << 7) | u64::from(next & 127);
            if next & 128 == 0 {
                return value;
            }
        }
        panic!("SMF variable length quantity exceeds four bytes");
    }
    assert_eq!(&bytes[..4], b"MThd");
    assert_eq!(u32::from_be_bytes(bytes[4..8].try_into().unwrap()), 6);
    assert_eq!(u16::from_be_bytes(bytes[8..10].try_into().unwrap()), 1);
    let count = usize::from(u16::from_be_bytes(bytes[10..12].try_into().unwrap()));
    let ppq = u16::from_be_bytes(bytes[12..14].try_into().unwrap());
    let mut offset = 14;
    let mut tempos = Vec::new();
    let mut notes = Vec::new();
    for track in 0..count {
        assert_eq!(&bytes[offset..offset + 4], b"MTrk");
        let size = u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let body = &bytes[offset..offset + size];
        offset += size;
        let mut at = 0;
        let mut tick = 0;
        let mut running = None;
        let mut marker = None;
        type ActiveNotes = BTreeMap<(u8, u8), std::collections::VecDeque<(EventId, u8, u64)>>;
        let mut active = ActiveNotes::new();
        let mut ended = false;
        while at < body.len() {
            tick += var(body, &mut at);
            let first = byte(body, &mut at);
            let status = if first < 128 {
                at -= 1;
                running.unwrap()
            } else {
                first
            };
            match status {
                0xff => {
                    running = None;
                    let kind = byte(body, &mut at);
                    let len = var(body, &mut at) as usize;
                    let data = &body[at..at + len];
                    at += len;
                    match kind {
                        0x51 => {
                            assert_eq!(data.len(), 3);
                            tempos.push(
                                u32::from(data[0]) << 16
                                    | u32::from(data[1]) << 8
                                    | u32::from(data[2]),
                            );
                        }
                        1 if data.starts_with(b"bn:") => {
                            let fields: Vec<_> =
                                std::str::from_utf8(data).unwrap().split(':').collect();
                            marker = Some(EventId {
                                take: fields[1].parse().unwrap(),
                                sequence: fields[2].parse().unwrap(),
                            });
                        }
                        0x2f => {
                            assert!(data.is_empty());
                            ended = true;
                            assert_eq!(at, body.len());
                        }
                        _ => {}
                    }
                }
                0x80..=0x9f => {
                    running = Some(status);
                    let pitch = byte(body, &mut at);
                    let velocity = byte(body, &mut at);
                    let channel = status & 15;
                    if status & 0xf0 == 0x90 && velocity != 0 {
                        active.entry((channel, pitch)).or_default().push_back((
                            marker.take().unwrap(),
                            velocity,
                            tick,
                        ));
                    } else {
                        let (id, velocity, start) = active
                            .get_mut(&(channel, pitch))
                            .unwrap()
                            .pop_front()
                            .unwrap();
                        notes.push(ReadNote {
                            track,
                            id,
                            channel,
                            pitch,
                            velocity,
                            start,
                            end: tick,
                        });
                    }
                }
                other => panic!("unexpected SMF event {other:x}"),
            }
        }
        assert!(ended);
        assert!(active.values().all(|queue| queue.is_empty()));
    }
    assert_eq!(offset, bytes.len());
    (ppq, tempos, notes)
}

fn read_wav(bytes: &[u8]) -> (u32, Vec<[f32; 2]>) {
    let mut reader = hound::WavReader::new(io::Cursor::new(bytes)).unwrap();
    assert_eq!(reader.spec().channels, 2);
    assert_eq!(reader.spec().sample_format, hound::SampleFormat::Float);
    assert_eq!(reader.spec().bits_per_sample, 32);
    let values = reader
        .samples::<f32>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(values.iter().all(|v| v.is_finite()));
    let (frames, remainder) = values.as_chunks::<2>();
    assert!(remainder.is_empty());
    let frames = frames.to_vec();
    (reader.spec().sample_rate, frames)
}

struct LiveFixture {
    document: Document,
    media: BTreeMap<project::AssetId, Vec<u8>>,
    takes: Takes,
    edits: EditModel,
    frozen: BTreeMap<performance::EventId, ProjectFrozenHit>,
    ids: Vec<performance::EventId>,
}

fn live_fixture() -> LiveFixture {
    let source = encode_fixture(48_000, 1, &vec![0.25; 48_000]);
    let retained = encode_fixture(44_100, 1, &vec![0.5; 44_100]);
    let wet = encode_fixture(48_000, 2, &[0.2, -0.1].repeat(36_000));
    let dry = encode_fixture(48_000, 2, &[0.05, 0.025].repeat(30_000));
    let source_id = project::AssetId::of(&source);
    let retained_id = project::AssetId::of(&retained);
    let wet_id = project::AssetId::of(&wet);
    let dry_id = project::AssetId::of(&dry);
    let media: BTreeMap<_, _> = [source, retained, wet, dry]
        .into_iter()
        .map(|bytes| (project::AssetId::of(&bytes), bytes))
        .collect();
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
        retained: Some(retained_id),
    });
    document.pads[3].gain = 0.75;
    document.pads[3].pan = 1.0;
    document.pads[3].semitones = 12.0;
    document.pads[3]
        .chain
        .stages
        .push(brownnote_native::pedals::render::Stage {
            effect: "Unavailable frozen plugin".into(),
            pots: Vec::new(),
            mix: None,
        });
    let mut takes = Takes::new(
        LoopBars::try_from(1).unwrap(),
        TakeId(u128::from(u64::MAX) + 123),
    );
    takes.set_tempo(Tempo::try_from(120_000).unwrap()).unwrap();
    let take = takes.arm(PassOptions::default()).unwrap();
    takes.start(take).unwrap();
    let pad = Target::Pad(PadId::try_from(3).unwrap());
    let instrument = Target::Instrument(InstrumentId(u128::from(u64::MAX) + 456));
    // The first two instrument notes overlap on the same channel/pitch.
    let specs = [
        (pad, 0, 36, 127, 11_000_000, 31_000_000),
        (pad, 0, 36, 100, 121_000_000, 151_000_000),
        (pad, 0, 36, 90, 251_000_000, 281_000_000), // deleted
        (pad, 0, 36, 80, 2_500_000_000, 2_600_000_000), // hidden by loop
        (instrument, 0, 60, 91, 500_000_000, 1_000_000_000),
        (instrument, 0, 60, 72, 625_000_000, 1_250_000_000),
        (instrument, 15, 64, 88, 750_000_000, 937_500_000),
        (instrument, 9, 67, 111, 875_000_000, 1_000_000_000),
        (instrument, 1, 60, 63, 1_000_000_000, 1_250_000_000),
    ];
    let key = |channel, pitch| NoteKey {
        source: SourceId(23),
        channel: Channel::try_from(channel).unwrap(),
        pitch: Pitch::try_from(pitch).unwrap(),
    };
    let mut actions = Vec::new();
    for (index, (_, _, _, _, start, end)) in specs.iter().enumerate() {
        actions.push((*start, false, index));
        actions.push((*end, true, index));
    }
    actions.sort();
    let mut ids = vec![None; specs.len()];
    for (time, off, index) in actions {
        let (target, channel, pitch, velocity, _, _) = specs[index];
        if off {
            takes
                .note_off(take, key(channel, pitch), OriginalTime::from_nanos(time))
                .unwrap();
        } else {
            ids[index] = Some(
                takes
                    .note_on(
                        take,
                        target,
                        key(channel, pitch),
                        Velocity::try_from(velocity).unwrap(),
                        OriginalTime::from_nanos(time),
                    )
                    .unwrap(),
            );
        }
    }
    takes.finish(take).unwrap();
    let ids: Vec<_> = ids.into_iter().map(Option::unwrap).collect();
    let mut edits = EditModel::new();
    edits.toggle_quantization(&takes, take).unwrap();
    edits.nudge(&takes, ids[0], 20_000_000).unwrap();
    edits
        .set_velocity(&takes, ids[0], Velocity::try_from(64).unwrap())
        .unwrap();
    edits.delete(&takes, ids[2]).unwrap();
    // Committed but muted/removed takes must not leak into any export writer.
    for removed in [false, true] {
        let extra = takes.arm(PassOptions::default()).unwrap();
        takes.start(extra).unwrap();
        takes
            .note_on(
                extra,
                instrument,
                key(0, 99),
                Velocity::try_from(127).unwrap(),
                OriginalTime::from_nanos(0),
            )
            .unwrap();
        takes
            .note_off(extra, key(0, 99), OriginalTime::from_nanos(100_000_000))
            .unwrap();
        takes.finish(extra).unwrap();
        if removed {
            edits.remove_take(&mut takes, extra).unwrap();
        } else {
            edits.set_muted(&mut takes, extra, true).unwrap();
        }
    }
    let frozen = ids[4..]
        .iter()
        .map(|id| {
            (
                *id,
                ProjectFrozenHit {
                    wet: wet_id,
                    dry: Some(dry_id),
                    detail: "instrument render frozen as project media; backend unavailable".into(),
                },
            )
        })
        .collect();
    LiveFixture {
        document,
        media,
        takes,
        edits,
        frozen,
        ids,
    }
}

fn neutral_id(id: performance::EventId) -> EventId {
    EventId {
        take: id.take.0,
        sequence: id.sequence,
    }
}

#[test]
fn real_store_effective_view_writes_five_instrument_notes_and_aligned_audio_after_cache_removal() {
    let fixture = live_fixture();
    let base = temporary("real-store");
    fs::create_dir_all(base.join("original-plugin-cache")).unwrap();
    let project = base.join("project");
    let mut sources = Vec::new();
    for (id, bytes) in &fixture.media {
        let path = base.join("original-plugin-cache").join(format!("{id}.wav"));
        fs::write(&path, bytes).unwrap();
        sources.push(MediaSource { id: *id, path });
    }
    Store::new(project.clone())
        .save(&fixture.document, &sources)
        .unwrap();
    fs::remove_dir_all(base.join("original-plugin-cache")).unwrap();
    let destination = base.join("bundle");
    let mut request = request(&project, &destination);
    request.timing.tempo_milli_bpm = 120_000;
    let receipt = adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &request,
        &fixture.takes,
        &fixture.edits,
        &fixture.frozen,
    )
    .unwrap();
    assert_eq!(receipt.event_count, 7); // two hits, five notes; no hidden/deleted/muted/removed events
    assert_eq!(receipt.manifest.parts.len(), 2);
    let manifest = Manifest::decode(&fs::read(destination.join(MANIFEST_NAME)).unwrap()).unwrap();
    assert_eq!(manifest, receipt.manifest);
    assert_eq!(manifest.origin.nanos, ORIGIN);
    for event in &manifest.events {
        assert!(event.id.take > u128::from(u64::MAX));
    }
    let (ppq, tempos, notes) = read_smf(&fs::read(destination.join("notes.mid")).unwrap());
    assert_eq!(ppq, 960);
    assert_eq!(tempos, [500_000]);
    assert_eq!(notes.len(), 5);
    let expected = [
        (4, 0, 60, 91, 960, 1920),
        (5, 0, 60, 72, 1200, 2400),
        (6, 15, 64, 88, 1440, 1800),
        (7, 9, 67, 111, 1680, 1920),
        (8, 1, 60, 63, 1920, 2400),
    ];
    for (index, channel, pitch, velocity, start, end) in expected {
        let id = neutral_id(fixture.ids[index]);
        let note = notes.iter().find(|note| note.id == id).unwrap();
        assert_eq!(
            (
                note.channel,
                note.pitch,
                note.velocity,
                note.start,
                note.end
            ),
            (channel, pitch, velocity, start, end)
        );
        let join = manifest
            .midi
            .as_ref()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.index as usize == note.track)
            .unwrap();
        assert!(join.events.contains(&id));
        assert_eq!(manifest.event(id).unwrap().part, join.part);
    }
    assert_eq!(
        notes
            .iter()
            .find(|n| n.id == neutral_id(fixture.ids[4]))
            .unwrap()
            .track,
        notes
            .iter()
            .find(|n| n.id == neutral_id(fixture.ids[5]))
            .unwrap()
            .track
    );
    let first = neutral_id(fixture.ids[0]);
    let second = neutral_id(fixture.ids[1]);
    let first_entry = manifest
        .audio
        .iter()
        .find(|a| a.event == Some(first) && a.role == AudioRole::HitWet)
        .unwrap();
    let second_entry = manifest
        .audio
        .iter()
        .find(|a| a.event == Some(second) && a.role == AudioRole::HitWet)
        .unwrap();
    assert_eq!((first_entry.start_frame, first_entry.frames), (0, 6_240)); // 125 ms + 5 ms fade
    assert_eq!(
        (second_entry.start_frame, second_entry.frames),
        (6_000, 24_000)
    ); // octave-up 44.1 kHz retained tail
    let (_, first_wav) =
        read_wav(&fs::read(destination.join(first_entry.asset.relative_path())).unwrap());
    let amplitude = 0.5 * (64.0 / 127.0) * 0.75;
    assert!((first_wav[0][1] - amplitude).abs() < 1e-6);
    assert!(first_wav[0][0].abs() < 1e-6);
    assert!((first_wav[6_120][1] - amplitude * 0.5).abs() < 1e-6);
    assert!((first_wav[6_239][1] - amplitude / 240.0).abs() < 1e-6);
    let instrument_hit = manifest
        .audio
        .iter()
        .find(|a| a.event == Some(neutral_id(fixture.ids[4])) && a.role == AudioRole::HitWet)
        .unwrap();
    assert_eq!(
        (instrument_hit.start_frame, instrument_hit.frames),
        (24_000, 36_000)
    );
    let (_, instrument_wav) =
        read_wav(&fs::read(destination.join(instrument_hit.asset.relative_path())).unwrap());
    assert_eq!(instrument_wav[0], [0.2, -0.1]); // no second velocity, pan or choke application
    let fallbacks: Vec<_> = manifest
        .audio
        .iter()
        .filter(|a| a.event.is_none())
        .collect();
    assert_eq!(fallbacks.len(), 5); // wet/dry for both parts plus reference
    for audio in fallbacks {
        assert_eq!((audio.start_frame, audio.frames), (0, 84_000));
        let (rate, frames) =
            read_wav(&fs::read(destination.join(audio.asset.relative_path())).unwrap());
        assert_eq!(rate, 48_000);
        assert_eq!(frames.len(), 84_000);
    }
    let reference = manifest
        .audio
        .iter()
        .find(|a| a.role == AudioRole::Reference)
        .unwrap();
    let (_, reference) =
        read_wav(&fs::read(destination.join(reference.asset.relative_path())).unwrap());
    assert!((reference[0][1] - amplitude * 0.8).abs() < 1e-6);
    for event in &manifest.events {
        assert!(manifest.part(event.part).is_some());
        assert!(manifest.asset(event.asset.unwrap()).is_some());
    }
    for asset in &manifest.assets {
        assert_eq!(
            AssetId::of(&fs::read(destination.join(&asset.path)).unwrap()),
            asset.id
        );
    }
    // Actual relocation, followed by removal of every source project/plugin path.
    let relocated = base.join("relocated");
    fs::rename(&destination, &relocated).unwrap();
    fs::remove_dir_all(&project).unwrap();
    let reopened = Manifest::decode(&fs::read(relocated.join(MANIFEST_NAME)).unwrap()).unwrap();
    for audio in &reopened.audio {
        let (rate, frames) =
            read_wav(&fs::read(relocated.join(audio.asset.relative_path())).unwrap());
        assert_eq!(rate, 48_000);
        assert_eq!(frames.len() as u64, audio.frames);
    }
    assert_eq!(
        read_smf(&fs::read(relocated.join("notes.mid")).unwrap())
            .2
            .len(),
        5
    );
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn midi_rounding_minimum_gates_and_non_fifo_overlaps_preserve_every_note() {
    let mut fixture = two_part_fixture();
    let template = fixture.export.takes[0].events[4].clone();
    let cases = [
        (0, 500_000_000, 0, 60),
        (125_000_000, 125_000_000, 0, 60), // nested gate needs its own track
        (260_416, 1, 15, 72),              // just before a half-tick
        (260_417, 2, 9, 73),               // just after a half-tick
        (3_000_000_001, 333_333_333, 1, 60),
        (3_500_000_000, 0, 2, 61), // zero-length original still has a note-on/off
    ];
    fixture.export.parts[1].event_ids.clear();
    fixture.export.takes[0].events.truncate(4);
    for (index, (start, duration, channel, pitch)) in cases.iter().enumerate() {
        let mut event = template.clone();
        event.id.sequence = 100 + index as u64;
        event.start_nanos = ORIGIN + start;
        event.duration_nanos = *duration;
        event.channel = Some(*channel);
        event.pitch = Some(*pitch);
        fixture.export.parts[1].event_ids.push(event.id);
        fixture.export.takes[0].events.push(event);
    }
    let mut request = request(Path::new(SOURCE_ROOT), Path::new("/out/midi"));
    request.timing.tempo_milli_bpm = 120_000;
    let manifest = bundle::build_manifest(&request, &fixture.export).unwrap();
    let encoded = midi::encode(&manifest).unwrap();
    let (ppq, tempos, decoded) = read_smf(&encoded.bytes);
    assert_eq!((ppq, tempos), (960, vec![500_000]));
    assert_eq!(decoded.len(), cases.len()); // ordinary pad hits never enter MIDI
    let tick_nanos = 500_000_000.0 / 960.0;
    for (index, (start, duration, channel, pitch)) in cases.iter().enumerate() {
        let note = decoded
            .iter()
            .find(|n| n.id.sequence == 100 + index as u64)
            .unwrap();
        assert_eq!(
            (note.channel, note.pitch, note.velocity),
            (*channel, *pitch, 90)
        );
        assert!((note.start as f64 * tick_nanos - *start as f64).abs() <= tick_nanos * 0.5);
        assert!(
            ((note.end - note.start) as f64 * tick_nanos - *duration as f64).abs() <= tick_nanos
        );
        assert!(note.end > note.start);
    }
    assert_ne!(
        decoded.iter().find(|n| n.id.sequence == 100).unwrap().track,
        decoded.iter().find(|n| n.id.sequence == 101).unwrap().track
    );
    assert!(encoded.losses.iter().any(|loss| matches!(loss,
        LossRecord::ConversionLoss { detail, .. } if detail.contains("non-FIFO"))));
    // Nonintegral tempo conversion uses the emitted tempo rather than allowing
    // a long-rest audio/MIDI drift. Expected tempo is calculated independently.
    request.timing.tempo_milli_bpm = 116_000;
    let last = fixture.export.takes[0].events.last_mut().unwrap();
    last.start_nanos = ORIGIN + 3_600_000_000_000;
    last.duration_nanos = 1_234_567_890;
    let manifest = bundle::build_manifest(&request, &fixture.export).unwrap();
    let (_, tempos, decoded) = read_smf(&midi::encode(&manifest).unwrap().bytes);
    assert_eq!(tempos, [517_241]);
    let last = decoded.iter().find(|n| n.id.sequence == 105).unwrap();
    let tick_nanos = 517_241_000.0 / 960.0;
    assert!((last.start as f64 * tick_nanos - 3_600_000_000_000.0).abs() <= tick_nanos * 0.5);
}

#[test]
fn real_adapter_preserves_tempo_positions_and_reversible_quantization_without_rewriting_originals()
{
    let mut fixture = live_fixture();
    let first_take = fixture.ids[0].take;
    let mut request = request(Path::new(SOURCE_ROOT), Path::new("/out/edits"));
    request.timing.tempo_milli_bpm = 120_000;
    for (quantized, bpm, expected_hit, expected_note) in [
        (true, 120_000, 0, 500_000_000),
        (false, 120_000, 31_000_000, 500_000_000),
        (false, 60_000, 62_000_000, 1_000_000_000),
        (true, 60_000, 0, 1_000_000_000),
    ] {
        let currently_quantized =
            fixture.edits.quantization(first_take) == performance::Quantization::Sixteenth;
        if quantized != currently_quantized {
            fixture
                .edits
                .toggle_quantization(&fixture.takes, first_take)
                .unwrap();
        }
        fixture
            .takes
            .set_tempo(Tempo::try_from(bpm).unwrap())
            .unwrap();
        request.timing.tempo_milli_bpm = bpm;
        let prepared = adapter::prepare(
            &request,
            &fixture.document,
            &fixture.takes,
            &fixture.edits,
            &fixture.frozen,
        )
        .unwrap();
        let manifest = bundle::build_manifest(&request, &prepared.events).unwrap();
        assert_eq!(
            manifest
                .event(neutral_id(fixture.ids[0]))
                .unwrap()
                .start_nanos,
            expected_hit
        );
        assert_eq!(
            manifest
                .event(neutral_id(fixture.ids[4]))
                .unwrap()
                .start_nanos,
            expected_note
        );
        let (_, _, notes) = read_smf(&midi::encode(&manifest).unwrap().bytes);
        let note = notes
            .iter()
            .find(|n| n.id == neutral_id(fixture.ids[4]))
            .unwrap();
        assert_eq!((note.start, note.end), (960, 1920)); // musical position does not change
    }
    assert_eq!(
        fixture.takes.take(first_take).unwrap().notes()[0]
            .on()
            .original_time
            .nanos(),
        11_000_000
    );
    assert_eq!(
        fixture.takes.take(first_take).unwrap().notes()[0]
            .on()
            .velocity
            .value(),
        127
    );
}

#[test]
fn all_staged_wav_midi_and_manifest_write_failures_leave_no_partial_bundle() {
    let fixture = two_part_fixture();
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/faults"));
    let mut baseline = MemoryWriter::new();
    baseline.seed_source(&fixture);
    bundle::write_bundle(&mut baseline, &req, &fixture.export).unwrap();
    let writes = baseline.writes;
    assert!(writes > fixture.export.assets.len() + 2);
    for successful_writes in 0..writes {
        let mut writer = MemoryWriter::new();
        writer.seed_source(&fixture);
        writer.fail_write_after = Some(successful_writes);
        let result = bundle::write_bundle(&mut writer, &req, &fixture.export);
        assert!(
            matches!(result, Err(BundleError::Io { .. })),
            "boundary {successful_writes}"
        );
        assert!(
            !writer.has(&req.destination),
            "boundary {successful_writes}"
        );
        assert!(!writer.has(&staging_path(&req.destination).unwrap()));
    }
}

#[test]
fn real_part_ids_do_not_shift_when_another_part_is_deleted_or_restored() {
    let mut fixture = live_fixture();
    let mut req = request(Path::new(SOURCE_ROOT), Path::new("/out/stable-parts"));
    req.timing.tempo_milli_bpm = 120_000;
    let before = adapter::prepare(
        &req,
        &fixture.document,
        &fixture.takes,
        &fixture.edits,
        &fixture.frozen,
    )
    .unwrap();
    let instrument = before
        .events
        .parts
        .iter()
        .find(|p| matches!(p.kind, FixturePartKind::InstrumentNotes { .. }))
        .unwrap()
        .id;
    let pad = before
        .events
        .parts
        .iter()
        .find(|p| matches!(p.kind, FixturePartKind::PadClips { .. }))
        .unwrap()
        .id;
    for id in &fixture.ids[..2] {
        fixture.edits.delete(&fixture.takes, *id).unwrap();
    }
    let after = adapter::prepare(
        &req,
        &fixture.document,
        &fixture.takes,
        &fixture.edits,
        &fixture.frozen,
    )
    .unwrap();
    assert_eq!(after.events.parts.len(), 1);
    assert_eq!(after.events.parts[0].id, instrument);
    fixture
        .edits
        .set_deleted(&fixture.takes, fixture.ids[0], false)
        .unwrap();
    let restored = adapter::prepare(
        &req,
        &fixture.document,
        &fixture.takes,
        &fixture.edits,
        &fixture.frozen,
    )
    .unwrap();
    assert_eq!(
        restored
            .events
            .parts
            .iter()
            .find(|p| matches!(p.kind, FixturePartKind::PadClips { .. }))
            .unwrap()
            .id,
        pad
    );
}

#[test]
fn missing_frozen_media_invalid_wav_and_invalid_midi_abort_before_publication() {
    let fixture = two_part_fixture();
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/invalid"));
    let mut export = fixture.export.clone();
    export.takes[0].events[4].channel = Some(16);
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    assert!(matches!(
        bundle::write_bundle(&mut writer, &req, &export),
        Err(BundleError::Midi(midi::MidiError::InvalidNote(_)))
    ));
    assert!(!writer.has(&req.destination));
    // A valid content hash is not evidence of valid audio.
    let invalid = b"not a WAV";
    let id = AssetId::of(invalid);
    export = fixture.export.clone();
    export.assets.push(FixtureAsset {
        id,
        bytes: invalid.len() as u64,
        kind: FixtureMediaKind::RetainedRender,
    });
    writer.seed_media(Path::new(SOURCE_ROOT), id, invalid);
    assert!(matches!(bundle::write_bundle(&mut writer, &req, &export),
        Err(BundleError::Audio(AudioError::InvalidWav(found))) if found == id));
    assert!(!writer.has(&req.destination));
    // A required frozen plugin/instrument render cannot fall back to a cache.
    let live = live_fixture();
    let mut writer = MemoryWriter::new();
    for (id, bytes) in &live.media {
        writer.seed_media(
            Path::new(SOURCE_ROOT),
            id.to_string().parse().unwrap(),
            bytes,
        );
    }
    let missing = live.frozen.values().next().unwrap().wet;
    writer
        .entries
        .remove(&Path::new(SOURCE_ROOT).join(missing.relative_path()));
    let mut req = req;
    req.timing.tempo_milli_bpm = 120_000;
    assert!(matches!(
        adapter::write_project_bundle(
            &mut writer,
            &req,
            &live.document,
            &live.takes,
            &live.edits,
            &live.frozen
        ),
        Err(adapter::AdapterError::Bundle(BundleError::MissingMedia(_)))
    ));
    assert!(!writer.has(&req.destination));
}

#[test]
fn pcm16_stereo_pitch_and_resampling_use_native_downmix_and_linear_interpolation() {
    let mut cursor = io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels: 2,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for value in [0_i16, 16_384, 16_384, 24_576, 24_576, 0, -16_384, 16_384] {
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }
    let bytes = cursor.into_inner();
    let id = AssetId::of(&bytes);
    let mut fixture = two_part_fixture();
    fixture.export.assets = vec![FixtureAsset {
        id,
        bytes: bytes.len() as u64,
        kind: FixtureMediaKind::Source,
    }];
    fixture.export.pads[0].source = Some(id);
    fixture.export.pads[0].retained = None;
    fixture.export.pads[0].choke = false;
    fixture.export.pads[0].baked_chain.clear();
    fixture.export.parts.truncate(1);
    fixture.export.parts[0].event_ids.truncate(1);
    fixture.export.takes[0].events.truncate(1);
    fixture.export.takes[0].events[0].velocity = Some(127);
    let mut writer = MemoryWriter::new();
    writer.seed_media(Path::new(SOURCE_ROOT), id, &bytes);
    let settings = AudioSettings {
        pads: [(
            3,
            PadSettings {
                gain: 1.0,
                pan: -1.0,
                semitones: 12.0,
            },
        )]
        .into(),
        ..AudioSettings::default()
    };
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/pcm"));
    let receipt =
        bundle::write_bundle_with_settings(&mut writer, &req, &fixture.export, &settings).unwrap();
    let hit = receipt
        .manifest
        .audio
        .iter()
        .find(|a| a.role == AudioRole::HitWet)
        .unwrap();
    assert_eq!(hit.frames, 12); // 4 input frames, 8->48 kHz, double playback rate
    let (_, frames) = read_wav(
        writer
            .file_bytes(&req.destination.join(hit.asset.relative_path()))
            .unwrap(),
    );
    // Downmixed input = [0.25, 0.625, 0.375, 0], step = 1/3.
    for (index, expected) in [
        (0, 0.25),
        (1, 0.375),
        (2, 0.5),
        (3, 0.625),
        (6, 0.375),
        (9, 0.0),
    ] {
        assert!((frames[index][0] - expected).abs() < 1e-6, "frame {index}");
        assert_eq!(frames[index][1], 0.0);
    }
}

#[test]
fn audio_fallbacks_report_missing_instrument_renders_instead_of_claiming_complete_sound() {
    let fixture = two_part_fixture();
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/no-instrument"));
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    let receipt = bundle::write_bundle(&mut writer, &req, &fixture.export).unwrap();
    assert_eq!(
        read_smf(
            writer
                .file_bytes(&req.destination.join("notes.mid"))
                .unwrap()
        )
        .2
        .len(),
        1
    );
    assert!(receipt.manifest.losses.iter().any(|loss| matches!(loss,
        LossRecord::UnsupportedMapping { event, reason }
        if *event == fixture.note && reason.contains("omitted from audio fallbacks"))));
}

#[test]
fn shared_source_pads_choke_across_parts_and_frozen_variants_are_not_processed_twice() {
    let mut fixture = two_part_fixture();
    let mut pad = fixture.export.pads[0].clone();
    pad.id = 4;
    fixture.export.pads.push(pad);
    let mut other = fixture.export.takes[0].events[0].clone();
    other.id.sequence = 99;
    other.target = FixtureTarget::Pad(4);
    other.start_nanos = ORIGIN + 100_000_000;
    fixture.export.parts.push(FixturePart {
        id: PartId(2),
        kind: FixturePartKind::PadClips { pad: 4 },
        event_ids: vec![other.id],
    });
    fixture.export.takes[0].events.push(other);
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/cross-pad"));
    let receipt = bundle::write_bundle(&mut writer, &req, &fixture.export).unwrap();
    let first = receipt
        .manifest
        .audio
        .iter()
        .find(|a| a.event == Some(fixture.hit) && a.role == AudioRole::HitWet)
        .unwrap();
    assert_eq!(first.frames, 5_040); // another pad sharing the source starts at 4800 + 240 fade
    let variant = receipt
        .manifest
        .audio
        .iter()
        .find(|a| a.event == Some(fixture.variant_hit) && a.role == AudioRole::HitWet)
        .unwrap();
    let (_, frames) = read_wav(
        writer
            .file_bytes(&req.destination.join(variant.asset.relative_path()))
            .unwrap(),
    );
    assert_eq!(frames[0], [-0.25, -0.25]); // authoritative render, no pan/velocity pass
    assert!(
        receipt
            .manifest
            .events
            .iter()
            .all(|e| e.source.is_some() || e.id == fixture.note)
    );
}

#[test]
fn reference_clamps_only_after_summing_and_stems_remain_unclamped() {
    let mut fixture = two_part_fixture();
    fixture.export.pads[0].choke = false;
    fixture.export.parts.truncate(1);
    fixture.export.parts[0].event_ids.truncate(2);
    fixture.export.takes[0].events.truncate(2);
    for event in &mut fixture.export.takes[0].events {
        event.start_nanos = ORIGIN;
        event.velocity = Some(127);
    }
    let settings = AudioSettings {
        pads: [(
            3,
            PadSettings {
                gain: 1.5,
                pan: 0.0,
                semitones: 0.0,
            },
        )]
        .into(),
        ..AudioSettings::default()
    };
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/clamp"));
    let manifest =
        bundle::write_bundle_with_settings(&mut writer, &req, &fixture.export, &settings)
            .unwrap()
            .manifest;
    let stem = manifest
        .audio
        .iter()
        .find(|a| a.role == AudioRole::StemWet)
        .unwrap();
    let reference = manifest
        .audio
        .iter()
        .find(|a| a.role == AudioRole::Reference)
        .unwrap();
    let (_, stem) = read_wav(
        writer
            .file_bytes(&req.destination.join(stem.asset.relative_path()))
            .unwrap(),
    );
    let (_, reference) = read_wav(
        writer
            .file_bytes(&req.destination.join(reference.asset.relative_path()))
            .unwrap(),
    );
    let expected = 2.0 * 0.5 * 1.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!((stem[0][0] - expected).abs() < 1e-6 && stem[0][0] > 1.0);
    assert_eq!(reference[0], [1.0, 1.0]);
}

#[test]
fn legacy_v1_manifests_read_and_new_audio_midi_joins_reject_tampering() {
    let fixture = two_part_fixture();
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/joins"));
    let mut legacy =
        serde_json::to_value(bundle::build_manifest(&req, &fixture.export).unwrap()).unwrap();
    legacy["version"] = serde_json::json!(1);
    for event in legacy["events"].as_array_mut().unwrap() {
        event["id"]["take"] = serde_json::json!(1);
        if !event["variant_of"].is_null() {
            event["variant_of"]["take"] = serde_json::json!(1);
        }
        event.as_object_mut().unwrap().remove("source");
    }
    for loss in legacy["losses"].as_array_mut().unwrap() {
        if !loss["event"].is_null() {
            loss["event"]["take"] = serde_json::json!(1);
        }
    }
    legacy["parts"][1]["kind"]["instrument"] = serde_json::json!(7);
    assert_eq!(
        Manifest::decode(&serde_json::to_vec(&legacy).unwrap())
            .unwrap()
            .version,
        1
    );
    assert_eq!(
        Manifest::decode(&serde_json::to_vec(&legacy).unwrap())
            .unwrap()
            .encode(),
        Err(ManifestError::ReadOnlyVersion(1))
    );
    let mut writer = MemoryWriter::new();
    writer.seed_source(&fixture);
    let manifest = bundle::write_bundle(&mut writer, &req, &fixture.export)
        .unwrap()
        .manifest;
    let mut bad = manifest.clone();
    bad.audio[0].asset = AssetId::of(b"missing");
    assert_eq!(bad.validate(), Err(ManifestError::InvalidAudio));
    let mut bad = manifest.clone();
    bad.midi.as_mut().unwrap().tracks[0].events = vec![fixture.hit];
    assert_eq!(bad.validate(), Err(ManifestError::InvalidMidi));
    let mut bad = manifest;
    bad.audio
        .iter_mut()
        .find(|a| a.role == AudioRole::Reference)
        .unwrap()
        .start_frame = 1;
    assert_eq!(bad.validate(), Err(ManifestError::InvalidAudio));
}

#[test]
fn pad_hits_cannot_be_mislabeled_as_instrument_notes() {
    let mut fixture = two_part_fixture();
    fixture.export.parts[1].event_ids.push(fixture.hit);
    let req = request(Path::new(SOURCE_ROOT), Path::new("/out/mislabeled"));
    assert!(matches!(
        bundle::build_manifest(&req, &fixture.export),
        Err(BundleError::TargetPartMismatch(id)) if id == fixture.hit
    ));
    let mut manifest = bundle::build_manifest(&req, &two_part_fixture().export).unwrap();
    manifest.events[0].part = PartId(1);
    manifest.events[0].pitch = Some(60);
    manifest.events[0].channel = Some(0);
    assert_eq!(
        midi::encode(&manifest).err(),
        Some(midi::MidiError::InvalidNote(fixture.hit))
    );
}

#[test]
fn system_writer_publishes_a_relocatable_bundle_on_disk() {
    let fixture = two_part_fixture();
    let base = std::env::temp_dir().join(format!(
        "brownnote-export-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let source_root = base.join("project");
    let destination = base.join("out/bundle");
    fs::create_dir_all(source_root.join(MEDIA_DIR)).unwrap();
    for (id, bytes) in [
        (fixture.source_id, &fixture.source),
        (fixture.retained_id, &fixture.retained),
        (fixture.variant_id, &fixture.variant),
    ] {
        fs::write(source_root.join(id.relative_path()), bytes).unwrap();
    }

    let mut writer = SystemWriter;
    let receipt = bundle::write_bundle(
        &mut writer,
        &request(&source_root, &destination),
        &fixture.export,
    )
    .unwrap();

    // The manifest and every media file are on disk and decode back.
    let bytes = fs::read(destination.join(MANIFEST_NAME)).unwrap();
    assert_eq!(AssetId::of(&bytes), receipt.manifest_id);
    let decoded = Manifest::decode(&bytes).unwrap();
    assert_eq!(decoded, receipt.manifest);
    for asset in &decoded.assets {
        let copied = fs::read(destination.join(&asset.path)).unwrap();
        assert_eq!(AssetId::of(&copied), asset.id);
        assert_eq!(copied.len() as u64, asset.bytes);
    }
    assert!(!staging_path(&destination).unwrap().exists());

    // Relocation: the bundle stays complete after the source project media
    // (and any cache) is gone — retained audio is bundle media (D4).
    fs::remove_dir_all(&source_root).unwrap();
    let still = Manifest::decode(&fs::read(destination.join(MANIFEST_NAME)).unwrap()).unwrap();
    assert_eq!(still.assets.len(), receipt.asset_count);
    for asset in &still.assets {
        let copied = fs::read(destination.join(&asset.path)).unwrap();
        assert_eq!(AssetId::of(&copied), asset.id);
    }
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn live_instrument_take_exports_its_recorded_audio_and_honest_identity() {
    let base = temporary("live-take");
    fs::create_dir_all(&base).unwrap();
    let cache = base.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let project = base.join("project");
    let samples: Vec<f32> = (0..4_800)
        .flat_map(|frame| {
            let value = (frame as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.3;
            [value, -value]
        })
        .collect();
    let audio = encode_fixture(48_000, 2, &samples);
    let asset = AssetId::of(&audio);
    let project_asset = project::AssetId::of(&audio);
    let path = cache.join("live.wav");
    fs::write(&path, &audio).unwrap();
    let plugin = Plugin {
        backend: Backend::Clap,
        id: "org.brownnote.fixture.instrument".into(),
        artifact: brownnote_native::pedals::offline::Hash256::of(b"fixture bundle"),
        abi: "clap-1.2".into(),
        version: "0.4.0".into(),
    };
    let stage = Stage {
        plugin: plugin.clone(),
        location: PathBuf::from("/machine/fixture.clap"),
        params: Vec::new(),
        state: Some(vec![9, 8, 7].into()),
    };
    let mut live = LiveInstrumentDocument::from_stage(&stage, "Fixture Instrument".into(), true);
    live.takes.push(LiveTakeDocument {
        take: "12".into(),
        asset: project_asset,
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
    document.validate().unwrap();
    Store::new(project.clone())
        .save(
            &document,
            &[MediaSource {
                id: project_asset,
                path: path.clone(),
            }],
        )
        .unwrap();
    fs::remove_dir_all(&cache).unwrap();

    let destination = base.join("bundle");
    let request = request(&project, &destination);
    let takes = Takes::new(LoopBars::try_from(4).unwrap(), TakeId(20));
    let edits = EditModel::default();
    let receipt = adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &request,
        &takes,
        &edits,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(receipt.event_count, 1);
    assert_eq!(receipt.manifest.parts.len(), 1);
    let manifest = Manifest::decode(&fs::read(destination.join(MANIFEST_NAME)).unwrap()).unwrap();
    let part = &manifest.parts[0];
    assert_eq!(
        part.kind,
        brownnote_native::export::manifest::PartKind::SampleClips { source: asset }
    );
    let event = manifest
        .events
        .iter()
        .find(|event| event.part == part.id)
        .unwrap();
    assert_eq!(event.asset, Some(asset));
    assert_eq!(event.duration_nanos, 50_000_000);
    // The retained take is copied byte-for-byte, not re-rendered.
    let bundled = fs::read(destination.join(&manifest.asset(asset).unwrap().path)).unwrap();
    assert_eq!(bundled, audio);
    let loss = manifest
        .losses
        .iter()
        .find(|loss| matches!(loss, LossRecord::BakedProcessing { asset: a, .. } if *a == asset))
        .unwrap();
    let LossRecord::BakedProcessing { detail, .. } = loss else {
        unreachable!()
    };
    assert!(detail.contains("org.brownnote.fixture.instrument"));
    assert!(detail.contains(&plugin.version));
    assert!(!detail.contains("/machine/fixture.clap"));
    assert!(detail.contains("not editable plugin state"));
    fs::remove_dir_all(base).unwrap();
}

/// The take was played at 60 BPM and the project now runs at 120 BPM, so the
/// project's tempo is twice the recording tempo and every wall-clock position
/// and duration below is halved by hand from the take fixture instead of being
/// read back from the exporter's own output.
const LIVE_RECORDING_MILLI_BPM: u32 = 60_000;
const LIVE_PROJECT_MILLI_BPM: u32 = 120_000;

#[test]
fn live_take_notes_export_with_the_plugin_identity_and_retained_audio() {
    let base = temporary("live-notes");
    fs::create_dir_all(&base).unwrap();
    let cache = base.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let project = base.join("project");
    let samples: Vec<f32> = (0..4_800)
        .flat_map(|frame| {
            let value = (frame as f32 / 48_000.0 * 220.0 * std::f32::consts::TAU).sin() * 0.25;
            [value, -value]
        })
        .collect();
    let audio = encode_fixture(48_000, 2, &samples);
    let asset = AssetId::of(&audio);
    let project_asset = project::AssetId::of(&audio);
    let path = cache.join("live-notes.wav");
    fs::write(&path, &audio).unwrap();
    let plugin = Plugin {
        backend: Backend::Clap,
        id: "org.brownnote.fixture.instrument".into(),
        artifact: brownnote_native::pedals::offline::Hash256::of(b"fixture notes bundle"),
        abi: "clap-1.2".into(),
        version: "0.4.0".into(),
    };
    let stage = Stage {
        plugin: plugin.clone(),
        location: PathBuf::from("/machine/fixture-notes.clap"),
        params: Vec::new(),
        state: Some(vec![7, 7].into()),
    };
    let mut live = LiveInstrumentDocument::from_stage(&stage, "Fixture Instrument".into(), true);
    live.takes.push(LiveTakeDocument {
        take: "12".into(),
        asset: project_asset,
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
    document.validate().unwrap();
    Store::new(project.clone())
        .save(
            &document,
            &[MediaSource {
                id: project_asset,
                path: path.clone(),
            }],
        )
        .unwrap();
    fs::remove_dir_all(&cache).unwrap();

    // The take the live instrument played into: two held notes, recorded at
    // 60 BPM into take 12.
    let mut takes = Takes::new(LoopBars::try_from(4).unwrap(), TakeId(12));
    takes
        .set_tempo(Tempo::try_from(LIVE_RECORDING_MILLI_BPM).unwrap())
        .unwrap();
    let take = takes.arm(PassOptions::default()).unwrap();
    assert_eq!(take, TakeId(12));
    takes.start(take).unwrap();
    let key = |channel, pitch| NoteKey {
        source: SourceId(31),
        channel: Channel::try_from(channel).unwrap(),
        pitch: Pitch::try_from(pitch).unwrap(),
    };
    let played = [
        (0, 60, 100, 200_000_000_u64, 300_000_000_u64),
        (1, 64, 90, 500_000_000, 800_000_000),
    ];
    for (channel, pitch, velocity, on, off) in played {
        takes
            .note_on(
                take,
                Target::Instrument(InstrumentId(1)),
                key(channel, pitch),
                Velocity::try_from(velocity).unwrap(),
                OriginalTime::from_nanos(on),
            )
            .unwrap();
        takes
            .note_off(take, key(channel, pitch), OriginalTime::from_nanos(off))
            .unwrap();
    }
    takes.finish(take).unwrap();
    // The project moved on to 120 BPM after the take was recorded.
    takes
        .set_tempo(Tempo::try_from(LIVE_PROJECT_MILLI_BPM).unwrap())
        .unwrap();

    let destination = base.join("bundle");
    let request = BundleRequest {
        source_root: project.clone(),
        destination: destination.clone(),
        origin: TimeOrigin { nanos: ORIGIN },
        timing: Timing {
            tempo_milli_bpm: LIVE_PROJECT_MILLI_BPM,
            sample_rate_hz: 48_000,
        },
    };
    let edits = EditModel::default();
    let receipt = adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &request,
        &takes,
        &edits,
        &BTreeMap::new(),
    )
    .unwrap();
    let bytes = fs::read(destination.join(MANIFEST_NAME)).unwrap();
    let manifest = Manifest::decode(&bytes).unwrap();
    assert_eq!(manifest, receipt.manifest);
    assert_eq!(manifest.timing.tempo_milli_bpm, LIVE_PROJECT_MILLI_BPM);
    // Two parts: the retained take audio, then that take's notes.
    assert_eq!(receipt.manifest.parts.len(), 2);
    assert_eq!(receipt.event_count, 3);

    // The retained audio part is unchanged: same kind, one synthetic clip
    // event, and the exact source WAV bytes.
    let audio_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::SampleClips { .. }))
        .unwrap();
    assert_eq!(audio_part.kind, PartKind::SampleClips { source: asset });
    let audio_events: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| event.part == audio_part.id)
        .collect();
    assert_eq!(audio_events.len(), 1);
    assert_eq!(
        audio_events[0].id,
        EventId {
            take: 12,
            sequence: u64::MAX
        }
    );
    assert_eq!(audio_events[0].asset, Some(asset));
    assert_eq!(audio_events[0].duration_nanos, 50_000_000);
    let bundled = fs::read(destination.join(&manifest.asset(asset).unwrap().path)).unwrap();
    assert_eq!(bundled, audio);

    // The notes part names the external plugin identity, never the sampled
    // instrument the note target happens to share.
    assert!(
        !manifest
            .parts
            .iter()
            .any(|part| matches!(part.kind, PartKind::InstrumentNotes { .. })),
        "live take notes must not become sampled-instrument notes"
    );
    let notes_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::LiveInstrumentNotes { .. }))
        .unwrap();
    let PartKind::LiveInstrumentNotes { plugin: identity } = &notes_part.kind else {
        unreachable!()
    };
    assert_eq!(identity.backend, "clap");
    assert_eq!(identity.id, plugin.id);
    assert_eq!(identity.artifact, plugin.artifact.to_string());
    assert_eq!(identity.abi, plugin.abi);
    assert_eq!(identity.version, plugin.version);

    // Counts, positions, durations and controls, halved from the fixture because
    // the take's 60 BPM recording tempo plays at the project's 120 BPM.
    let mut notes: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| event.part == notes_part.id)
        .map(|event| {
            (
                event.id,
                event.start_nanos,
                event.duration_nanos,
                event.pitch,
                event.channel,
                event.velocity,
                event.asset,
            )
        })
        .collect();
    notes.sort();
    let take_events = |sequence| EventId { take: 12, sequence };
    assert_eq!(
        notes,
        [
            (
                take_events(0),
                100_000_000,
                50_000_000,
                Some(60),
                Some(0),
                Some(100),
                None
            ),
            (
                take_events(1),
                250_000_000,
                150_000_000,
                Some(64),
                Some(1),
                Some(90),
                None
            ),
        ]
    );
    // The take's own recording tempo, not the project's, timed those notes.
    assert!(manifest.losses.iter().any(|loss| matches!(
        loss,
        LossRecord::ConversionLoss { event, detail }
            if *event == take_events(0)
                && detail.contains("original 200000000 ns at 60000 milli-BPM")
    )));

    // A destination with a plain SMF is not told these notes are the sampled
    // instrument: the manifest carries them, notes.mid stays note-free.
    let midi = manifest.midi.as_ref().unwrap();
    assert!(midi.tracks.is_empty());
    let (_, _, smf_notes) = read_smf(&fs::read(destination.join(&midi.path)).unwrap());
    assert!(smf_notes.is_empty());

    // Identity, never a machine path: the recall hint and the project root
    // stay out of the manifest entirely.
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains("/machine/fixture-notes.clap"));
    assert!(!text.contains(project.to_str().unwrap()));
    // The retained take is still the statement about the audio, now naming
    // where the notes went and what a target that cannot load the plugin gets.
    let loss = manifest
        .losses
        .iter()
        .find(|loss| matches!(loss, LossRecord::BakedProcessing { asset: a, .. } if *a == asset))
        .unwrap();
    let LossRecord::BakedProcessing { detail, .. } = loss else {
        unreachable!()
    };
    assert!(detail.contains(&plugin.id));
    assert!(detail.contains("their own part"));
    assert!(detail.contains("unsupported mapping"));
    assert!(detail.contains("not editable plugin state"));
    assert!(!detail.contains("/machine/fixture-notes.clap"));
    fs::remove_dir_all(base).unwrap();
}

/// #171's outcome for the other target kinds: a pad hit recorded during a
/// live-instrument pass is not inside the take's retained plugin capture (the
/// capture is the plugin's own blocks, the pad mixer is summed later), so it
/// must export as the pad clip it is instead of disappearing from the bundle.
#[test]
fn live_take_pad_hit_exports_as_a_pad_clip_beside_the_take_notes() {
    let base = temporary("live-pad-hits");
    fs::create_dir_all(&base).unwrap();
    let cache = base.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let project = base.join("project");
    let samples: Vec<f32> = (0..4_800)
        .flat_map(|frame| {
            let value = (frame as f32 / 48_000.0 * 220.0 * std::f32::consts::TAU).sin() * 0.25;
            [value, -value]
        })
        .collect();
    let audio = encode_fixture(48_000, 2, &samples);
    let asset = AssetId::of(&audio);
    let project_asset = project::AssetId::of(&audio);
    let pad_source = encode_fixture(48_000, 1, &vec![0.25; 24_000]);
    let pad_retained = encode_fixture(44_100, 1, &vec![0.5; 36_000]);
    let pad_source_id = project::AssetId::of(&pad_source);
    let pad_retained_id = project::AssetId::of(&pad_retained);
    let pad_source_asset = AssetId::of(&pad_source);
    let pad_retained_asset = AssetId::of(&pad_retained);
    let plugin = Plugin {
        backend: Backend::Clap,
        id: "org.brownnote.fixture.instrument".into(),
        artifact: brownnote_native::pedals::offline::Hash256::of(b"fixture pad hits bundle"),
        abi: "clap-1.2".into(),
        version: "0.4.0".into(),
    };
    let stage = Stage {
        plugin: plugin.clone(),
        location: PathBuf::from("/machine/fixture-pad-hits.clap"),
        params: Vec::new(),
        state: Some(vec![6, 5].into()),
    };
    let mut live = LiveInstrumentDocument::from_stage(&stage, "Fixture Instrument".into(), true);
    live.takes.push(LiveTakeDocument {
        take: "12".into(),
        asset: project_asset,
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
    document.validate().unwrap();
    let mut sources = Vec::new();
    for (id, name, bytes) in [
        (project_asset, "live-pad-hits.wav", &audio),
        (pad_source_id, "pad-source.wav", &pad_source),
        (pad_retained_id, "pad-retained.wav", &pad_retained),
    ] {
        let path = cache.join(name);
        fs::write(&path, bytes).unwrap();
        sources.push(MediaSource { id, path });
    }
    Store::new(project.clone())
        .save(&document, &sources)
        .unwrap();
    fs::remove_dir_all(&cache).unwrap();

    // One live pass at 60 BPM into take 12: a held instrument note and a pad
    // hit on pad 3, from the same pad bus the pass was played over.
    let mut takes = Takes::new(LoopBars::try_from(4).unwrap(), TakeId(12));
    takes
        .set_tempo(Tempo::try_from(LIVE_RECORDING_MILLI_BPM).unwrap())
        .unwrap();
    let take = takes.arm(PassOptions::default()).unwrap();
    takes.start(take).unwrap();
    let key = |channel, pitch| NoteKey {
        source: SourceId(41),
        channel: Channel::try_from(channel).unwrap(),
        pitch: Pitch::try_from(pitch).unwrap(),
    };
    let played = takes
        .note_on(
            take,
            Target::Instrument(InstrumentId(1)),
            key(0, 60),
            Velocity::try_from(100).unwrap(),
            OriginalTime::from_nanos(200_000_000),
        )
        .unwrap();
    takes
        .note_off(take, key(0, 60), OriginalTime::from_nanos(300_000_000))
        .unwrap();
    let hit = takes
        .note_on(
            take,
            Target::Pad(PadId::try_from(3).unwrap()),
            key(0, 36),
            Velocity::try_from(110).unwrap(),
            OriginalTime::from_nanos(900_000_000),
        )
        .unwrap();
    takes
        .note_off(take, key(0, 36), OriginalTime::from_nanos(1_100_000_000))
        .unwrap();
    takes.finish(take).unwrap();
    takes
        .set_tempo(Tempo::try_from(LIVE_PROJECT_MILLI_BPM).unwrap())
        .unwrap();
    // The notes' real identities: take 12, the sequences the pass recorded.
    let neutral = |id: performance::EventId| EventId {
        take: id.take.0,
        sequence: id.sequence,
    };
    assert_eq!((played.take.0, played.sequence), (12, 0));
    assert_eq!((hit.take.0, hit.sequence), (12, 1));
    let note_event = neutral(played);
    let pad_event_id = neutral(hit);

    let destination = base.join("bundle");
    let request = BundleRequest {
        source_root: project.clone(),
        destination: destination.clone(),
        origin: TimeOrigin { nanos: ORIGIN },
        timing: Timing {
            tempo_milli_bpm: LIVE_PROJECT_MILLI_BPM,
            sample_rate_hz: 48_000,
        },
    };
    let receipt = adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &request,
        &takes,
        &EditModel::default(),
        &BTreeMap::new(),
    )
    .unwrap();
    let manifest = Manifest::decode(&fs::read(destination.join(MANIFEST_NAME)).unwrap()).unwrap();
    assert_eq!(manifest, receipt.manifest);
    assert_eq!(manifest.validate(), Ok(()));
    // The take's audio clip, its held note and the pad hit: nothing dropped.
    assert_eq!(receipt.event_count, 3);
    assert_eq!(manifest.events.len(), 3);
    assert_eq!(receipt.manifest.parts.len(), 3);
    assert!(
        !manifest.losses.iter().any(|loss| matches!(
            loss,
            LossRecord::UnsupportedMapping { event, .. } if *event == pad_event_id
        )),
        "the pad hit is exported, not reported as an unsupported mapping: {:?}",
        manifest.losses
    );

    // The instrument note keeps its live-take part and the plugin identity.
    let notes_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::LiveInstrumentNotes { .. }))
        .unwrap();
    let PartKind::LiveInstrumentNotes { plugin: identity } = &notes_part.kind else {
        unreachable!()
    };
    assert_eq!(identity.backend, "clap");
    assert_eq!(identity.id, plugin.id);
    assert_eq!(identity.artifact, plugin.artifact.to_string());
    let note_events: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| event.part == notes_part.id)
        .map(|event| {
            (
                event.id,
                event.pad,
                event.asset,
                event.start_nanos,
                event.duration_nanos,
            )
        })
        .collect();
    // The held note stays sample-timed in the take's own part, with no pad and
    // no per-hit render.
    assert_eq!(
        note_events,
        [(note_event, None, None, 100_000_000, 50_000_000)]
    );

    // The pad hit is a normal pad-clip part for pad 3: the pad's retained
    // render is the media it plays from, the source stays as provenance, and
    // the per-hit audio is the exporter's usual baked pad render.
    let pad_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::PadClips { .. }))
        .unwrap();
    assert_eq!(pad_part.kind, PartKind::PadClips { pad: 3 });
    let pad_events: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| event.part == pad_part.id)
        .collect();
    assert_eq!(pad_events.len(), 1);
    let pad_event = pad_events[0];
    // Halved by hand from the fixture: the take recorded at 60 BPM plays at the
    // project's 120 BPM.
    assert_eq!(pad_event.id, pad_event_id);
    assert_eq!(pad_event.start_nanos, 450_000_000);
    assert_eq!(pad_event.duration_nanos, 100_000_000);
    assert_eq!(pad_event.velocity, Some(110));
    assert_eq!(pad_event.pitch, Some(36));
    assert_eq!(pad_event.channel, Some(0));
    assert_eq!(pad_event.source, Some(pad_source_asset));
    let hit_audio = manifest
        .audio
        .iter()
        .find(|entry| entry.event == Some(pad_event_id) && entry.role == AudioRole::HitWet)
        .expect("the pad hit is audible in the bundle");
    assert_eq!(pad_event.asset, Some(hit_audio.asset));
    assert!(hit_audio.frames > 0);
    assert!(manifest.losses.iter().any(|loss| matches!(
        loss,
        LossRecord::BakedProcessing { part, asset, detail }
            if *part == pad_part.id && *asset == hit_audio.asset && detail.contains("native one-shot")
    )));
    // The pad's retained render is bundle media, copied byte-for-byte.
    assert_eq!(
        manifest.asset(pad_retained_asset).unwrap().kind,
        AssetKind::RetainedRender
    );
    let copied =
        fs::read(destination.join(&manifest.asset(pad_retained_asset).unwrap().path)).unwrap();
    assert_eq!(copied, pad_retained);

    // The retained take audio is unchanged and byte-for-byte: the pad hit is
    // the pad mixer's signal, never part of the plugin's capture, so exporting
    // it adds no duplicated audio.
    let audio_part = manifest
        .parts
        .iter()
        .find(|part| matches!(part.kind, PartKind::SampleClips { .. }))
        .unwrap();
    assert_eq!(audio_part.kind, PartKind::SampleClips { source: asset });
    let audio_events: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| event.part == audio_part.id)
        .collect();
    assert_eq!(audio_events.len(), 1);
    assert_eq!(
        audio_events[0].id,
        EventId {
            take: 12,
            sequence: u64::MAX
        }
    );
    assert_eq!(audio_events[0].asset, Some(asset));
    assert_eq!(audio_events[0].duration_nanos, 50_000_000);
    let bundled = fs::read(destination.join(&manifest.asset(asset).unwrap().path)).unwrap();
    assert_eq!(bundled, audio);
    fs::remove_dir_all(base).unwrap();
}
