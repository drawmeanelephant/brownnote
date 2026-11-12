//! Versioned DAW-neutral export manifest format (issue #109).
//!
//! The manifest is the stable contract between an export bundle and any DAW
//! adapter: a versioned document, stable IDs for parts/events/assets, and
//! explicit loss records for baked processing, unsupported mappings and
//! conversion losses. Unknown or newer format versions are typed errors, and
//! unknown fields are rejected, so a reader never silently misreads a
//! manifest written by a different version.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt, path::PathBuf, str::FromStr};

/// Current manifest format version.
pub const MANIFEST_FORMAT_VERSION: u32 = 4;
/// Every manifest version this build can read.
pub const KNOWN_VERSIONS: &[u32] = &[1, 2, 3, MANIFEST_FORMAT_VERSION];
/// Bundle-relative manifest file name.
pub const MANIFEST_NAME: &str = "manifest.json";
/// Bundle-relative directory holding copied media (D3).
pub const MEDIA_DIR: &str = "media";
/// Bound on one plugin-identity string, matching the project's plugin identity
/// text bound so a manifest can never carry more than a document could.
const MAX_IDENTITY_TEXT: usize = 512;

/// Content-hash asset identity, mirroring the #98 `project::AssetId` shape:
/// media files are named by the SHA-256 of their exact encoded bytes, so two
/// pads that share a sound share one bundle file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AssetId([u8; 32]);

impl AssetId {
    /// Hash the exact encoded file bytes, not decoded or resampled audio.
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Bundle-relative media path, `media/<sha256>.wav` (D3).
    pub fn relative_path(self) -> PathBuf {
        PathBuf::from(MEDIA_DIR).join(format!("{self}.wav"))
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl From<AssetId> for String {
    fn from(id: AssetId) -> Self {
        id.to_string()
    }
}

impl FromStr for AssetId {
    type Err = ManifestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ManifestError::MalformedAssetId);
        }
        let mut digest = [0; 32];
        for (index, byte) in digest.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| ManifestError::MalformedAssetId)?;
        }
        Ok(Self(digest))
    }
}

impl TryFrom<String> for AssetId {
    type Error = ManifestError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

/// Stable part (track) identity in the bundle, assigned by the exporter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PartId(pub u64);

/// Stable event identity, mirroring the #104/#106 `EventId` shape: unique
/// within a take by monotonically increasing sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId {
    #[serde(with = "identity128")]
    pub take: u128,
    pub sequence: u64,
}

/// The bundle's single time origin. Every event time in the manifest is
/// relative to this origin, so all parts, stems and the reference mix
/// align to one origin instead of per-take clocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeOrigin {
    pub nanos: u64,
}

/// Export timing context: recording tempo in milli-BPM and the bundle sample
/// rate that audio writers align to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timing {
    pub tempo_milli_bpm: u32,
    pub sample_rate_hz: u32,
}

/// Which external plugin made a live take. Identity only: no state, no
/// parameters, no machine path, so the bundle names a plugin another host can
/// re-drive where it can load one, and nothing it cannot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIdentity {
    pub backend: String,
    pub id: String,
    /// SHA-256 of the loaded binary, 64 lowercase hex digits.
    pub artifact: String,
    pub abi: String,
    pub version: String,
}

impl PluginIdentity {
    /// Identity for a loss or adapter record. Never includes a machine path.
    pub fn describe(&self) -> String {
        let artifact: String = self.artifact.chars().take(12).collect();
        format!(
            "{} plugin \"{}\" ({} {}, artifact {artifact}…)",
            self.backend, self.id, self.version, self.abi
        )
    }

