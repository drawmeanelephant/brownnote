//! Backend-neutral offline plugin render: the request, its identity, the
//! result, and the errors a caller can act on.
//!
//! Every chain stage runs in its own helper process that speaks
//! [`protocol`] (see `PROTOCOL.md`). The helper only loads the plugin and
//! processes blocks; this side owns block scheduling, latency compensation,
//! finite-output checks, tail measurement, deadlines and cancellation, so
//! those rules are the same for every backend.
//!
//! Plugin state is applied before parameters. A render's audio is retained
//! output: reloading a plugin's state does not guarantee the same sound, so
//! callers keep the rendered samples rather than re-rendering on recall.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

pub mod protocol;
mod runner;

pub use runner::{Cancel, HelperCommand, Helpers, Limits, probe, render};

/// Bumped whenever a change to rendering would change audio for an unchanged
/// request, so older cache entries stop matching.
pub const SCHEMA: u32 = 1;
pub const MIN_RATE: u32 = 8_000;
pub const MAX_RATE: u32 = 192_000;
pub const MAX_SOURCE_SECONDS: u32 = 30;
pub const MAX_STAGES: usize = 4;
pub const MAX_PARAMS: usize = 4096;
pub const MAX_STATE_BYTES: usize = 16 << 20;
pub const MAX_TEXT: usize = 512;
pub const MIN_BLOCK: u32 = 16;
pub const MAX_BLOCK: u32 = 8192;
pub const MAX_TAIL_SECONDS: u32 = 30;
/// Plugin latency beyond one second is rejected rather than rendered.
pub const MAX_LATENCY_SECONDS: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Clap,
    Ladspa,
}

impl Backend {
    /// The project document's spelling, reused by portable records such as an
    /// export part identity so the two never drift.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clap => "clap",
            Self::Ladspa => "ladspa",
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Clap => "CLAP",
            Self::Ladspa => "LADSPA",
        })
    }
}

/// The channel layout every stage of a chain processes and outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Mono,
    Stereo,
}

impl Layout {
    pub fn channels(self) -> u16 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

/// SHA-256, shown as 64 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Hash256(pub [u8; 32]);

impl Hash256 {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

impl fmt::Display for Hash256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Hash256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash256({self})")
    }
}

impl FromStr for Hash256 {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(format!("not a lowercase SHA-256: {value:?}"));
        }
        let mut digest = [0; 32];
        for (index, byte) in digest.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|error| error.to_string())?;
        }
        Ok(Self(digest))
    }
}

