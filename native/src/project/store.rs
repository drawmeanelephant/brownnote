//! Append-only media and complete document snapshots, then one atomic publish.
//! See `native/docs/project-store.md` for the commit and durability contract.
use super::{AssetId, AssetRef, DOCUMENT_NAME, Document, DocumentError, media::MediaError};
use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

const PRIVATE: &str = ".brownnote-store";

/// Filesystem operations are separate so faults can surround every boundary.
/// `publish` must atomically replace its destination on the same filesystem.
/// Any mutating operation may return an error *after* taking effect.
pub trait FileSystem {
    type Writer;
    type Lock;
    fn ensure_dir(&mut self, path: &Path) -> io::Result<()>;
    fn check_dir(&mut self, path: &Path) -> io::Result<()>;
    fn lock(&mut self, path: &Path) -> io::Result<Self::Lock>;
    fn list(&mut self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn read(&mut self, path: &Path) -> io::Result<Vec<u8>>;
    fn create(&mut self, path: &Path) -> io::Result<Self::Writer>;
    fn write(&mut self, file: &mut Self::Writer, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self, file: &mut Self::Writer) -> io::Result<()>;
    fn sync(&mut self, file: &Self::Writer) -> io::Result<()>;
    fn sync_existing(&mut self, path: &Path) -> io::Result<()>;
    fn sync_dir(&mut self, path: &Path) -> io::Result<()>;
    /// Atomic no-replace publication; AlreadyExists must leave bytes untouched.
    fn link(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn publish(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove(&mut self, path: &Path) -> io::Result<()>;
}

#[derive(Default)]
pub struct SystemFs;

fn regular(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_file() {
        Ok(())
    } else {
        Err(io::ErrorKind::InvalidInput.into())
    }
}

impl FileSystem for SystemFs {
    type Writer = File;
    type Lock = File;

    fn ensure_dir(&mut self, path: &Path) -> io::Result<()> {
        match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => self.check_dir(path),
            Err(e) => Err(e),
        }
    }
    fn check_dir(&mut self, path: &Path) -> io::Result<()> {
        if fs::symlink_metadata(path)?.is_dir() {
            Ok(())
        } else {
            Err(io::ErrorKind::InvalidInput.into())
        }
    }
    fn lock(&mut self, path: &Path) -> io::Result<File> {
        match fs::symlink_metadata(path) {
            Ok(_) => regular(path)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        Ok(file)
    }
    fn list(&mut self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        Ok(entries)
    }
    fn read(&mut self, path: &Path) -> io::Result<Vec<u8>> {
        regular(path)?;
        fs::read(path)
    }
    fn create(&mut self, path: &Path) -> io::Result<File> {
        OpenOptions::new().write(true).create_new(true).open(path)
    }
    fn write(&mut self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        file.write_all(bytes)
    }
    fn flush(&mut self, file: &mut File) -> io::Result<()> {
        file.flush()
    }
    fn sync(&mut self, file: &File) -> io::Result<()> {
        file.sync_all()
    }
    fn sync_existing(&mut self, path: &Path) -> io::Result<()> {
        regular(path)?;
        File::open(path)?.sync_all()
    }
    fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        self.check_dir(path)?;
        File::open(path)?.sync_all()
    }
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
    fn link(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::hard_link(from, to)
    }
    fn publish(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
    fn remove(&mut self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }
}

/// Document-byte identity, distinct from a media identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationId(AssetId);

impl GenerationId {
    pub fn relative_path(self) -> PathBuf {
        PathBuf::from(PRIVATE)
            .join("versions")
            .join(format!("{}.json", self.0))
    }

    pub(super) fn from_name(name: &str) -> Option<Self> {
        name.parse::<AssetId>().ok().map(Self)
    }

    pub(super) fn evidence_path(self) -> PathBuf {
        PathBuf::from(PRIVATE)
            .join("committed")
            .join(self.to_string())
    }
}

impl fmt::Display for GenerationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An explicit source is copied, never linked. Omit it to reuse project media.
#[derive(Clone, Debug)]
pub struct MediaSource {
    pub id: AssetId,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inspection {
    pub document: Document,
    pub generation: GenerationId,
    /// Exact committed bytes, not a re-encoding of the document.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Io(io::ErrorKind),
    Document(DocumentError),
    Media(MediaError),
    InvalidSources,
    SnapshotMismatch(GenerationId),
    InvalidCommitEvidence(GenerationId),
    UncommittedGeneration(GenerationId),
    NoCommittedProject,
    InvalidRoot,
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}
impl From<DocumentError> for StoreError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "project store: {self:?}")
    }
}
impl std::error::Error for StoreError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitState {
    /// No publish was attempted. The committed document was not changed.
    Unchanged,
    /// Publish returned an error; it may have taken effect. Inspect to decide.
    Unknown(GenerationId),
    /// Publish succeeded, but postcommit sync or evidence recording failed.
    PublishedNotDurable(GenerationId),
}

#[derive(Debug)]
pub struct SaveFailure {
    pub commit: CommitState,
    /// Boxed so every save `Result` stays small; the commit state is the
    /// actionable part and this is the detailed cause.
    pub error: Box<StoreError>,
}
impl fmt::Display for SaveFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({:?})", self.error, self.commit)
    }
}
impl std::error::Error for SaveFailure {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReceipt {
    pub generation: GenerationId,
    pub previous: Option<GenerationId>,
}

/// One store per project is usual; an OS file lock also excludes other writers.
/// The root's parent must already exist. No UI state is mutated by this API.
pub struct Store<F = SystemFs> {
    pub(super) root: PathBuf,
    pub(super) fs: F,
}

impl Store<SystemFs> {
    pub fn new(root: PathBuf) -> Self {
        Self::with_fs(root, SystemFs)
    }
}

impl<F: FileSystem> Store<F> {
    pub fn with_fs(root: PathBuf, fs: F) -> Self {
        Self { root, fs }
    }

