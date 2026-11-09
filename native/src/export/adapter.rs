//! The only export module coupled to project/takes/edit owners.
//!
//! A caller supplies the current in-memory takes/edit layer: the project store
//! does not yet persist those registries. We never pretend the kit document
//! contains a saved performance. Take clocks overlay at `request.origin`;
//! musical positions convert at the current project tempo (D10).
//! Future instrument/plugin owners supply project-owned per-event frozen
//! renders explicitly, because today's public note model has no audio field.

use super::{
    audio::{AudioSettings, FrozenHit, PadSettings},
    bundle::{self, BundleError, BundleReceipt, BundleRequest, BundleWriter},
    fixture::{
        FixtureAsset, FixtureEdit, FixtureEvent, FixtureExport, FixtureMediaKind, FixturePad,
        FixturePart, FixturePartKind, FixtureTake, FixtureTarget,
    },
    manifest::{AssetId, EventId, PartId},
};
use crate::{
    edit::{EditError, EditModel},
    performance::{self, MusicalPosition, PerformanceError, Target},
    project::{
        self, Document, DocumentError, MediaKind,
        store::{Store, StoreError},
    },
    takes::Takes,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Debug)]
pub struct ProjectFrozenHit {
    pub wet: project::AssetId,
    pub dry: Option<project::AssetId>,
    pub detail: String,
}

#[derive(Debug)]
pub enum AdapterError {
    Document(DocumentError),
    Store(StoreError),
    NoProject,
    Edit(EditError),
    Performance(PerformanceError),
    Bundle(BundleError),
    TempoMismatch,
    Overflow,
}
impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "project export: {self:?}")
    }
}
impl std::error::Error for AdapterError {}