    fn validate(&self) -> bool {
        let text = |value: &str| {
            !value.is_empty()
                && value.len() <= MAX_IDENTITY_TEXT
                && !value.chars().any(char::is_control)
        };
        text(&self.backend)
            && text(&self.id)
            && text(&self.abi)
            && text(&self.version)
            && self.artifact.len() == 64
            && self
                .artifact
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

/// What a bundle part carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PartKind {
    /// Separate pad-hit clips; every hit stays its own event, collisions are
    /// never collapsed.
    PadClips {
        pad: u8,
    },
    InstrumentNotes {
        #[serde(with = "identity128")]
        instrument: u128,
    },
    SampleClips {
        source: AssetId,
    },
    /// Editable, sample-timed notes from a live external instrument take. The
    /// kind names the plugin identity, never the sampled instrument: the notes
    /// are the performance, and only a host that can load that plugin can
    /// sound them. The take ID arrives on every event identity.
    LiveInstrumentNotes {
        plugin: PluginIdentity,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartEntry {
    pub id: PartId,
    pub kind: PartKind,
}

/// One exported event, joined to its part and (optionally) retained asset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEntry {
    pub id: EventId,
    pub part: PartId,
    /// Effective onset relative to the bundle time origin, nanoseconds.
    pub start_nanos: u64,
    pub duration_nanos: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    /// Retained media copied into the bundle for this hit (D4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetId>,
    /// Original, unprocessed source, kept even when `asset` is a baked hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AssetId>,
    /// Base event of the per-hit variant group this event renders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_of: Option<EventId>,
}

/// Mirror of #98 `MediaKind`: source bytes vs an authoritative retained render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Source,
    RetainedRender,
}

/// A media file copied into the bundle, named by content hash (D3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetEntry {
    pub id: AssetId,
    pub kind: AssetKind,
    pub bytes: u64,
    /// Bundle-relative path; always `media/<sha256>.wav`.
    pub path: PathBuf,
}

/// Each hit entry is an independent clip/track, never a destructive overlap
/// on a shared pad track. Stems/reference begin at frame zero of `origin`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioEntry {
    pub role: AudioRole,
    pub part: Option<PartId>,
    pub event: Option<EventId>,
    pub asset: AssetId,
    pub start_frame: u64,
    pub frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioRole {
    HitWet,
    HitDry,
    StemWet,
    StemDry,
    Reference,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MidiTrack {
    /// SMF track zero is the conductor; note tracks are one-based.
    pub index: u16,
    pub part: PartId,
    pub events: Vec<EventId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MidiEntry {
    pub path: PathBuf,
    pub ppq: u16,
    pub tracks: Vec<MidiTrack>,
}

/// An explicit, joinable record of information the neutral bundle cannot
/// represent losslessly. Every variant references stable manifest IDs, so a
/// consumer can always resolve what was lost and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LossRecord {
    /// Processing (pedal/plugin/instrument chain) is baked into the retained
    /// render and cannot be un-baked by the DAW.
    BakedProcessing {
        part: PartId,
        asset: AssetId,
        detail: String,
    },
    /// A mapping has no neutral representation (e.g. pad choke groups). The
    /// events are kept as separate hits, never collapsed.
    UnsupportedMapping { event: EventId, reason: String },
    /// A conversion lost information (e.g. an edit moved the onset).
    ConversionLoss { event: EventId, detail: String },
}

/// The versioned bundle manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub origin: TimeOrigin,
    pub timing: Timing,
    /// Static project master gain applied to the reference mix. Hit, dry and
    /// stem assets are pre-master, so an adapter must apply this once to the
    /// imported mix rather than baking it into individual clips.
    #[serde(default = "unity_master_gain")]
    pub master_gain: f32,
    pub parts: Vec<PartEntry>,
    pub events: Vec<EventEntry>,
    pub assets: Vec<AssetEntry>,
    pub losses: Vec<LossRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<MidiEntry>,
}

fn unity_master_gain() -> f32 {
    1.0
}

impl Manifest {
    pub fn new(origin: TimeOrigin, timing: Timing) -> Self {
        Self {
            version: MANIFEST_FORMAT_VERSION,
            origin,
            timing,
            master_gain: unity_master_gain(),
            parts: Vec::new(),
            events: Vec::new(),
            assets: Vec::new(),
            losses: Vec::new(),
            audio: Vec::new(),
            midi: None,
        }
    }

