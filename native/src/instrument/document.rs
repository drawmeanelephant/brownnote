//! Project data for the one instrument (D8): identity, exact SF2 source, the
//! preset it plays, gain, and the per-note renders retained as project media
//! (D4). Runtime voices, channels and output rates are never stored.
use super::{BACKEND, BLOCK, Preset, RENDER_RATE, RenderKey};
use crate::{
    edit::{EditError, EditModel},
    export::adapter::ProjectFrozenHit,
    performance::{EventId, InstrumentId, Target},
    project::AssetId,
    takes::Takes,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub const FORMAT_VERSION: u32 = 1;
const MAX_NAME: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentDocument {
    pub version: u32,
    /// Decimal `InstrumentId`; MIDI mappings and takes target this identity.
    pub id: String,
    pub name: String,
    /// Exact SF2 bytes, owned by the project in its content-addressed media.
    pub source: AssetId,
    pub bank: u8,
    pub program: u8,
    pub gain: f32,
    /// Provenance of the renders, not a promise that another build matches.
    pub backend: String,
    pub render_rate: u32,
    /// One retained render per distinct (pitch, velocity, gate).
    #[serde(default)]
    pub renders: Vec<RenderDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderDocument {
    pub pitch: u8,
    pub velocity: u8,
    pub gate_frames: u64,
    pub asset: AssetId,
}

impl RenderDocument {
    pub fn key(&self) -> RenderKey {
        RenderKey {
            pitch: self.pitch,
            velocity: self.velocity,
            gate_frames: self.gate_frames,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentError {
    UnsupportedVersion(u32),
    InvalidId,
    InvalidName,
    InvalidPreset,
    InvalidGain,
    RenderRate(u32),
    InvalidRender,
    DuplicateRender(RenderKey),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "instrument document: {self:?}")
    }
}
impl std::error::Error for DocumentError {}

impl InstrumentDocument {
    pub fn instrument(&self) -> Result<InstrumentId, DocumentError> {
        self.id
            .parse::<u128>()
            .ok()
            .filter(|id| self.id == id.to_string())
            .map(InstrumentId)
            .ok_or(DocumentError::InvalidId)
    }

    pub fn preset(&self) -> Preset {
        Preset {
            bank: self.bank,
            program: self.program,
        }
    }

    pub fn validate(&self) -> Result<(), DocumentError> {
        if self.version != FORMAT_VERSION {
            return Err(DocumentError::UnsupportedVersion(self.version));
        }
        self.instrument()?;
        if self.name.len() > MAX_NAME || self.backend.is_empty() || self.backend.len() > MAX_NAME {
            return Err(DocumentError::InvalidName);
        }
        if self.bank > 127 || self.program > 127 {
            return Err(DocumentError::InvalidPreset);
        }
        if !(self.gain.is_finite() && (0.0..=1.0).contains(&self.gain)) {
            return Err(DocumentError::InvalidGain);
        }
        if self.render_rate != RENDER_RATE {
            return Err(DocumentError::RenderRate(self.render_rate));
        }
        let mut keys = BTreeSet::new();
        for render in &self.renders {
            if render.pitch > 127 || !(1..=127).contains(&render.velocity) {
                return Err(DocumentError::InvalidRender);
            }
            if !keys.insert(render.key()) {
                return Err(DocumentError::DuplicateRender(render.key()));
            }
        }
        Ok(())
    }

    pub fn render(&self, key: RenderKey) -> Option<AssetId> {
        self.renders
            .iter()
            .find(|render| render.key() == key)
            .map(|render| render.asset)
    }

    /// Render keys of every effective note this instrument plays now.
    pub fn needed(
        &self,
        takes: &Takes,
        edits: &EditModel,
    ) -> Result<BTreeSet<RenderKey>, EditError> {
        let Ok(id) = self.instrument() else {
            return Ok(BTreeSet::new());
        };
        Ok(edits
            .effective_notes(takes)?
            .iter()
            .filter(|note| note.target == Target::Instrument(id))
            .filter_map(|note| RenderKey::of(note, takes.tempo()).ok())
            .filter(|key| key.gate_frames <= super::MAX_GATE_FRAMES)
            .collect())
    }

    /// Export joins (#109): every effective note of this instrument whose
    /// retained render exists. The count is of notes without one; export then
    /// reports the instrument as absent from those audio fallbacks.
    pub fn frozen_hits(
        &self,
        takes: &Takes,
        edits: &EditModel,
    ) -> Result<(BTreeMap<EventId, ProjectFrozenHit>, usize), EditError> {
        let Ok(id) = self.instrument() else {
            return Ok((BTreeMap::new(), 0));
        };
        let mut hits = BTreeMap::new();
        let mut missing = 0;
        for note in edits.effective_notes(takes)? {
            if note.target != Target::Instrument(id) {
                continue;
            }
            let asset = RenderKey::of(&note, takes.tempo())
                .ok()
                .and_then(|key| self.render(key));
            match asset {
                Some(wet) => {
                    hits.insert(
                        note.id,
                        ProjectFrozenHit {
                            wet,
                            dry: None,
                            detail: format!(
                                "{} preset {} (rendered by {}); one note frozen at {} Hz \
                                 with reverb/chorus off; live onsets can trail by under {} frames",
                                self.name,
                                self.preset(),
                                self.backend,
                                self.render_rate,
                                BLOCK
                            ),
                        },
                    );
                }
                None => missing += 1,
            }
        }
        Ok((hits, missing))
    }
}

/// A new document for the instrument this build sounds.
pub fn describe(
    id: InstrumentId,
    name: &str,
    source: AssetId,
    preset: Preset,
    gain: f32,
) -> InstrumentDocument {
    InstrumentDocument {
        version: FORMAT_VERSION,
        id: id.0.to_string(),
        name: name.chars().take(MAX_NAME / 4).collect(),
        source,
        bank: preset.bank,
        program: preset.program,
        gain,
        backend: BACKEND.into(),
        render_rate: RENDER_RATE,
        renders: Vec::new(),
    }
}