impl TryFrom<String> for Hash256 {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Hash256> for String {
    fn from(hash: Hash256) -> Self {
        hash.to_string()
    }
}

/// Which plugin, exactly. The helper reports these fields when it loads the
/// plugin; a render fails with [`ErrorKind::ArtifactChanged`] if they differ.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub backend: Backend,
    /// CLAP plugin ID, or LADSPA `UniqueID:Label`.
    pub id: String,
    /// SHA-256 of the loaded binary.
    pub artifact: Hash256,
    /// The plugin API and version the helper loaded it through.
    pub abi: String,
    /// The plugin's own declared version.
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Param {
    /// CLAP parameter ID or LADSPA control port index.
    pub id: u32,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub plugin: Plugin,
    /// Where this machine finds the plugin. Not part of the render identity.
    pub location: PathBuf,
    /// Strictly ascending by ID. Applied after `state`.
    pub params: Vec<Param>,
    /// Opaque plugin state, or `None` for a fresh instance.
    pub state: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub channels: u16,
    pub rate: u32,
    /// Interleaved.
    pub samples: Arc<[f32]>,
}

impl Source {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

/// How the end of a tail is found. A plugin's declared tail is reported but
/// not trusted: rendering continues on silent input until every channel
/// stays below `threshold_dbfs` for `hold_frames`, or `max_frames` of tail
/// have been rendered. Output after the last frame at or above the
/// threshold is dropped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TailPolicy {
    pub threshold_dbfs: f64,
    pub hold_frames: u32,
    pub max_frames: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderPolicy {
    pub layout: Layout,
    pub block_frames: u32,
    pub tail: TailPolicy,
    /// Drop the plugin's reported latency from the front of its output.
    pub compensate_latency: bool,
}

impl RenderPolicy {
    /// One-second hold at -90 dBFS, capped at 20 seconds of tail.
    pub fn standard(layout: Layout, rate: u32) -> Self {
        Self {
            layout,
            block_frames: 512,
            tail: TailPolicy {
                threshold_dbfs: -90.0,
                hold_frames: rate,
                max_frames: rate.saturating_mul(20),
            },
            compensate_latency: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub source: Source,
    /// In signal order.
    pub chain: Vec<Stage>,
    pub policy: RenderPolicy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StageReport {
    /// As the helper reported it; equal to the requested plugin.
    pub plugin: Plugin,
    pub name: String,
    pub latency_frames: u32,
    /// What the plugin declared: `None` for no tail, `Some(u32::MAX)` for
    /// infinite.
    pub declared_tail_frames: Option<u32>,
    /// Frames this stage rendered after its input ended.
    pub tail_frames: usize,
    /// The tail was cut at `max_frames` while still above the threshold.
    pub tail_capped: bool,
    /// The plugin's state after rendering, if it has any.
    pub state: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub identity: Hash256,
    pub layout: Layout,
    pub rate: u32,
    /// Interleaved in `layout`.
    pub samples: Arc<[f32]>,
    pub source_frames: usize,
    pub stages: Vec<StageReport>,
}

impl Rendered {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.layout.channels())
    }
    pub fn tail_capped(&self) -> bool {
        self.stages.iter().any(|stage| stage.tail_capped)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Open,
    Block(usize),
    Close,
    Total,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErrorKind {
    Invalid(String),
    /// No helper is configured for this backend.
    NoHelper(Backend),
    Spawn(String),
    /// The plugin could not be loaded: missing, not a plugin, unknown ID,
    /// or failed to initialise. `code` is the helper's stable error code.
    Unavailable {
        code: String,
        message: String,
    },
    /// The plugin loaded but cannot render this request offline.
    Unsupported(String),
    /// The plugin at `location` is not the one the request names.
    ArtifactChanged {
        expected: Box<Plugin>,
        found: Box<Plugin>,
    },
    /// The plugin reported an error after loading.
    Failed {
        code: String,
        message: String,
    },
    /// The helper broke the protocol, or sent a block of the wrong size.
    Protocol(String),
    /// The helper exited unexpectedly, for example after a plugin crash.
    Exited {
        status: String,
        stderr: String,
    },
    Timeout(Phase),
    /// The plugin produced NaN or infinity at this frame of its output.
    NonFinite {
        frame: usize,
    },
    Cancelled,
}

/// A failed render. Nothing was produced, so the caller keeps whatever audio
/// it already had.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderError {
    /// Index into the chain; `None` before any stage started.
    pub stage: Option<usize>,
    /// The plugin of that stage, so a message can name what is unavailable.
    pub plugin: Option<Box<Plugin>>,
    pub kind: ErrorKind,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let (Some(stage), Some(plugin)) = (self.stage, &self.plugin) {
            write!(
                f,
                "Plugin stage {} ({} {}): ",
                stage + 1,
                plugin.backend,
                plugin.id
            )?;
        }
        match &self.kind {
            ErrorKind::Invalid(message) => write!(f, "invalid render: {message}"),
            ErrorKind::NoHelper(backend) => write!(f, "no {backend} helper is configured"),
            ErrorKind::Spawn(message) => write!(f, "could not start the helper: {message}"),
            ErrorKind::Unavailable { code, message } => {
                write!(f, "plugin unavailable ({code}): {message}")
            }
            ErrorKind::Unsupported(message) => write!(f, "cannot render offline: {message}"),
            ErrorKind::ArtifactChanged { expected, found } => write!(
                f,
                "a different plugin is installed (expected {} {} {}, found {} {} {})",
                expected.id,
                expected.version,
                expected.artifact,
                found.id,
                found.version,
                found.artifact
            ),
            ErrorKind::Failed { code, message } => write!(f, "plugin failed ({code}): {message}"),
            ErrorKind::Protocol(message) => write!(f, "helper protocol error: {message}"),
            ErrorKind::Exited { status, stderr } => {
                let first = stderr.lines().next().unwrap_or("no message");
                write!(f, "the plugin helper stopped ({status}): {first}")
            }
            ErrorKind::Timeout(phase) => write!(f, "timed out during {phase:?}"),
            ErrorKind::NonFinite { frame } => {
                write!(
                    f,
                    "the plugin produced a non-finite sample at frame {frame}"
                )
            }
            ErrorKind::Cancelled => f.write_str("cancelled"),
        }
    }
}
impl std::error::Error for RenderError {}

fn check_text(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_TEXT || value.chars().any(char::is_control) {
        return Err(format!(
            "{what} must be 1..={MAX_TEXT} bytes without control characters"
        ));
    }
    Ok(())
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        let source = &self.source;
        if !(1..=2).contains(&source.channels) {
            return Err(format!(
                "a source has 1 or 2 channels, not {}",
                source.channels
            ));
        }
        if !(MIN_RATE..=MAX_RATE).contains(&source.rate) {
            return Err(format!(
                "sample rate {} is outside {MIN_RATE}..={MAX_RATE}",
                source.rate
            ));
        }
        if !source
            .samples
            .len()
            .is_multiple_of(usize::from(source.channels))
        {
            return Err("source samples are not whole frames".into());
        }
        let frames = source.frames();
        if frames == 0 {
            return Err("the source is empty".into());
        }
        if frames > (MAX_SOURCE_SECONDS * source.rate) as usize {
            return Err(format!(
                "the source is longer than {MAX_SOURCE_SECONDS} seconds"
            ));
        }
        if let Some(index) = source.samples.iter().position(|s| !s.is_finite()) {
            return Err(format!("source sample {index} is not finite"));
        }
        let policy = &self.policy;
        if source.channels > policy.layout.channels() {
            return Err("a stereo source needs a stereo layout".into());
        }
        if !(MIN_BLOCK..=MAX_BLOCK).contains(&policy.block_frames) {
            return Err(format!(
                "block size {} is outside {MIN_BLOCK}..={MAX_BLOCK}",
                policy.block_frames
            ));
        }
        let tail = &policy.tail;
        if !(-200.0..=-20.0).contains(&tail.threshold_dbfs) {
            return Err("the tail threshold must be within -200..=-20 dBFS".into());
        }
        if tail.hold_frames == 0 {
            return Err("the tail hold must be at least one frame".into());
        }
        if tail.max_frames > MAX_TAIL_SECONDS * source.rate {
            return Err(format!("a tail is capped at {MAX_TAIL_SECONDS} seconds"));
        }
        if self.chain.is_empty() {
            return Err("an empty chain has nothing to render".into());
        }
        if self.chain.len() > MAX_STAGES {
            return Err(format!("a chain holds at most {MAX_STAGES} plugins"));
        }
        for (index, stage) in self.chain.iter().enumerate() {
            let at = |message: String| format!("stage {}: {message}", index + 1);
            check_text("a plugin ID", &stage.plugin.id).map_err(at)?;
            check_text("a plugin ABI", &stage.plugin.abi).map_err(at)?;
            check_text("a plugin version", &stage.plugin.version).map_err(at)?;
            let location = stage
                .location
                .to_str()
                .ok_or_else(|| at("the plugin location is not UTF-8".into()))?;
            check_text("a plugin location", location).map_err(at)?;
            if stage.params.len() > MAX_PARAMS {
                return Err(at(format!("at most {MAX_PARAMS} parameters")));
            }
            for pair in stage.params.windows(2) {
                if pair[0].id >= pair[1].id {
                    return Err(at("parameters must be strictly ascending by ID".into()));
                }
            }
            if let Some(param) = stage.params.iter().find(|p| !p.value.is_finite()) {
                return Err(at(format!("parameter {} is not finite", param.id)));
            }
            if stage
                .state
                .as_ref()
                .is_some_and(|s| s.len() > MAX_STATE_BYTES)
            {
                return Err(at(format!(
                    "plugin state is larger than {MAX_STATE_BYTES} bytes"
                )));
            }
        }
        Ok(())
    }

