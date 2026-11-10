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
        FixtureAsset, FixtureEdit, FixtureEvent, FixtureExport, FixtureLoss, FixtureMediaKind,
        FixturePad, FixturePart, FixturePartKind, FixturePluginIdentity, FixtureTake,
        FixtureTarget,
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
    /// A live-take note part can only be built from a project that still holds
    /// the live instrument's identity.
    LiveIdentity,
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
    /// One take's retained live-instrument audio.
    LiveTake(u128),
    /// One live take's editable notes, joined to that audio by take identity.
    LiveTakeNotes(u128),
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
            Self::LiveTake(take) => {
                hash.update(b"live-take:");
                hash.update(take.to_be_bytes());
            }
            Self::LiveTakeNotes(take) => {
                hash.update(b"live-take-notes:");
                hash.update(take.to_be_bytes());
            }
        }
        let digest = hash.finalize();
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&digest[..8]);
        PartId(u64::from_be_bytes(bytes))
    }
}

/// What is baked into a pad's retained render, named honestly: GuitarPedal
/// effects by name, plugin stages by exact identity. Plugin state and
/// parameters are a record of how the sound was made, not editable effects.
fn baked_chain(pad: &project::Pad) -> Vec<String> {
    let mut chain: Vec<String> = pad.chain.stages.iter().map(|s| s.effect.clone()).collect();
    if let Some(plugins) = &pad.plugins {
        chain.extend(plugins.stages.iter().map(|stage| stage.describe()));
        chain.push(format!(
            "offline {:?} render downmixed to the mono pad voice (render identity {})",
            plugins.policy.layout, plugins.render
        ));
    }
    chain
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
            baked_chain: baked_chain(pad),
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
    // The instrument's SF2 source is project data, not audio the bundle plays.
    let assets = document
        .assets
        .iter()
        .filter_map(|asset| {
            let kind = match asset.kind {
                MediaKind::Source => FixtureMediaKind::Source,
                MediaKind::RetainedRender
                | MediaKind::InstrumentRender
                | MediaKind::LiveTakeRender => FixtureMediaKind::RetainedRender,
                MediaKind::InstrumentSource => return None,
            };
            Some(asset_id(asset.reference.id).map(|id| FixtureAsset {
                id,
                bytes: asset.reference.bytes,
                kind,
            }))
        })
        .collect::<Result<_, AdapterError>>()?;
    // The project's own retained instrument renders join the caller's.
    let mut frozen = frozen.clone();
    if let Some(instrument) = &document.instrument {
        let (hits, _) = instrument
            .frozen_hits(takes, edits)
            .map_err(AdapterError::Edit)?;
        for (id, hit) in hits {
            frozen.entry(id).or_insert(hit);
        }
    }
    let effective = edits.effective_notes(takes).map_err(AdapterError::Edit)?;
    // A live take's identity travels with its notes, so a destination can name
    // the plugin it cannot load instead of inventing a sampled instrument.
    let live_plugin = document
        .live_instrument
        .as_ref()
        .map(|live| FixturePluginIdentity {
            backend: live.plugin.backend.as_str().into(),
            id: live.plugin.id.clone(),
            artifact: live.plugin.artifact.to_string(),
            abi: live.plugin.abi.clone(),
            version: live.plugin.version.clone(),
        });
    let mut grouped: BTreeMap<PartKey, Vec<EventId>> = BTreeMap::new();
    let mut exported_takes: BTreeMap<u128, FixtureTake> = BTreeMap::new();
    for note in effective {
        // Only a live take's instrument notes are regrouped: they carry the
        // plugin identity instead of the sampled instrument's. A pad or sample
        // hit in that take keeps its normal part and asset, because the
        // retained take audio carries the plugin's own output and not the pad
        // mixer those hits were summed from.
        let live_instrument_note = matches!(note.target, Target::Instrument(_))
            && document
                .live_instrument
                .as_ref()
                .is_some_and(|live| live.take(note.id.take).is_some());
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
        let part = if live_instrument_note {
            PartKey::LiveTakeNotes(id.take)
        } else {
            part
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
        // A live instrument note never joins a frozen render: the take's
        // retained audio is that note's sound, and a sampled-instrument render
        // of the same target would fabricate a performance it never played.
        if !live_instrument_note && let Some(render) = frozen.get(&note.id) {
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
    // Retained live-instrument takes are the authoritative audio (D4). They
    // export as one clip part each, with the exact plugin identity recorded as
    // baked processing: the state recalls the plugin here, it is not editable
    // effect state another host can reconstruct. The take's notes travel in a
    // separate part naming that same identity, so the performance stays
    // editable where the destination can load the plugin.
    let mut losses = Vec::new();
    let mut live_parts: BTreeMap<PartId, AssetId> = BTreeMap::new();
    if let Some(live) = &document.live_instrument {
        for take in &live.takes {
            let Some(take_id) = take.id() else { continue };
            // Replay never schedules a muted or removed/replaced take, so
            // export must not bake one either: its retained audio would sound
            // in the reference mix and double a performance the app replaced.
            // A take with no registry record (document-only) keeps exporting.
            if takes
                .take(take_id)
                .is_some_and(|saved| saved.muted() || saved.removal().is_some())
            {
                continue;
            }
            let asset = asset_id(take.asset)?;
            let key = PartKey::LiveTake(take_id.0);
            let part = key.id();
            let duration_nanos =
                u128::from(take.frames) * 1_000_000_000 / u128::from(take.rate.max(1));
            let duration_nanos = u64::try_from(duration_nanos).unwrap_or(u64::MAX).max(1);
            let id = EventId {
                take: take_id.0,
                // Reserved: the take's audio clip never collides with one of
                // its note sequences.
                sequence: u64::MAX,
            };
            let tempo = takes
                .take(take_id)
                .map_or(takes.tempo().milli_bpm(), |saved| {
                    saved.timing().recording_tempo().milli_bpm()
                });
            exported_takes
                .entry(take_id.0)
                .or_insert_with(|| FixtureTake {
                    id: take_id.0,
                    recording_tempo_milli_bpm: tempo,
                    events: Vec::new(),
                })
                .events
                .push(FixtureEvent {
                    id,
                    target: FixtureTarget::Sample(asset),
                    start_nanos: request.origin.nanos,
                    duration_nanos,
                    velocity: None,
                    pitch: None,
                    channel: None,
                    asset: Some(asset),
                    variant_of: None,
                    edit: None,
                });
            grouped.entry(key).or_default().push(id);
            live_parts.insert(part, asset);
            losses.push(FixtureLoss {
                part,
                asset,
                detail: format!(
                    "live instrument take audio baked from {}; this take's notes are exported in \
                     their own part, sample-timed and pinned to that identity, and a destination \
                     that cannot load the plugin reports them as an unsupported mapping with this \
                     retained audio as the fallback; the project's opaque state is a recall record \
                     for this plugin, not editable plugin state another host can reconstruct",
                    live.describe()
                ),
            });
        }
    }
    let parts = grouped
        .into_iter()
        .map(|(key, event_ids)| {
            let id = key.id();
            Ok(FixturePart {
                id,
                event_ids,
                kind: match key {
                    PartKey::Pad(pad) => FixturePartKind::PadClips { pad },
                    PartKey::Sample(source) => FixturePartKind::SampleClips { source },
                    PartKey::Instrument(instrument) => {
                        FixturePartKind::InstrumentNotes { instrument }
                    }
                    PartKey::LiveTake(_) => FixturePartKind::SampleClips {
                        source: *live_parts.get(&id).ok_or(AdapterError::Overflow)?,
                    },
                    PartKey::LiveTakeNotes(take) => FixturePartKind::LiveInstrumentNotes {
                        take,
                        plugin: live_plugin.clone().ok_or(AdapterError::LiveIdentity)?,
                    },
                },
            })
        })
        .collect::<Result<Vec<_>, AdapterError>>()?;
    Ok(ProjectExport {
        events: FixtureExport {
            takes: exported_takes.into_values().collect(),
            pads,
            assets,
            parts,
            losses,
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
