//! Versioned kit document, not UI state or a storage transaction.
//!
//! D3/D4/D8: project.json + media/<sha256>.wav; required renders are retained.
//! The v1 kit boundary remains provisional until the #104 contract lands.
pub mod media;
pub mod recovery;
pub mod session;
pub mod store;

use crate::{
    midi::{
        input::{MAPPINGS_FORMAT_VERSION, MappingDocumentError, MappingsDocument},
        mapping::Mappings,
    },
    pedals::render::{Chain, MAX_STAGES, Stage},
};
pub use media::{Asset, AssetId, AssetRef, MediaKind};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

pub const FORMAT_VERSION: u32 = 1;
pub const DOCUMENT_NAME: &str = "project.json";
pub const PAD_COUNT: usize = 16;

/// Stable grid identity (0..15), never an engine sample slot or choke group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct PadId(u8);

impl PadId {
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}
impl TryFrom<u8> for PadId {
    type Error = DocumentError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if usize::from(value) < PAD_COUNT {
            Ok(Self(value))
        } else {
            Err(DocumentError::InvalidPad(value))
        }
    }
}
impl From<PadId> for u8 {
    fn from(value: PadId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleAssignment {
    pub source: AssetId,
    /// Play this exact output, not a regenerated approximation.
    pub retained: Option<AssetId>,
}

impl SampleAssignment {
    pub fn audible(&self) -> AssetId {
        self.retained.unwrap_or(self.source)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pad {
    pub sample: Option<SampleAssignment>,
    pub gain: f32,
    pub pan: f32,
    pub semitones: f32,
    #[serde(with = "chain_document")]
    pub chain: Chain,
}

impl Default for Pad {
    fn default() -> Self {
        Self {
            sample: None,
            gain: 1.0,
            pan: 0.0,
            semitones: 0.0,
            chain: Chain::default(),
        }
    }
}

/// Pad array position is its stable PadId. Machine preferences, runtime slots,
/// voices, render jobs and evictable caches have no fields here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub version: u32,
    pub pads: [Pad; PAD_COUNT],
    pub assets: Vec<Asset>,
    pub master: f32,
    pub choke: bool,
    /// D8: channel-keyed project data, never the selected input device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_mappings: Option<MappingsDocument>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            pads: std::array::from_fn(|_| Pad::default()),
            assets: Vec::new(),
            master: 0.8,
            choke: true,
            midi_mappings: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentError {
    MalformedJson,
    InvalidDocument,
    OlderVersion(u32),
    NewerVersion(u32),
    InvalidPad(u8),
    Setting {
        pad: Option<PadId>,
        name: &'static str,
    },
    DuplicateAsset(AssetId),
    MissingAsset(AssetId),
    InvalidChain(PadId),
    MissingRetained(PadId),
    WrongMediaKind(AssetId),
    MidiMappings(MappingDocumentError),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "project document: {self:?}")
    }
}
impl std::error::Error for DocumentError {}

impl Document {
    /// Inspect the version before interpreting its schema. No migration or
    /// silent dropping of unknown fields, and no writes on any failure.
    pub fn decode(bytes: &[u8]) -> Result<Self, DocumentError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| DocumentError::MalformedJson)?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(DocumentError::InvalidDocument)?;
        check_version(version)?;
        let document: Self =
            serde_json::from_slice(bytes).map_err(|_| DocumentError::InvalidDocument)?;
        document.validate()?;
        Ok(document)
    }

    pub fn encode(&self) -> Result<Vec<u8>, DocumentError> {
        self.validate()?;
        serde_json::to_vec_pretty(self).map_err(|_| DocumentError::InvalidDocument)
    }

    /// Legacy v1 documents without mappings open with an empty mapping bank.
    pub fn mappings(&self) -> Result<Mappings, DocumentError> {
        match &self.midi_mappings {
            Some(document) => document.to_mappings(),
            None => MappingsDocument {
                version: MAPPINGS_FORMAT_VERSION,
                mappings: Vec::new(),
            }
            .to_mappings(),
        }
        .map_err(DocumentError::MidiMappings)
    }