fn asset_id(id: project::AssetId) -> Result<AssetId, AdapterError> {
    id.to_string().parse().map_err(|_| AdapterError::Overflow)
}
fn event_id(id: performance::EventId) -> EventId {
    EventId {
        take: id.take.0,
        sequence: id.sequence,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PartKey {
    Pad(u8),
    Sample(AssetId),
    Instrument(u128),
}

impl PartKey {
    fn id(self) -> PartId {
        let mut hash = Sha256::new();
        hash.update(b"brownnote-export-part-v2:");
        match self {
            Self::Pad(pad) => {
                hash.update(b"pad:");
                hash.update([pad]);
            }
            Self::Sample(asset) => {
                hash.update(b"sample:");
                hash.update(asset.to_string().as_bytes());
            }
            Self::Instrument(instrument) => {
                hash.update(b"instrument:");
                hash.update(instrument.to_be_bytes());
            }
        }
        let digest = hash.finalize();
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&digest[..8]);
        PartId(u64::from_be_bytes(bytes))
    }
}

pub struct ProjectExport {
    pub events: FixtureExport,
    pub audio: AudioSettings,
}

/// Snapshot the real effective view, not a second implementation of edits or
/// quantization. Original event IDs (including full 128-bit take IDs) survive.
pub fn prepare(
    request: &BundleRequest,
    document: &Document,
    takes: &Takes,
    edits: &EditModel,
    frozen: &BTreeMap<performance::EventId, ProjectFrozenHit>,
) -> Result<ProjectExport, AdapterError> {
    document.validate().map_err(AdapterError::Document)?;
    if request.timing.tempo_milli_bpm != takes.tempo().milli_bpm() {
        return Err(AdapterError::TempoMismatch);
    }
    let mut audio = AudioSettings {
        master_gain: document.master,
        ..AudioSettings::default()
    };
    let mut pads = Vec::new();
    for (index, pad) in document.pads.iter().enumerate() {
        let Some(sample) = &pad.sample else { continue };
        pads.push(FixturePad {
            id: index as u8,
            source: Some(asset_id(sample.source)?),
            retained: sample.retained.map(asset_id).transpose()?,
            baked_chain: pad.chain.stages.iter().map(|s| s.effect.clone()).collect(),
            choke: document.choke,
        });
        audio.pads.insert(
            index as u8,
            PadSettings {
                gain: pad.gain,
                pan: pad.pan,
                semitones: pad.semitones,
            },
        );
    }
    let assets = document
        .assets
        .iter()
        .map(|asset| {
            Ok(FixtureAsset {
                id: asset_id(asset.reference.id)?,
                bytes: asset.reference.bytes,
                kind: match asset.kind {
                    MediaKind::Source => FixtureMediaKind::Source,
                    MediaKind::RetainedRender => FixtureMediaKind::RetainedRender,
                },
            })
        })
        .collect::<Result<_, AdapterError>>()?;
    let effective = edits.effective_notes(takes).map_err(AdapterError::Edit)?;
    let mut grouped: BTreeMap<PartKey, Vec<EventId>> = BTreeMap::new();
    let mut exported_takes: BTreeMap<u128, FixtureTake> = BTreeMap::new();
    for note in effective {
        let id = event_id(note.id);
        let (target, part) = match note.target {
            Target::Pad(pad) => (FixtureTarget::Pad(pad.into()), PartKey::Pad(pad.into())),
            Target::Sample(asset) => {
                let asset = asset_id(asset)?;
                (FixtureTarget::Sample(asset), PartKey::Sample(asset))
            }
            Target::Instrument(instrument) => (
                FixtureTarget::Instrument(instrument.0),
                PartKey::Instrument(instrument.0),
            ),
        };
        let take = takes.take(note.id.take).ok_or(AdapterError::Overflow)?;
        let original = take
            .notes()
            .iter()
            .find(|n| n.on().id == note.id)
            .ok_or(AdapterError::Overflow)?;
        let to_nanos = |position: MusicalPosition| {
            position
                .at_tempo_nanos(takes.tempo())
                .map_err(AdapterError::Performance)
        };
        let start = to_nanos(note.start)?;
        let end = to_nanos(note.end)?;
        let original_start = to_nanos(MusicalPosition::from_original(
            original.on().original_time,
            take.timing().recording_tempo(),
        ))?;
        let absolute = |time: u64| {
            request
                .origin
                .nanos
                .checked_add(time)
                .ok_or(AdapterError::Overflow)
        };
        let edit = edits.event_edit(note.id);
        let event = FixtureEvent {
            id,
            target,
            start_nanos: absolute(original_start)?,
            duration_nanos: end.checked_sub(start).ok_or(AdapterError::Overflow)?,
            velocity: Some(note.velocity.value()),
            pitch: Some(note.key.pitch.value()),
            channel: Some(note.key.channel.value()),
            asset: None,
            variant_of: None,
            edit: Some(FixtureEdit {
                effective_start_nanos: absolute(start)?,
                detail: format!(
                    "effective view: original {} ns at {} milli-BPM; quantization {:?}, \
                     nudge {} ns, velocity {} -> {}",
                    original.on().original_time.nanos(),
                    take.timing().recording_tempo().milli_bpm(),
                    edits.quantization(note.id.take),
                    edit.nudge_nanos,
                    original.on().velocity.value(),
                    note.velocity.value(),
                ),
            }),
        };
        if let Some(render) = frozen.get(&note.id) {
            audio.frozen.insert(
                id,
                FrozenHit {
                    wet: asset_id(render.wet)?,
                    dry: render.dry.map(asset_id).transpose()?,
                    detail: render.detail.clone(),
                },
            );
        }
        grouped.entry(part).or_default().push(id);
        exported_takes
            .entry(id.take)
            .or_insert_with(|| FixtureTake {
                id: id.take,
                recording_tempo_milli_bpm: take.timing().recording_tempo().milli_bpm(),
                events: Vec::new(),
            })
            .events
            .push(event);
    }
    let parts = grouped
        .into_iter()
        .map(|(key, event_ids)| FixturePart {
            id: key.id(),
            event_ids,
            kind: match key {
                PartKey::Pad(pad) => FixturePartKind::PadClips { pad },
                PartKey::Sample(source) => FixturePartKind::SampleClips { source },
                PartKey::Instrument(instrument) => FixturePartKind::InstrumentNotes { instrument },
            },
        })
        .collect();
    Ok(ProjectExport {
        events: FixtureExport {
            takes: exported_takes.into_values().collect(),
            pads,
            assets,
            parts,
        },
        audio,
    })
}

/// Export the current kit snapshot and current in-memory performance.
pub fn write_project_bundle<W: BundleWriter + ?Sized>(
    writer: &mut W,
    request: &BundleRequest,
    document: &Document,
    takes: &Takes,
    edits: &EditModel,
    frozen: &BTreeMap<performance::EventId, ProjectFrozenHit>,
) -> Result<BundleReceipt, AdapterError> {
    let prepared = prepare(request, document, takes, edits, frozen)?;
    bundle::write_bundle_with_settings(writer, request, &prepared.events, &prepared.audio)
        .map_err(AdapterError::Bundle)
}

/// Read the real committed project/media closure through Store, then combine
/// it with the owner's current takes/edit layer. No source/cache/plugin lookup.
pub fn write_saved_project_bundle<W: BundleWriter + ?Sized>(
    writer: &mut W,
    request: &BundleRequest,
    takes: &Takes,
    edits: &EditModel,
    frozen: &BTreeMap<performance::EventId, ProjectFrozenHit>,
) -> Result<BundleReceipt, AdapterError> {
    let inspection = Store::new(request.source_root.clone())
        .inspect_committed()
        .map_err(AdapterError::Store)?
        .ok_or(AdapterError::NoProject)?;
    write_project_bundle(writer, request, &inspection.document, takes, edits, frozen)
}