    pub fn into_fs(self) -> F {
        self.fs
    }

    /// Read-only, fully validated current document and media closure.
    /// Missing project.json is the only condition interpreted as an empty store.
    pub fn inspect_committed(&mut self) -> Result<Option<Inspection>, StoreError> {
        self.fs.check_dir(&self.root)?;
        let bytes = match self.fs.read(&self.root.join(DOCUMENT_NAME)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        self.inspect_bytes(bytes).map(Some)
    }

    /// A complete immutable snapshot, not evidence that it was ever committed.
    /// Use `open(OpenChoice::Committed(id))` for explicit evidenced recovery.
    pub fn inspect_snapshot(&mut self, id: GenerationId) -> Result<Inspection, StoreError> {
        self.fs.check_dir(&self.root)?;
        self.fs.check_dir(&self.private())?;
        self.fs.check_dir(&self.private().join("versions"))?;
        let bytes = self.fs.read(&self.root.join(id.relative_path()))?;
        if GenerationId(AssetId::of(&bytes)) != id {
            return Err(StoreError::SnapshotMismatch(id));
        }
        self.inspect_bytes(bytes)
    }

    fn inspect_bytes(&mut self, bytes: Vec<u8>) -> Result<Inspection, StoreError> {
        let document = Document::decode(&bytes)?;
        self.fs.check_dir(&self.root.join("media"))?;
        for asset in &document.assets {
            self.read_media(&asset.reference)?;
        }
        Ok(Inspection {
            generation: GenerationId(AssetId::of(&bytes)),
            document,
            bytes,
        })
    }

    pub(super) fn read_media(&mut self, reference: &AssetRef) -> Result<Vec<u8>, StoreError> {
        let id = reference.id;
        let bytes = self
            .fs
            .read(&self.root.join(id.relative_path()))
            .map_err(|e| {
                StoreError::Media(match e.kind() {
                    io::ErrorKind::NotFound => MediaError::Missing(id),
                    kind => MediaError::Io { id, kind },
                })
            })?;
        verify(reference, &bytes)?;
        Ok(bytes)
    }

    pub fn save(
        &mut self,
        document: &Document,
        sources: &[MediaSource],
    ) -> Result<SaveReceipt, SaveFailure> {
        let mut commit = CommitState::Unchanged;
        self.save_inner(document, sources, &mut commit)
            .map_err(|error| SaveFailure {
                commit,
                error: Box::new(error),
            })
    }

    pub(super) fn private(&self) -> PathBuf {
        self.root.join(PRIVATE)
    }

    fn stage(&self, name: &str) -> PathBuf {
        self.private().join("staging").join(name)
    }

    fn save_inner(
        &mut self,
        document: &Document,
        sources: &[MediaSource],
        commit: &mut CommitState,
    ) -> Result<SaveReceipt, StoreError> {
        let bytes = document.encode()?;
        let generation = GenerationId(AssetId::of(&bytes));
        let mut ids = BTreeSet::new();
        if sources.iter().any(|source| {
            !ids.insert(source.id) || !document.assets.iter().any(|a| a.reference.id == source.id)
        }) {
            return Err(StoreError::InvalidSources);
        }
        if !self.root.is_absolute() || self.root.parent().is_none() {
            return Err(StoreError::InvalidRoot);
        }
        self.fs.ensure_dir(&self.root)?;
        self.fs.ensure_dir(&self.private())?;
        let _lock = self.fs.lock(&self.private().join("lock"))?;
        for dir in [
            self.root.join("media"),
            self.private().join("versions"),
            self.private().join("committed"),
            self.private().join("staging"),
        ] {
            self.fs.ensure_dir(&dir)?;
        }
        // Stabilize the directory hierarchy, including a newly created root.
        self.fs.sync_dir(&self.private())?;
        self.fs.sync_dir(&self.root)?;
        let parent = self.root.parent().ok_or(StoreError::InvalidRoot)?;
        self.fs.sync_dir(parent)?;
        self.cleanup_staging()?;
        let previous = self.inspect_committed()?;
        if let Some(old) = &previous {
            // Adopt a valid contract-only project without changing its bytes.
            for asset in &old.document.assets {
                self.fs
                    .sync_existing(&self.root.join(asset.reference.id.relative_path()))?;
            }
            self.fs.sync_dir(&self.root.join("media"))?;
            self.snapshot(&old.bytes, old.generation)?;
            // Only an observed current document, never a candidate snapshot.
            self.record_committed(old.generation)?;
        }

        // Read and validate *all* sources into owned staging before publishing
        // any media. A vanished source can never become a saved dependency.
        for asset in &document.assets {
            let id = asset.reference.id;
            let path = sources
                .iter()
                .find(|source| source.id == id)
                .map(|source| source.path.clone())
                .unwrap_or_else(|| self.root.join(id.relative_path()));
            let media = self.fs.read(&path)?;
            verify(&asset.reference, &media)?;
            self.write_synced(&self.stage(&format!("media-{id}.tmp")), &media)?;
        }
        self.write_synced(&self.stage("document.tmp"), &bytes)?;
        self.fs.sync_dir(&self.private().join("staging"))?;

        for asset in &document.assets {
            let id = asset.reference.id;
            let destination = self.root.join(id.relative_path());
            let staged = self.stage(&format!("media-{id}.tmp"));
            match self.fs.read(&destination) {
                Ok(existing) => {
                    verify(&asset.reference, &existing)?;
                    self.fs.sync_existing(&destination)?;
                    self.fs.remove(&staged)?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match self.fs.link(&staged, &destination) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                            // An immutable importer may publish concurrently.
                            let existing = self.fs.read(&destination)?;
                            verify(&asset.reference, &existing)?;
                            self.fs.sync_existing(&destination)?;
                        }
                        Err(e) => return Err(e.into()),
                    }
                    self.fs.remove(&staged)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        self.fs.sync_dir(&self.root.join("media"))?;
        self.snapshot(&bytes, generation)?;
        self.fs.sync_dir(&self.private().join("staging"))?;
        self.fs.sync_dir(&self.private())?;
        self.fs.sync_dir(&self.root)?;

        // The only commit point. Error does not imply that rename did not run.
        *commit = CommitState::Unknown(generation);
        self.fs
            .publish(&self.stage("document.tmp"), &self.root.join(DOCUMENT_NAME))?;
        *commit = CommitState::PublishedNotDurable(generation);
        self.fs.sync_dir(&self.private().join("staging"))?;
        self.fs.sync_dir(&self.root)?;
        self.record_committed(generation)?;
        Ok(SaveReceipt {
            generation,
            previous: previous.map(|old| old.generation),
        })
    }

    fn record_committed(&mut self, id: GenerationId) -> Result<(), StoreError> {
        let destination = self.root.join(id.evidence_path());
        match self.fs.read(&destination) {
            Ok(bytes) if bytes.is_empty() => self.fs.sync_existing(&destination)?,
            Ok(_) => return Err(StoreError::InvalidCommitEvidence(id)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let staged = self.stage("commit.tmp");
                self.write_synced(&staged, &[])?;
                self.fs.link(&staged, &destination)?;
                self.fs.remove(&staged)?;
            }
            Err(e) => return Err(e.into()),
        }
        self.fs.sync_dir(&self.private().join("committed"))?;
        self.fs.sync_dir(&self.private().join("staging"))?;
        Ok(())
    }

    fn write_synced(&mut self, path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
        let mut file = self.fs.create(path)?;
        self.fs.write(&mut file, bytes)?;
        self.fs.flush(&mut file)?;
        self.fs.sync(&file)?;
        Ok(())
    }

    fn snapshot(&mut self, bytes: &[u8], id: GenerationId) -> Result<(), StoreError> {
        let destination = self.root.join(id.relative_path());
        match self.fs.read(&destination) {
            Ok(existing) => {
                if existing != bytes {
                    return Err(StoreError::SnapshotMismatch(id));
                }
                self.fs.sync_existing(&destination)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let staged = self.stage("version.tmp");
                self.write_synced(&staged, bytes)?;
                self.fs.rename(&staged, &destination)?;
            }
            Err(e) => return Err(e.into()),
        }
        self.fs.sync_dir(&self.private().join("versions"))?;
        Ok(())
    }

    fn cleanup_staging(&mut self) -> Result<(), StoreError> {
        let directory = self.private().join("staging");
        for path in self.fs.list(&directory)? {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let owned = name == "document.tmp"
                || name == "version.tmp"
                || name == "commit.tmp"
                || name
                    .strip_prefix("media-")
                    .and_then(|name| name.strip_suffix(".tmp"))
                    .is_some_and(|hash| hash.parse::<AssetId>().is_ok());
            // Never recurse or delete media, versions, lock, or unknown entries.
            if path.parent() == Some(directory.as_path()) && owned {
                self.fs.remove(&path)?;
            }
        }
        self.fs.sync_dir(&directory)?;
        Ok(())
    }
}

fn verify(reference: &AssetRef, bytes: &[u8]) -> Result<(), StoreError> {
    if bytes.len() as u64 != reference.bytes {
        return Err(StoreError::Media(MediaError::SizeMismatch {
            id: reference.id,
            expected: reference.bytes,
            actual: bytes.len() as u64,
        }));
    }
    let actual = AssetId::of(bytes);
    if actual != reference.id {
        return Err(StoreError::Media(MediaError::HashMismatch {
            expected: reference.id,
            actual,
        }));
    }
    Ok(())
}
