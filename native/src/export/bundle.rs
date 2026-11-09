//! Export bundle transaction: one time origin, deterministic traversal,
//! write-then-commit (issue #109).
//!
//! A bundle is a directory: `manifest.json` plus `media/<sha256>.wav` copies
//! (D3/D4). Everything is staged in a sibling directory and published with a
//! single atomic rename, so an injected write failure or a missing media file
//! can never present a partial bundle as complete. The writer is injectable,
//! so fault tests never touch disk. WAVs and SMF publish in that transaction.

use super::{
    audio::{self, AudioError, AudioSettings},
    fixture::{
        FixtureAsset, FixtureEvent, FixtureExport, FixtureMediaKind, FixturePad, FixturePart,
        FixturePartKind, FixtureTarget,
    },
    manifest::{
        AssetEntry, AssetId, AssetKind, EventEntry, EventId, LossRecord, MANIFEST_NAME, MEDIA_DIR,
        Manifest, ManifestError, PartEntry, PartKind, TimeOrigin, Timing,
    },
    midi::{self, MidiError},
};
use std::{
    collections::BTreeMap,
    fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
};

/// Filesystem operations the transaction needs. Faults can surround every
/// boundary; any mutating call may return an error after taking effect.
pub trait BundleWriter {
    fn exists(&mut self, path: &Path) -> io::Result<bool>;
    fn ensure_dir(&mut self, path: &Path) -> io::Result<()>;
    /// Create, write, flush and sync one file.
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn sync_dir(&mut self, path: &Path) -> io::Result<()>;
    fn read(&mut self, path: &Path) -> io::Result<Vec<u8>>;
    /// Atomic publication of the staged bundle onto its destination.
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_dir_all(&mut self, path: &Path) -> io::Result<()>;
}

/// Production writer over the real filesystem.
#[derive(Default)]
pub struct SystemWriter;

impl BundleWriter for SystemWriter {
    fn exists(&mut self, path: &Path) -> io::Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
    fn ensure_dir(&mut self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = fs::File::create(path)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()
    }
    fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        fs::File::open(path)?.sync_all()
    }
    fn read(&mut self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
    fn remove_dir_all(&mut self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }
}

/// A bundle transaction request: where source media is read from, where the
/// bundle goes, and the single time origin plus timing every part aligns to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleRequest {
    /// Source project directory; media is read from `<root>/media/<hash>.wav`.
    pub source_root: PathBuf,
    /// Bundle directory to create. Must not exist yet.
    pub destination: PathBuf,
    /// The bundle's one time origin; event times are stored relative to it.
    pub origin: TimeOrigin,
    pub timing: Timing,
}

/// What a successful transaction published.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleReceipt {
    pub destination: PathBuf,
    pub manifest: Manifest,
    /// Content hash of the encoded manifest bytes.
    pub manifest_id: AssetId,
    pub event_count: usize,
    pub asset_count: usize,
}

#[derive(Debug)]
pub enum BundleError {
    Audio(AudioError),
    Midi(MidiError),
    TimingOverflow(EventId),
    TargetPartMismatch(EventId),
    Manifest(ManifestError),
    Io {
        context: &'static str,
        source: io::Error,
    },
    /// The destination already holds a bundle; it is never overwritten.
    DestinationExists,
    /// Retained media is project media (D4); a vanished source is an error,
    /// never a silent regeneration or cache fallback.
    MissingMedia(AssetId),
    MediaSizeMismatch {
        id: AssetId,
        expected: u64,
        actual: u64,
    },
    MediaHashMismatch {
        expected: AssetId,
        actual: AssetId,
    },
    EventBeforeOrigin {
        event: EventId,
        label: &'static str,
        origin: u64,
        time: u64,
    },
    UnknownEvent(EventId),
    UnknownPad(u8),
}

impl BundleError {
    fn io(context: &'static str, source: io::Error) -> Self {
        Self::Io { context, source }
    }
}

impl From<ManifestError> for BundleError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}
impl From<AudioError> for BundleError {
    fn from(error: AudioError) -> Self {
        Self::Audio(error)
    }
}
impl From<MidiError> for BundleError {
    fn from(error: MidiError) -> Self {
        Self::Midi(error)
    }
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "export bundle: {self:?}")
    }
}
impl std::error::Error for BundleError {}