    pub fn part(&self, id: PartId) -> Option<&PartEntry> {
        self.parts.iter().find(|part| part.id == id)
    }

    pub fn event(&self, id: EventId) -> Option<&EventEntry> {
        self.events.iter().find(|event| event.id == id)
    }

    pub fn asset(&self, id: AssetId) -> Option<&AssetEntry> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    /// Validate the format version and every join, then encode. A manifest
    /// that cannot be read back with all references resolving is never
    /// written into a bundle.
    pub fn encode(&self) -> Result<Vec<u8>, ManifestError> {
        self.validate()?;
        if self.version != MANIFEST_FORMAT_VERSION {
            return Err(ManifestError::ReadOnlyVersion(self.version));
        }
        serde_json::to_vec_pretty(self).map_err(|_| ManifestError::Encode)
    }

    /// Decode and fully validate a manifest, rejecting malformed JSON,
    /// unknown/newer versions and dangling references with typed errors.
    pub fn decode(bytes: &[u8]) -> Result<Self, ManifestError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }
        let probe: VersionProbe =
            serde_json::from_slice(bytes).map_err(|_| ManifestError::MalformedJson)?;
        check_version(probe.version)?;
        let manifest: Manifest =
            serde_json::from_slice(bytes).map_err(|_| ManifestError::MalformedJson)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Version and join validation: no duplicate IDs, every event's part,
    /// asset and variant reference resolves, every loss record resolves, and
    /// every asset path is its content-hash path.
    pub fn validate(&self) -> Result<(), ManifestError> {
        check_version(self.version)?;
        if !self.master_gain.is_finite() || !(0.0..=1.0).contains(&self.master_gain) {
            return Err(ManifestError::InvalidMaster);
        }
        let mut parts = BTreeSet::new();
        for part in &self.parts {
            if !parts.insert(part.id) {
                return Err(ManifestError::DuplicatePart(part.id));
            }
            if let PartKind::LiveInstrumentNotes { plugin } = &part.kind
                && !plugin.validate()
            {
                return Err(ManifestError::InvalidPluginIdentity(part.id));
            }
        }
        let mut assets = BTreeSet::new();
        for asset in &self.assets {
            if !assets.insert(asset.id) {
                return Err(ManifestError::DuplicateAsset(asset.id));
            }
            if asset.path != asset.id.relative_path() {
                return Err(ManifestError::AssetPathMismatch(asset.id));
            }
        }
        let mut events = BTreeSet::new();
        for event in &self.events {
            if !events.insert(event.id) {
                return Err(ManifestError::DuplicateEvent(event.id));
            }
        }
        for event in &self.events {
            if !parts.contains(&event.part) {
                return Err(ManifestError::DanglingEventPart {
                    event: event.id,
                    part: event.part,
                });
            }
            if let Some(asset) = event.asset
                && !assets.contains(&asset)
            {
                return Err(ManifestError::DanglingEventAsset {
                    event: event.id,
                    asset,
                });
            }
            if let Some(asset) = event.source
                && !assets.contains(&asset)
            {
                return Err(ManifestError::DanglingEventAsset {
                    event: event.id,
                    asset,
                });
            }
            if let Some(variant_of) = event.variant_of
                && !events.contains(&variant_of)
            {
                return Err(ManifestError::DanglingVariant {
                    event: event.id,
                    variant_of,
                });
            }
        }
        for (index, loss) in self.losses.iter().enumerate() {
            match loss {
                LossRecord::BakedProcessing { part, asset, .. } => {
                    if !parts.contains(part) {
                        return Err(ManifestError::DanglingLossPart { index, part: *part });
                    }
                    if !assets.contains(asset) {
                        return Err(ManifestError::DanglingLossAsset {
                            index,
                            asset: *asset,
                        });
                    }
                }
                LossRecord::UnsupportedMapping { event, .. }
                | LossRecord::ConversionLoss { event, .. } => {
                    if !events.contains(event) {
                        return Err(ManifestError::DanglingLossEvent {
                            index,
                            event: *event,
                        });
                    }
                }
            }
        }
        for entry in &self.audio {
            if !assets.contains(&entry.asset)
                || entry.part.is_some_and(|id| !parts.contains(&id))
                || entry.event.is_some_and(|id| {
                    self.event(id)
                        .is_none_or(|event| Some(event.part) != entry.part)
                })
                || entry.frames == 0
                || entry.start_frame.checked_add(entry.frames).is_none()
                || match entry.role {
                    AudioRole::HitWet | AudioRole::HitDry => entry.event.is_none(),
                    AudioRole::StemWet | AudioRole::StemDry => {
                        entry.part.is_none() || entry.event.is_some() || entry.start_frame != 0
                    }
                    AudioRole::Reference => {
                        entry.part.is_some() || entry.event.is_some() || entry.start_frame != 0
                    }
                }
            {
                return Err(ManifestError::InvalidAudio);
            }
        }
        if let Some(midi) = &self.midi {
            let mut seen = BTreeSet::new();
            let mut indices = BTreeSet::new();
            if midi.path != std::path::Path::new("notes.mid") || midi.ppq != 960 {
                return Err(ManifestError::InvalidMidi);
            }
            for track in &midi.tracks {
                if track.index == 0
                    || !indices.insert(track.index)
                    || !matches!(
                        self.part(track.part).map(|part| &part.kind),
                        Some(PartKind::InstrumentNotes { .. })
                    )
                {
                    return Err(ManifestError::InvalidMidi);
                }
                for id in &track.events {
                    if !seen.insert(*id)
                        || self.event(*id).is_none_or(|event| event.part != track.part)
                    {
                        return Err(ManifestError::InvalidMidi);
                    }
                }
            }
            if self.events.iter().any(|event| {
                matches!(
                    self.part(event.part).map(|part| &part.kind),
                    Some(PartKind::InstrumentNotes { .. })
                ) && !seen.contains(&event.id)
            }) {
                return Err(ManifestError::InvalidMidi);
            }
        }
        Ok(())
    }
}

