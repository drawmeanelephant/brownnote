//! Neutral writer DTOs, also used by synthetic export fixtures.
//!
//! The public names from the first manifest PR are retained. `adapter` now
//! supplies these from real project/takes/edit inputs, without making the
//! writers depend on those owners. SoundCraft does not shape them (D19).

use super::manifest::{AssetId, EventId, PartId};

/// Mirror of #98 `MediaKind`: source bytes vs an authoritative retained render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureMediaKind {
    Source,
    RetainedRender,
}

/// Mirror of #98 `Asset`: a content-hash reference plus its kind. The bytes
/// live at `<source root>/media/<hash>.wav`, exactly like a project (D3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureAsset {
    pub id: AssetId,
    pub bytes: u64,
    pub kind: FixtureMediaKind,
}

/// Mirror of the #98 `Pad` subset that export depends on.
#[derive(Clone, Debug, PartialEq)]
pub struct FixturePad {
    pub id: u8,
    pub source: Option<AssetId>,
    /// D4: the audible output is this retained render, never a regeneration.
    pub retained: Option<AssetId>,
    /// Nonempty when the pad's chain is baked into the retained render.
    pub baked_chain: Vec<String>,
    /// Choke-group behavior has no neutral bundle mapping.
    pub choke: bool,
}

/// Mirror of the #104/#106 `Target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureTarget {
    Pad(u8),
    Sample(AssetId),
    Instrument(u128),
}

/// Mirror of the #107 effective edit view: the exported onset may differ from
/// the captured original; the difference is recorded as a conversion loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureEdit {
    pub effective_start_nanos: u64,
    pub detail: String,
}

/// Mirror of a #104/#106 original event. Times are absolute nanoseconds; the
/// bundle transaction subtracts its single time origin.
#[derive(Clone, Debug, PartialEq)]
pub struct FixtureEvent {
    pub id: EventId,
    pub target: FixtureTarget,
    pub start_nanos: u64,
    pub duration_nanos: u64,
    pub velocity: Option<u8>,
    pub pitch: Option<u8>,
    pub channel: Option<u8>,
    /// Per-hit retained render to copy into the bundle (D4), overriding the
    /// pad's default audible asset.
    pub asset: Option<AssetId>,
    /// This hit is a rendered variant of that event (pitch/choke/processing).
    pub variant_of: Option<EventId>,
    pub edit: Option<FixtureEdit>,
}

/// Mirror of a #106 take: event IDs are unique per take by sequence, and the
/// recording tempo is retained with the take.
#[derive(Clone, Debug, PartialEq)]
pub struct FixtureTake {
    pub id: u128,
    pub recording_tempo_milli_bpm: u32,
    pub events: Vec<FixtureEvent>,
}

/// Mirror of the #107 part grouping: what a bundle part (track) carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixturePartKind {
    PadClips { pad: u8 },
    InstrumentNotes { instrument: u128 },
    SampleClips { source: AssetId },
}

/// A bundle part and the stable event IDs it renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixturePart {
    pub id: PartId,
    pub kind: FixturePartKind,
    pub event_ids: Vec<EventId>,
}

/// A processing record the adapter supplies explicitly, joined to one part
/// and asset. Kept neutral so bundle.rs never reads a project or plugin type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureLoss {
    pub part: PartId,
    pub asset: AssetId,
    pub detail: String,
}

/// The whole synthetic export input: takes (events), pads, assets and parts.
#[derive(Clone, Debug, PartialEq)]
pub struct FixtureExport {
    pub takes: Vec<FixtureTake>,
    pub pads: Vec<FixturePad>,
    pub assets: Vec<FixtureAsset>,
    pub parts: Vec<FixturePart>,
    /// Explicit baked-processing records, such as a live instrument take's
    /// honest identity. Validation still checks every join.
    pub losses: Vec<FixtureLoss>,
}
