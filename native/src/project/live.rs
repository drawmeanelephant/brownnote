//! The project's one live CLAP instrument (D8, D4).
//!
//! Identity is the stable plugin tuple the helper reported: backend, plugin
//! ID, artifact SHA-256, ABI and version. The machine path is only a hint for
//! reconnecting the bundle here; it is never identity and never required.
//! Parameters and opaque state are what a fresh host restores. A take the
//! live instrument played into keeps its captured audio as project media, so
//! the take still plays after the plugin is gone. Live audio is not an offline
//! render: there is no render recipe and no re-render on recall.
use super::{
    AssetId,
    plugin::{PluginParam, StateBytes},
};
use crate::pedals::offline::{
    Backend, Hash256, MAX_PARAMS, MAX_RATE, MAX_STATE_BYTES, MAX_TEXT, MIN_RATE, Param, Plugin,
    Stage,
};
use crate::performance::TakeId;
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

pub const FORMAT_VERSION: u32 = 1;
/// Retained take clips one project may carry. Each is checked project media.
pub const MAX_TAKES: usize = 64;
/// A location hint is a path, still bounded like the offline stage hint.
const MAX_HINT: usize = 4 * MAX_TEXT;

/// Captured audio of one take that the live instrument played into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveTakeDocument {
    /// Canonical decimal `TakeId`.
    pub take: String,
    /// Stereo float WAV, retained byte-for-byte as project media (D4).
    pub asset: AssetId,
    pub rate: u32,
    pub frames: u64,
}

impl LiveTakeDocument {
    pub fn id(&self) -> Option<TakeId> {
        self.take
            .parse::<u128>()
            .ok()
            .filter(|id| self.take == id.to_string())
            .map(TakeId)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveInstrumentDocument {
    pub version: u32,
    /// Backend, plugin ID, artifact SHA-256, ABI and version.
    pub plugin: Plugin,
    /// The display name the helper reported.
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<PluginParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateBytes>,
    /// Where this machine last found the bundle. A reconnect hint (D8), never
    /// identity and never required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_hint: Option<String>,
    /// The user asked this project's instrument to run. A disabled record
    /// keeps its honest identity and retained takes without starting a helper.
    pub enabled: bool,
    /// Takes the instrument played into, with their retained audio.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub takes: Vec<LiveTakeDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveDocumentError {
    UnsupportedVersion(u32),
    NotClap(Backend),
    InvalidIdentity,
    InvalidName,
    InvalidHint,
    InvalidParams,
    StateTooLarge,
    TooManyTakes,
    InvalidTakeId,
    DuplicateTake(TakeId),
    InvalidTake,
}

impl fmt::Display for LiveDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "live instrument document: {self:?}")
    }
}
impl std::error::Error for LiveDocumentError {}

impl LiveInstrumentDocument {
    pub fn from_stage(stage: &Stage, name: String, enabled: bool) -> Self {
        Self {
            version: FORMAT_VERSION,
            plugin: stage.plugin.clone(),
            name,
            params: stage
                .params
                .iter()
                .map(|param| PluginParam {
                    id: param.id,
                    value: super::plugin::Exact(param.value),
                })
                .collect(),
            state: stage.state.clone().map(StateBytes),
            location_hint: stage.location.to_str().map(str::to_owned),
            enabled,
            takes: Vec::new(),
        }
    }

    /// The stage to start, with the supplied machine location (a relinked path
    /// or the saved hint). Location is not part of any identity.
    pub fn to_stage(&self, location: PathBuf) -> Stage {
        Stage {
            plugin: self.plugin.clone(),
            location,
            params: self
                .params
                .iter()
                .map(|param| Param {
                    id: param.id,
                    value: param.value.0,
                })
                .collect(),
            state: self.state.as_ref().map(|state| state.0.clone()),
        }
    }