/// Version gate: anything above the current version is reported as newer;
/// anything else this build has never known is reported as unknown.
pub fn check_version(version: u32) -> Result<(), ManifestError> {
    if version > MANIFEST_FORMAT_VERSION {
        return Err(ManifestError::NewerVersion {
            found: version,
            supported: MANIFEST_FORMAT_VERSION,
        });
    }
    if !KNOWN_VERSIONS.contains(&version) {
        return Err(ManifestError::UnknownVersion(version));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    ReadOnlyVersion(u32),
    InvalidAudio,
    InvalidMaster,
    InvalidMidi,
    InvalidPluginIdentity(PartId),
    MalformedJson,
    MalformedAssetId,
    Encode,
    UnknownVersion(u32),
    NewerVersion { found: u32, supported: u32 },
    DuplicatePart(PartId),
    DuplicateEvent(EventId),
    DuplicateAsset(AssetId),
    DanglingEventPart { event: EventId, part: PartId },
    DanglingEventAsset { event: EventId, asset: AssetId },
    DanglingVariant { event: EventId, variant_of: EventId },
    DanglingLossEvent { index: usize, event: EventId },
    DanglingLossPart { index: usize, part: PartId },
    DanglingLossAsset { index: usize, asset: AssetId },
    AssetPathMismatch(AssetId),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "export manifest: {self:?}")
    }
}
impl std::error::Error for ManifestError {}

// JSON numbers are not portable 128-bit identities. V2 writes decimal strings;
// accept v1's u64 numbers while preserving the complete real registry IDs.
mod identity128 {
    use serde::{
        Deserializer, Serializer,
        de::{self, Visitor},
    };
    use std::fmt;

    pub fn serialize<S: Serializer>(value: &u128, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u128, D::Error> {
        struct Identity;
        impl Visitor<'_> for Identity {
            type Value = u128;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal identity string or a legacy unsigned integer")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<u128, E> {
                value.parse().map_err(E::custom)
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<u128, E> {
                Ok(u128::from(value))
            }
        }
        deserializer.deserialize_any(Identity)
    }
}