    /// The content identity of this render: equal requests give equal
    /// identities, and any change that can change the audio changes it.
    /// Plugin locations and time limits are deliberately excluded.
    pub fn identity(&self) -> Hash256 {
        #[derive(Serialize)]
        struct Doc<'a> {
            kind: &'static str,
            schema: u32,
            mode: &'static str,
            source: SourceDoc,
            policy: PolicyDoc,
            chain: Vec<StageDoc<'a>>,
        }
        #[derive(Serialize)]
        struct SourceDoc {
            channels: u16,
            rate: u32,
            frames: usize,
            samples: Hash256,
        }
        #[derive(Serialize)]
        struct PolicyDoc {
            layout: Layout,
            block_frames: u32,
            threshold_dbfs_bits: u64,
            hold_frames: u32,
            max_frames: u32,
            compensate_latency: bool,
        }
        #[derive(Serialize)]
        struct StageDoc<'a> {
            plugin: &'a Plugin,
            params: Vec<(u32, u64)>,
            state: Option<Hash256>,
        }
        let mut samples = Sha256::new();
        for value in self.source.samples.iter() {
            samples.update(value.to_bits().to_le_bytes());
        }
        let policy = &self.policy;
        let doc = Doc {
            kind: "brownnote-offline-plugin-render",
            schema: SCHEMA,
            mode: "offline",
            source: SourceDoc {
                channels: self.source.channels,
                rate: self.source.rate,
                frames: self.source.frames(),
                samples: Hash256(samples.finalize().into()),
            },
            policy: PolicyDoc {
                layout: policy.layout,
                block_frames: policy.block_frames,
                threshold_dbfs_bits: policy.tail.threshold_dbfs.to_bits(),
                hold_frames: policy.tail.hold_frames,
                max_frames: policy.tail.max_frames,
                compensate_latency: policy.compensate_latency,
            },
            chain: self
                .chain
                .iter()
                .map(|stage| StageDoc {
                    plugin: &stage.plugin,
                    params: stage
                        .params
                        .iter()
                        .map(|p| (p.id, p.value.to_bits()))
                        .collect(),
                    state: stage.state.as_deref().map(Hash256::of),
                })
                .collect(),
        };
        // Serializing plain structs of integers and strings cannot fail.
        let bytes = serde_json::to_vec(&doc).unwrap_or_default();
        Hash256::of(&bytes)
    }
}

impl Limits {
    fn validate(&self) -> Result<(), String> {
        if [self.open, self.block, self.close, self.total].contains(&Duration::ZERO) {
            return Err("time limits must be greater than zero".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
