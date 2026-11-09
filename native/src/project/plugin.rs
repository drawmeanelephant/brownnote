//! Backend-neutral offline plugin stages a pad keeps in its project.
//!
//! A pad with a plugin chain plays its retained render (D4); these records
//! describe how that render was made so it can be re-rendered, relinked or
//! reported. Nothing here is needed to play the pad: a missing plugin, a
//! deleted render cache or a relocated project leaves the retained sound
//! playing. CLAP and LADSPA use the same shape; backend differences live in
//! [`Plugin`], the parameter IDs and the opaque state.
use crate::pedals::offline::{
    self, Hash256, Layout, MAX_BLOCK, MAX_PARAMS, MAX_STATE_BYTES, MAX_TEXT, MIN_BLOCK, Param,
    Plugin, RenderPolicy, Request, Source, Stage, TailPolicy,
};
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, str::FromStr, sync::Arc};

pub const MAX_STAGES: usize = offline::MAX_STAGES;

/// An `f64` written as its shortest round-trip decimal string. Render
/// identities hash exact bits, and the JSON number parser is not guaranteed
/// to round correctly, so a number could reopen one bit different.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Exact(pub f64);

impl TryFrom<String> for Exact {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match f64::from_str(&value) {
            Ok(number) if number.is_finite() => Ok(Self(number)),
            _ => Err(format!("not a finite number: {value:?}")),
        }
    }
}

impl From<Exact> for String {
    fn from(value: Exact) -> Self {
        value.0.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginParam {
    /// CLAP parameter ID or LADSPA control port index.
    pub id: u32,
    pub value: Exact,
}

/// Opaque plugin state, stored as base64 in the document. It is only valid
/// for the same plugin and backend, and reloading it may not reproduce the
/// retained sound.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StateBytes(pub Arc<[u8]>);

impl fmt::Debug for StateBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "StateBytes({} bytes, {})",
            self.0.len(),
            Hash256::of(&self.0)
        )
    }
}

impl TryFrom<String> for StateBytes {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        base64::decode(&value).map(|bytes| Self(bytes.into()))
    }
}