    /// Identity and parameters, for status lines and honest export records.
    pub fn describe(&self) -> String {
        let artifact = self.plugin.artifact.to_string();
        format!(
            "{} plugin \"{}\" ({} {}, {}, bundle sha256 {}…) with {} parameter{} and opaque state",
            self.plugin.backend,
            self.name,
            self.plugin.id,
            self.plugin.version,
            self.plugin.abi,
            &artifact[..12],
            self.params.len(),
            if self.params.len() == 1 { "" } else { "s" },
        )
    }

    /// Identity only, for a disabled status that must stay honest without a
    /// loaded helper.
    pub fn identity_line(&self) -> String {
        let artifact = self.plugin.artifact.to_string();
        format!(
            "{} {} · {} · artifact {}…",
            self.plugin.backend,
            self.plugin.id,
            self.plugin.version,
            &artifact[..12]
        )
    }

    pub fn take(&self, id: TakeId) -> Option<&LiveTakeDocument> {
        self.takes.iter().find(|take| take.id() == Some(id))
    }

    pub fn validate(&self) -> Result<(), LiveDocumentError> {
        if self.version != FORMAT_VERSION {
            return Err(LiveDocumentError::UnsupportedVersion(self.version));
        }
        if self.plugin.backend != Backend::Clap {
            return Err(LiveDocumentError::NotClap(self.plugin.backend));
        }
        for text in [
            self.plugin.id.as_str(),
            self.plugin.abi.as_str(),
            self.plugin.version.as_str(),
        ] {
            if text.is_empty() || text.len() > MAX_TEXT || text.chars().any(char::is_control) {
                return Err(LiveDocumentError::InvalidIdentity);
            }
        }
        if self.name.len() > MAX_TEXT || self.name.chars().any(char::is_control) {
            return Err(LiveDocumentError::InvalidName);
        }
        if self
            .location_hint
            .as_ref()
            .is_some_and(|hint| hint.len() > MAX_HINT || hint.chars().any(char::is_control))
        {
            return Err(LiveDocumentError::InvalidHint);
        }
        if self.params.len() > MAX_PARAMS
            || self.params.windows(2).any(|pair| pair[0].id >= pair[1].id)
        {
            return Err(LiveDocumentError::InvalidParams);
        }
        if self
            .state
            .as_ref()
            .is_some_and(|state| state.0.len() > MAX_STATE_BYTES)
        {
            return Err(LiveDocumentError::StateTooLarge);
        }
        if self.takes.len() > MAX_TAKES {
            return Err(LiveDocumentError::TooManyTakes);
        }
        let mut ids = std::collections::BTreeSet::new();
        for take in &self.takes {
            let id = take.id().ok_or(LiveDocumentError::InvalidTakeId)?;
            if !ids.insert(id) {
                return Err(LiveDocumentError::DuplicateTake(id));
            }
            if !(MIN_RATE..=MAX_RATE).contains(&take.rate) || take.frames == 0 {
                return Err(LiveDocumentError::InvalidTake);
            }
        }
        Ok(())
    }

