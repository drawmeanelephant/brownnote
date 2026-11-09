//! Project-owned bytes. Cache keys and paths are deliberately not asset IDs.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AssetId([u8; 32]);

impl AssetId {
    /// Hash the exact encoded file bytes, not decoded or resampled audio.
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn relative_path(self) -> PathBuf {
        PathBuf::from("media").join(format!("{self}.wav"))
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
    type Err = MediaError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MediaError::MalformedId);
        }
        let mut digest = [0; 32];
        for (index, byte) in digest.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| MediaError::MalformedId)?;
        }
        Ok(Self(digest))
    }
}

impl TryFrom<String> for AssetId {
    type Error = MediaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRef {
    pub id: AssetId,
    pub bytes: u64,
}

impl AssetRef {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            id: AssetId::of(bytes),
            bytes: bytes.len() as u64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Source,
    /// Authoritative audible output, including pedal/plugin renders.
    RetainedRender,
    /// Exact SF2 bytes of the project instrument (D18). Never decoded as audio;
    /// stored under the same content-addressed media name as every asset.
    InstrumentSource,
    /// One frozen instrument note (stereo float WAV). Exported as retained
    /// audio, never loaded into a pad sample slot.
    InstrumentRender,
    /// Audio a live instrument played into a take (stereo float WAV, #112).
    /// Project media retained for replay and export, not a pad sample.
    LiveTakeRender,
}

impl MediaKind {
    /// Pad-playable WAV media that occupies an engine sample slot.
    pub fn is_pad_audio(self) -> bool {
        matches!(self, Self::Source | Self::RetainedRender)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub reference: AssetRef,
    pub kind: MediaKind,
}

/// Disposable cache entries are never serialized into a project document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisposableCache {
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaError {
    MalformedId,
    Missing(AssetId),
    EscapesProject(AssetId),
    NotFile(AssetId),
    SizeMismatch {
        id: AssetId,
        expected: u64,
        actual: u64,
    },
    HashMismatch {
        expected: AssetId,
        actual: AssetId,
    },
    Io {
        id: AssetId,
        kind: io::ErrorKind,
    },
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Diagnostics contain logical identities, never machine paths.
        write!(f, "project media: {self:?}")
    }
}
impl std::error::Error for MediaError {}

/// Read-only validation for #100. No fallback to source audio or render cache.
///
/// The returned path is valid at verification time; storage owns concurrent
/// mutation/transaction coordination and must not treat it as an open handle.
pub fn resolve_verified(root: &Path, reference: &AssetRef) -> Result<PathBuf, MediaError> {
    let id = reference.id;
    let error = |e: io::Error| match e.kind() {
        io::ErrorKind::NotFound => MediaError::Missing(id),
        kind => MediaError::Io { id, kind },
    };
    let root = root.canonicalize().map_err(error)?;
    let path = root
        .join(id.relative_path())
        .canonicalize()
        .map_err(error)?;
    if !path.starts_with(&root) {
        return Err(MediaError::EscapesProject(id));
    }
    let mut file = File::open(&path).map_err(error)?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() {
        return Err(MediaError::NotFile(id));
    }
    if metadata.len() != reference.bytes {
        return Err(MediaError::SizeMismatch {
            id,
            expected: reference.bytes,
            actual: metadata.len(),
        });
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(error)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let actual = AssetId(hash.finalize().into());
    if actual != id {
        return Err(MediaError::HashMismatch {
            expected: id,
            actual,
        });
    }
    Ok(path)
}

pub fn verify_all(root: &Path, assets: &[Asset]) -> Vec<MediaError> {
    assets
        .iter()
        .filter_map(|asset| resolve_verified(root, &asset.reference).err())
        .collect()
}
