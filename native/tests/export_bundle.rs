#![allow(clippy::unwrap_used)]
//! Public-boundary tests for the export bundle manifest and transaction
//! (issue #109, wave 3 first PR: manifest + bundle transaction against
//! synthetic DTOs; MIDI/audio writers are the follow-up PR).

use brownnote_native::export::{
    bundle::{self, BundleError, BundleRequest, BundleWriter, SystemWriter, staging_path},
    fixture::{
        FixtureAsset, FixtureEdit, FixtureEvent, FixtureExport, FixtureMediaKind, FixturePad,
        FixturePart, FixturePartKind, FixtureTake, FixtureTarget,
    },
    manifest::{
        AssetId, EventId, LossRecord, MANIFEST_FORMAT_VERSION, MANIFEST_NAME, MEDIA_DIR, Manifest,
        ManifestError, PartId, TimeOrigin, Timing,
    },
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
    format!("brownnote-synthetic-wav:{label}").into_bytes()
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
    assert_eq!(receipt.asset_count, 3);

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
    let rebuilt = bundle::build_manifest(
        &request(Path::new(SOURCE_ROOT), &destination),
        &fixture.export,
    )
    .unwrap()
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

static NEXT: AtomicU64 = AtomicU64::new(0);

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
    assert_eq!(still.assets.len(), 3);
    for asset in &still.assets {
        let copied = fs::read(destination.join(&asset.path)).unwrap();
        assert_eq!(AssetId::of(&copied), asset.id);
    }
    fs::remove_dir_all(&base).unwrap();
}