/// Sibling staging directory used while a bundle is written. It lives next to
/// the destination so the commit rename stays on one filesystem.
pub fn staging_path(destination: &Path) -> Result<PathBuf, BundleError> {
    let invalid = || BundleError::io("destination", io::Error::from(io::ErrorKind::InvalidInput));
    let parent = destination.parent().ok_or_else(invalid)?.to_path_buf();
    let name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(invalid)?;
    Ok(parent.join(format!(".{name}.staging")))
}

fn effective_start_nanos(event: &FixtureEvent) -> u64 {
    event
        .edit
        .as_ref()
        .map_or(event.start_nanos, |edit| edit.effective_start_nanos)
}

/// Resolve an event's pad and retained asset (D4: `retained` over `source`,
/// an explicit per-hit variant render over the pad default).
fn event_target(
    event: &FixtureEvent,
    pads: &BTreeMap<u8, &FixturePad>,
) -> Result<(Option<u8>, Option<AssetId>), BundleError> {
    if let Some(asset) = event.asset {
        let pad = match event.target {
            FixtureTarget::Pad(pad) => Some(pad),
            _ => None,
        };
        return Ok((pad, Some(asset)));
    }
    match event.target {
        FixtureTarget::Pad(pad) => {
            let definition = pads.get(&pad).ok_or(BundleError::UnknownPad(pad))?;
            Ok((Some(pad), definition.retained.or(definition.source)))
        }
        FixtureTarget::Sample(id) => Ok((None, Some(id))),
        FixtureTarget::Instrument(_) => Ok((None, None)),
    }
}

fn part_kind(kind: &FixturePartKind) -> PartKind {
    match kind {
        FixturePartKind::PadClips { pad } => PartKind::PadClips { pad: *pad },
        FixturePartKind::InstrumentNotes { instrument } => PartKind::InstrumentNotes {
            instrument: *instrument,
        },
        FixturePartKind::SampleClips { source } => PartKind::SampleClips { source: *source },
    }
}

fn asset_kind(kind: FixtureMediaKind) -> AssetKind {
    match kind {
        FixtureMediaKind::Source => AssetKind::Source,
        FixtureMediaKind::RetainedRender => AssetKind::RetainedRender,
    }
}