    /// SHA-256 of the exact plugin tuple, for a compact honest record.
    pub fn identity_hash(&self) -> Hash256 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.plugin.id.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&self.plugin.artifact.0);
        bytes.push(0);
        bytes.extend_from_slice(self.plugin.abi.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.plugin.version.as_bytes());
        Hash256::of(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pedals::offline::{Backend, Param},
        project::{Asset, AssetRef, Document, MediaKind},
    };

    fn plugin(id: &str) -> Plugin {
        Plugin {
            backend: Backend::Clap,
            id: id.into(),
            artifact: Hash256::of(b"bundle bytes"),
            abi: "clap-1.2".into(),
            version: "1.0.0".into(),
        }
    }

    fn document() -> LiveInstrumentDocument {
        let stage = Stage {
            plugin: plugin("org.brownnote.fixture.instrument"),
            location: PathBuf::from("/Users/someone/Instrument.clap"),
            params: vec![Param { id: 3, value: 0.25 }],
            state: Some(vec![1, 2, 3, 4].into()),
        };
        LiveInstrumentDocument::from_stage(&stage, "Fixture".into(), true)
    }

    fn project(live: LiveInstrumentDocument) -> Document {
        let bytes = b"stereo float wav bytes";
        let id = AssetId::of(bytes);
        let mut live = live;
        live.takes.push(LiveTakeDocument {
            take: "11".into(),
            asset: id,
            rate: 48_000,
            frames: 96_000,
        });
        Document {
            assets: vec![Asset {
                reference: AssetRef::of(bytes),
                kind: MediaKind::LiveTakeRender,
            }],
            live_instrument: Some(live),
            ..Document::default()
        }
    }

    #[test]
    fn a_live_record_round_trips_with_its_identity_state_and_retained_take() {
        let document = project(document());
        document.validate().unwrap();
        let bytes = document.encode().unwrap();
        let reopened = Document::decode(&bytes).unwrap();
        assert_eq!(reopened, document);
        let live = reopened.live_instrument.as_ref().unwrap();
        assert_eq!(live.plugin.id, "org.brownnote.fixture.instrument");
        assert_eq!(live.plugin.artifact, Hash256::of(b"bundle bytes"));
        assert_eq!(
            live.location_hint.as_deref(),
            Some("/Users/someone/Instrument.clap")
        );
        assert_eq!(live.state.as_ref().unwrap().0.as_ref(), &[1, 2, 3, 4]);
        assert_eq!(live.params.len(), 1);
        let take = live.take(TakeId(11)).unwrap();
        assert_eq!(take.frames, 96_000);
        assert_eq!(take.rate, 48_000);
    }

    #[test]
    fn a_location_is_not_identity_and_missing_assets_fail_explicitly() {
        let mut document = project(document());
        {
            let live = document.live_instrument.as_mut().unwrap();
            live.location_hint = Some("/somewhere/else/Instrument.clap".into());
        }
        document.validate().unwrap();
        let before = document.live_instrument.as_ref().unwrap().identity_hash();
        document.live_instrument.as_mut().unwrap().location_hint =
            Some("/third/place/Instrument.clap".into());
        assert_eq!(
            document.live_instrument.as_ref().unwrap().identity_hash(),
            before
        );

        let mut missing = document.clone();
        missing.assets.clear();
        assert_eq!(
            missing.validate(),
            Err(crate::project::DocumentError::MissingAsset(
                missing.live_instrument.as_ref().unwrap().takes[0].asset
            ))
        );
    }

    #[test]
    fn malformed_identity_state_and_take_ids_are_rejected() {
        let mut non_clap = document();
        non_clap.plugin.backend = Backend::Ladspa;
        assert_eq!(
            non_clap.validate(),
            Err(LiveDocumentError::NotClap(Backend::Ladspa))
        );

        let mut huge = document();
        huge.state = Some(StateBytes(vec![0; MAX_STATE_BYTES + 1].into()));
        assert_eq!(huge.validate(), Err(LiveDocumentError::StateTooLarge));

        let mut unordered = document();
        unordered.params = vec![
            PluginParam {
                id: 2,
                value: super::super::plugin::Exact(1.0),
            },
            PluginParam {
                id: 1,
                value: super::super::plugin::Exact(2.0),
            },
        ];
        assert_eq!(unordered.validate(), Err(LiveDocumentError::InvalidParams));

        let mut noncanonical = project(document());
        let live = noncanonical.live_instrument.as_mut().unwrap();
        live.takes[0].take = "011".into();
        assert_eq!(
            noncanonical
                .live_instrument
                .as_ref()
                .unwrap()
                .validate()
                .unwrap_err(),
            LiveDocumentError::InvalidTakeId
        );

        let mut duplicate = project(document());
        let live = duplicate.live_instrument.as_mut().unwrap();
        let copy = live.takes[0].clone();
        live.takes.push(copy);
        assert_eq!(
            live.validate().unwrap_err(),
            LiveDocumentError::DuplicateTake(TakeId(11))
        );
    }

    #[test]
    fn an_older_document_without_a_live_record_still_opens() {
        let old = Document::default();
        old.validate().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&old.encode().unwrap()).unwrap();
        assert!(value.get("live_instrument").is_none());
        let reopened = Document::decode(&old.encode().unwrap()).unwrap();
        assert!(reopened.live_instrument.is_none());
    }
}