    pub fn validate(&self) -> Result<(), DocumentError> {
        check_version(self.version)?;
        self.mappings()?;
        if !in_range(self.master, 0.0, 1.0) {
            return Err(DocumentError::Setting {
                pad: None,
                name: "master",
            });
        }
        let mut ids = BTreeSet::new();
        for asset in &self.assets {
            if !ids.insert(asset.reference.id) {
                return Err(DocumentError::DuplicateAsset(asset.reference.id));
            }
        }
        for (index, pad) in self.pads.iter().enumerate() {
            let id = PadId(index as u8);
            for (name, value, min, max) in [
                ("gain", pad.gain, 0.0, 1.5),
                ("pan", pad.pan, -1.0, 1.0),
                ("semitones", pad.semitones, -12.0, 12.0),
            ] {
                if !in_range(value, min, max) {
                    return Err(DocumentError::Setting {
                        pad: Some(id),
                        name,
                    });
                }
            }
            let mut effects = BTreeSet::new();
            if pad.chain.stages.len() > MAX_STAGES
                || pad.chain.stages.iter().any(|stage| {
                    let mut pots = BTreeSet::new();
                    stage.effect.is_empty()
                        || !effects.insert(&stage.effect)
                        || stage.mix.is_some_and(|mix| mix > 120)
                        || stage
                            .pots
                            .iter()
                            .any(|(name, _)| name.is_empty() || !pots.insert(name))
                })
            {
                return Err(DocumentError::InvalidChain(id));
            }
            if let Some(sample) = &pad.sample {
                self.require_asset(sample.source, MediaKind::Source)?;
                if let Some(retained) = sample.retained {
                    self.require_asset(retained, MediaKind::RetainedRender)?;
                } else if !pad.chain.stages.is_empty() {
                    return Err(DocumentError::MissingRetained(id));
                }
            } else if !pad.chain.stages.is_empty() {
                return Err(DocumentError::MissingRetained(id));
            }
        }
        Ok(())
    }

    fn require_asset(&self, id: AssetId, kind: MediaKind) -> Result<(), DocumentError> {
        let asset = self
            .assets
            .iter()
            .find(|a| a.reference.id == id)
            .ok_or(DocumentError::MissingAsset(id))?;
        // A retained render can itself be reused as another pad's source.
        if kind == MediaKind::RetainedRender && asset.kind != kind {
            return Err(DocumentError::WrongMediaKind(id));
        }
        Ok(())
    }
}

fn in_range(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && (min..=max).contains(&value)
}

fn check_version(version: u32) -> Result<(), DocumentError> {
    match version.cmp(&FORMAT_VERSION) {
        std::cmp::Ordering::Less => Err(DocumentError::OlderVersion(version)),
        std::cmp::Ordering::Greater => Err(DocumentError::NewerVersion(version)),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

// serde remote derives preserve the existing typed Stage/Chain values without
// making runtime render jobs serializable or requiring edits to another lane.
mod chain_document {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(remote = "Stage", deny_unknown_fields)]
    struct StageFields {
        effect: String,
        pots: Vec<(String, u8)>,
        mix: Option<u8>,
    }

    #[derive(Serialize, Deserialize)]
    struct StageValue(#[serde(with = "StageFields")] Stage);

    #[derive(Serialize)]
    struct StageBorrow<'a>(#[serde(with = "StageFields")] &'a Stage);

    pub fn serialize<S: Serializer>(chain: &Chain, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(chain.stages.iter().map(StageBorrow))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Chain, D::Error> {
        Ok(Chain {
            stages: Vec::<StageValue>::deserialize(deserializer)?
                .into_iter()
                .map(|stage| stage.0)
                .collect(),
        })
    }
}