/// Deterministic traversal of a synthetic export into a manifest: parts by
/// `PartId`, events within a part by (effective onset, event ID), assets by
/// content hash. Same-pad collisions are never collapsed (unlike the browser
/// `exportLoop`); each hit keeps its own event, and the collision is recorded
/// as an unsupported-mapping loss instead.
pub fn build_manifest(
    request: &BundleRequest,
    export: &FixtureExport,
) -> Result<Manifest, BundleError> {
    let mut events_by_id: BTreeMap<EventId, &FixtureEvent> = BTreeMap::new();
    for take in &export.takes {
        for event in &take.events {
            if events_by_id.insert(event.id, event).is_some() {
                return Err(ManifestError::DuplicateEvent(event.id).into());
            }
        }
    }
    let pads: BTreeMap<u8, &FixturePad> = export.pads.iter().map(|pad| (pad.id, pad)).collect();
    let mut parts: Vec<&FixturePart> = export.parts.iter().collect();
    parts.sort_by_key(|part| part.id);

    let mut manifest = Manifest::new(request.origin, request.timing);
    for part in &parts {
        manifest.parts.push(PartEntry {
            id: part.id,
            kind: part_kind(&part.kind),
        });
    }

    let mut losses: Vec<LossRecord> = Vec::new();
    // pad -> (effective start, end, event) hits on choking pads
    let mut choke_hits: BTreeMap<u8, Vec<(u64, u64, EventId)>> = BTreeMap::new();
    for part in &parts {
        let mut events: Vec<&FixtureEvent> = Vec::new();
        for id in &part.event_ids {
            events.push(events_by_id.get(id).ok_or(BundleError::UnknownEvent(*id))?);
        }
        events.sort_by(|a, b| {
            effective_start_nanos(a)
                .cmp(&effective_start_nanos(b))
                .then_with(|| a.id.cmp(&b.id))
        });
        for event in events {
            let matches_part = match (&part.kind, event.target) {
                (FixturePartKind::PadClips { pad }, FixtureTarget::Pad(target)) => *pad == target,
                (FixturePartKind::SampleClips { source }, FixtureTarget::Sample(target)) => {
                    *source == target
                }
                (
                    FixturePartKind::InstrumentNotes { instrument },
                    FixtureTarget::Instrument(target),
                ) => *instrument == target,
                _ => false,
            };
            if !matches_part {
                return Err(BundleError::TargetPartMismatch(event.id));
            }
            for (label, time) in [
                ("start", event.start_nanos),
                ("effective start", effective_start_nanos(event)),
            ] {
                if time < request.origin.nanos {
                    return Err(BundleError::EventBeforeOrigin {
                        event: event.id,
                        label,
                        origin: request.origin.nanos,
                        time,
                    });
                }
            }
            let (pad, asset) = event_target(event, &pads)?;
            let effective = effective_start_nanos(event);
            if let Some(edit) = &event.edit {
                losses.push(LossRecord::ConversionLoss {
                    event: event.id,
                    detail: format!(
                        "edit moved onset from {} to {} ns ({})",
                        event.start_nanos, edit.effective_start_nanos, edit.detail
                    ),
                });
            }
            if let (Some(pad), Some(asset)) = (pad, asset)
                && let Some(definition) = pads.get(&pad)
            {
                if !definition.baked_chain.is_empty() {
                    losses.push(LossRecord::BakedProcessing {
                        part: part.id,
                        asset,
                        detail: format!(
                            "processing baked into retained render: {}",
                            definition.baked_chain.join(", ")
                        ),
                    });
                }
                if definition.choke {
                    choke_hits.entry(pad).or_default().push((
                        effective,
                        effective
                            .checked_add(event.duration_nanos)
                            .ok_or(BundleError::TimingOverflow(event.id))?,
                        event.id,
                    ));
                }
            }
            manifest.events.push(EventEntry {
                id: event.id,
                part: part.id,
                start_nanos: effective - request.origin.nanos,
                duration_nanos: event.duration_nanos,
                pad,
                pitch: event.pitch,
                channel: event.channel,
                velocity: event.velocity,
                asset,
                source: match event.target {
                    FixtureTarget::Pad(pad) => pads.get(&pad).and_then(|pad| pad.source),
                    FixtureTarget::Sample(id) => Some(id),
                    FixtureTarget::Instrument(_) => None,
                },
                variant_of: event.variant_of,
            });
        }
    }
    // Same-pad collisions under choke: kept as separate hits, recorded as
    // unsupported mappings (never collapsed into one event).
    for (pad, mut hits) in choke_hits {
        hits.sort();
        let mut open_end: Option<u64> = None;
        for (start, end, event) in hits {
            if open_end.is_some_and(|open| start < open) {
                losses.push(LossRecord::UnsupportedMapping {
                    event,
                    reason: format!(
                        "pad {pad} choke collision kept as a separate hit, not collapsed"
                    ),
                });
            }
            open_end = Some(open_end.map_or(end, |open| open.max(end)));
        }
    }

    let mut assets: Vec<&FixtureAsset> = export.assets.iter().collect();
    assets.sort_by_key(|asset| asset.id);
    for asset in assets {
        manifest.assets.push(AssetEntry {
            id: asset.id,
            kind: asset_kind(asset.kind),
            bytes: asset.bytes,
            path: asset.id.relative_path(),
        });
    }
    manifest.losses = losses;
    Ok(manifest)
}

/// Run one bundle transaction: build and validate the manifest, stage every
/// media file (verified by byte count and SHA-256) plus the manifest in a
/// sibling staging directory, then publish with one atomic rename. Any
/// failure before the commit removes the staging directory, so a partial
/// bundle is never observable at the destination.
pub fn write_bundle<W: BundleWriter + ?Sized>(
    writer: &mut W,
    request: &BundleRequest,
    export: &FixtureExport,
) -> Result<BundleReceipt, BundleError> {
    write_bundle_with_settings(writer, request, export, &AudioSettings::default())
}