impl From<StateBytes> for String {
    fn from(value: StateBytes) -> Self {
        base64::encode(&value.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStage {
    /// Backend, plugin ID, artifact SHA-256, ABI and version: the identity a
    /// render or relink must match exactly.
    pub plugin: Plugin,
    /// The display name the helper reported.
    pub name: String,
    /// Strictly ascending by ID; applied after `state`. Absent parameters
    /// keep the plugin's defaults.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<PluginParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateBytes>,
    /// Where this plugin was last found on the machine that saved the
    /// project. A reconnect hint only (D8): never identity, never required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_hint: Option<String>,
}

impl PluginStage {
    pub fn from_stage(stage: &Stage, name: String) -> Self {
        Self {
            plugin: stage.plugin.clone(),
            name,
            params: stage
                .params
                .iter()
                .map(|param| PluginParam {
                    id: param.id,
                    value: Exact(param.value),
                })
                .collect(),
            state: stage.state.clone().map(StateBytes),
            location_hint: stage.location.to_str().map(str::to_owned),
        }
    }

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

    /// One line for export loss records and status messages.
    pub fn describe(&self) -> String {
        let plugin = &self.plugin;
        let artifact = plugin.artifact.to_string();
        format!(
            "{} plugin \"{}\" ({} {}, {}, binary sha256 {}…; {} parameter{} set{})",
            plugin.backend,
            self.name,
            plugin.id,
            plugin.version,
            plugin.abi,
            &artifact[..12],
            self.params.len(),
            if self.params.len() == 1 { "" } else { "s" },
            if self.state.is_some() {
                "; opaque plugin state kept in the project, not editable in other hosts"
            } else {
                ""
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRecord {
    pub layout: Layout,
    pub block_frames: u32,
    pub tail_threshold_dbfs: Exact,
    pub tail_hold_frames: u32,
    pub tail_max_frames: u32,
    pub compensate_latency: bool,
}

impl From<RenderPolicy> for PolicyRecord {
    fn from(policy: RenderPolicy) -> Self {
        Self {
            layout: policy.layout,
            block_frames: policy.block_frames,
            tail_threshold_dbfs: Exact(policy.tail.threshold_dbfs),
            tail_hold_frames: policy.tail.hold_frames,
            tail_max_frames: policy.tail.max_frames,
            compensate_latency: policy.compensate_latency,
        }
    }
}

impl From<PolicyRecord> for RenderPolicy {
    fn from(record: PolicyRecord) -> Self {
        Self {
            layout: record.layout,
            block_frames: record.block_frames,
            tail: TailPolicy {
                threshold_dbfs: record.tail_threshold_dbfs.0,
                hold_frames: record.tail_hold_frames,
                max_frames: record.tail_max_frames,
            },
            compensate_latency: record.compensate_latency,
        }
    }
}

/// A pad's offline plugin chain and the identity of the render it retains.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginChain {
    /// In signal order.
    pub stages: Vec<PluginStage>,
    pub policy: PolicyRecord,
    /// [`Request::identity`] of the render the pad's `retained` asset holds.
    pub render: Hash256,
}

impl PluginChain {
    /// Record what `request` rendered. `names` are the helpers' display names.
    pub fn record(request: &Request, names: &[String], render: Hash256) -> Self {
        Self {
            stages: request
                .chain
                .iter()
                .enumerate()
                .map(|(index, stage)| {
                    PluginStage::from_stage(stage, names.get(index).cloned().unwrap_or_default())
                })
                .collect(),
            policy: request.policy.into(),
            render,
        }
    }

    /// The render request for `source`, with each stage's machine location
    /// supplied by `locate` (for example a relinked path or the saved hint).
    pub fn request(
        &self,
        source: Source,
        mut locate: impl FnMut(usize, &PluginStage) -> PathBuf,
    ) -> Request {
        Request {
            source,
            chain: self
                .stages
                .iter()
                .enumerate()
                .map(|(index, stage)| stage.to_stage(locate(index, stage)))
                .collect(),
            policy: self.policy.into(),
        }
    }

    /// The identity a render of `source` through this chain would have.
    /// Locations are not part of it, so no plugin needs to be present.
    pub fn identity(&self, source: Source) -> Hash256 {
        self.request(source, |_, _| PathBuf::new()).identity()
    }

    /// Shape checks that need no audio and no plugin. The renderer repeats
    /// them, with the source, before any helper starts.
    pub fn validate(&self) -> Result<(), String> {
        if self.stages.is_empty() || self.stages.len() > MAX_STAGES {
            return Err(format!("a plugin chain holds 1..={MAX_STAGES} stages"));
        }
        let policy = &self.policy;
        if !(MIN_BLOCK..=MAX_BLOCK).contains(&policy.block_frames) {
            return Err("block size out of range".into());
        }
        if !(-200.0..=-20.0).contains(&policy.tail_threshold_dbfs.0) || policy.tail_hold_frames == 0
        {
            return Err("tail policy out of range".into());
        }
        for (index, stage) in self.stages.iter().enumerate() {
            let at = |message: &str| format!("plugin stage {}: {message}", index + 1);
            for text in [
                stage.plugin.id.as_str(),
                stage.plugin.abi.as_str(),
                stage.plugin.version.as_str(),
            ] {
                if text.is_empty() || text.len() > MAX_TEXT || text.chars().any(char::is_control) {
                    return Err(at("plugin identity text is empty, long or has controls"));
                }
            }
            if stage.name.len() > MAX_TEXT
                || stage
                    .location_hint
                    .as_ref()
                    .is_some_and(|hint| hint.len() > 4 * MAX_TEXT)
            {
                return Err(at("name or location hint is too long"));
            }
            if stage.params.len() > MAX_PARAMS
                || stage.params.windows(2).any(|pair| pair[0].id >= pair[1].id)
            {
                return Err(at("parameters must be strictly ascending by ID"));
            }
            if stage
                .state
                .as_ref()
                .is_some_and(|state| state.0.len() > MAX_STATE_BYTES)
            {
                return Err(at("plugin state is too large"));
            }
        }
        Ok(())
    }
}

/// Standard base64 with padding; the project document is JSON text.
mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let word = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            for index in 0..4 {
                if index <= chunk.len() {
                    out.push(char::from(
                        ALPHABET[(word >> (18 - 6 * index)) as usize & 63],
                    ));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(text: &str) -> Result<Vec<u8>, String> {
        let invalid = || "plugin state is not canonical base64".to_string();
        let bytes = text.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return Err(invalid());
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for (number, chunk) in bytes.chunks(4).enumerate() {
            let last = number + 1 == bytes.len() / 4;
            let padding = chunk.iter().rev().take_while(|&&b| b == b'=').count();
            if padding > 2 || (padding > 0 && !last) {
                return Err(invalid());
            }
            let mut word = 0u32;
            for &byte in &chunk[..4 - padding] {
                let value = ALPHABET
                    .iter()
                    .position(|&a| a == byte)
                    .ok_or_else(invalid)?;
                word = (word << 6) | value as u32;
            }
            word <<= 6 * padding as u32;
            let decoded = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
            out.extend_from_slice(&decoded[..3 - padding]);
        }
        // Reject non-canonical trailing bits so one state has one spelling.
        if encode(&out) != text {
            return Err(invalid());
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