pub fn write_bundle_with_settings<W: BundleWriter + ?Sized>(
    writer: &mut W,
    request: &BundleRequest,
    export: &FixtureExport,
    settings: &AudioSettings,
) -> Result<BundleReceipt, BundleError> {
    let mut manifest = build_manifest(request, export)?;
    manifest.validate()?;
    if writer
        .exists(&request.destination)
        .map_err(|e| BundleError::io("destination", e))?
    {
        return Err(BundleError::DestinationExists);
    }
    let mut media = BTreeMap::new();
    for asset in &manifest.assets {
        let id = asset.id;
        let source = request.source_root.join(id.relative_path());
        let bytes = match writer.read(&source) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(BundleError::MissingMedia(id));
            }
            Err(e) => return Err(BundleError::io("read media", e)),
        };
        if bytes.len() as u64 != asset.bytes {
            return Err(BundleError::MediaSizeMismatch {
                id,
                expected: asset.bytes,
                actual: bytes.len() as u64,
            });
        }
        let actual = AssetId::of(&bytes);
        if actual != id {
            return Err(BundleError::MediaHashMismatch {
                expected: id,
                actual,
            });
        }
        media.insert(id, bytes);
    }
    let generated = audio::render(&mut manifest, export, &media, settings)?;
    media.extend(generated);
    let midi = midi::encode(&manifest)?;
    manifest.midi = Some(midi.entry);
    manifest.losses.extend(midi.losses);
    let manifest_bytes = manifest.encode()?;
    let manifest_id = AssetId::of(&manifest_bytes);
    let event_count = manifest.events.len();
    let asset_count = manifest.assets.len();
    let parent = request
        .destination
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            BundleError::io("destination", io::Error::from(io::ErrorKind::InvalidInput))
        })?;
    let staging = staging_path(&request.destination)?;
    writer
        .ensure_dir(&parent)
        .map_err(|e| BundleError::io("ensure destination parent", e))?;
    if writer
        .exists(&staging)
        .map_err(|e| BundleError::io("staging", e))?
    {
        // Leftover from an aborted run of this same destination.
        writer
            .remove_dir_all(&staging)
            .map_err(|e| BundleError::io("clean stale staging", e))?;
    }

    match stage_and_commit(
        writer,
        request,
        &staging,
        &manifest,
        &manifest_bytes,
        &media,
        &midi.bytes,
    ) {
        Ok(()) => Ok(BundleReceipt {
            destination: request.destination.clone(),
            manifest,
            manifest_id,
            event_count,
            asset_count,
        }),
        Err(error) => {
            // Abort: the destination only ever appears via the commit rename,
            // so removing staging guarantees no partial bundle is presented.
            let _ = writer.remove_dir_all(&staging);
            Err(error)
        }
    }
}

fn stage_and_commit<W: BundleWriter + ?Sized>(
    writer: &mut W,
    request: &BundleRequest,
    staging: &Path,
    manifest: &Manifest,
    manifest_bytes: &[u8],
    media: &BTreeMap<AssetId, Vec<u8>>,
    midi_bytes: &[u8],
) -> Result<(), BundleError> {
    writer
        .ensure_dir(staging)
        .map_err(|e| BundleError::io("ensure staging", e))?;
    writer
        .ensure_dir(&staging.join(MEDIA_DIR))
        .map_err(|e| BundleError::io("ensure media", e))?;
    // Original bytes were verified before rendering; generated assets share
    // their hash paths and publish in the same transaction, not a sidecar.
    for asset in &manifest.assets {
        let bytes = media
            .get(&asset.id)
            .ok_or(BundleError::MissingMedia(asset.id))?;
        writer
            .write_file(&staging.join(&asset.path), bytes)
            .map_err(|e| BundleError::io("write media", e))?;
    }
    writer
        .write_file(&staging.join(midi::MIDI_PATH), midi_bytes)
        .map_err(|e| BundleError::io("write MIDI", e))?;
    // The manifest is staged last; it becomes visible only together with the
    // whole staging directory at the commit rename.
    writer
        .write_file(&staging.join(MANIFEST_NAME), manifest_bytes)
        .map_err(|e| BundleError::io("write manifest", e))?;
    writer
        .sync_dir(&staging.join(MEDIA_DIR))
        .map_err(|e| BundleError::io("sync media", e))?;
    writer
        .sync_dir(staging)
        .map_err(|e| BundleError::io("sync staging", e))?;
    // The only commit point. An error does not imply the rename did not run.
    if let Err(e) = writer.rename(staging, &request.destination) {
        // An error does not imply the rename did not run; if the destination
        // appeared, the bundle was already fully staged and synced.
        let published = writer.exists(&request.destination).unwrap_or(true);
        if !published {
            return Err(BundleError::io("commit", e));
        }
    }
    writer
        .sync_dir(request.destination.parent().ok_or_else(|| {
            BundleError::io("destination", io::Error::from(io::ErrorKind::InvalidInput))
        })?)
        .map_err(|e| BundleError::io("sync destination parent", e))?;
    Ok(())
}
