//! Append-only media and complete document snapshots, then one atomic publish.
//! See `native/docs/project-store.md` for the commit and durability contract
//! and `native/docs/project-retention.md` for the retention and collection
//! policy. A save never deletes; `collect` is the only deleter.
use super::{AssetId, AssetRef, DOCUMENT_NAME, Document, DocumentError, media::MediaError};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    /// Size of a regular file, without following a symlink. Read-only.
    fn size(&mut self, path: &Path) -> io::Result<u64>;
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
    fn size(&mut self, path: &Path) -> io::Result<u64> {
        regular(path)?;
        Ok(fs::metadata(path)?.len())
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    /// A retention plan tried to name a path outside the managed names.
    UnsafeRetentionPath,
    /// A bounded collection could not meet its budget without deleting a
    /// protected generation, and the caller asked for refusal instead.
    RetentionBoundUnmet,
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

/// Default byte budget for superseded history. The current document and its
/// media closure are never charged to it. See `native/docs/project-retention.md`.
pub const DEFAULT_HISTORY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnconfirmedSnapshots {
    /// Legacy and abandoned snapshots stay for manual investigation.
    #[default]
    Keep,
    /// An explicit, recorded decision permits their removal.
    Remove,
}

/// The bound and legacy decision used by a plan or a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Bytes of snapshot JSON plus media referenced only by superseded
    /// generations. `u64::MAX` disables the bound.
    pub history_bytes: u64,
    pub unconfirmed: UnconfirmedSnapshots,
    /// Return `RetentionBoundUnmet` instead of reporting overage when the
    /// bound cannot be met without removing a protected generation.
    pub require_bound: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            history_bytes: DEFAULT_HISTORY_BYTES,
            unconfirmed: UnconfirmedSnapshots::Keep,
            require_bound: false,
        }
    }
}

impl RetentionPolicy {
    /// Retain all valid history; only stale evidence and unreferenced media
    /// remain collectable.
    pub const fn unbounded() -> Self {
        Self {
            history_bytes: u64::MAX,
            unconfirmed: UnconfirmedSnapshots::Keep,
            require_bound: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RootUsage {
    pub snapshots: u64,
    pub snapshot_bytes: u64,
    pub evidence: u64,
    pub evidence_bytes: u64,
    pub media: u64,
    pub media_bytes: u64,
    /// Recognized directories containing entries this plan never removes.
    pub unknown: u64,
    pub unknown_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionPlan {
    pub usage: RootUsage,
    /// The current document plus every generation this policy retains.
    pub retained: Vec<GenerationId>,
    /// Evidence is always removed before the snapshot it belongs to.
    pub evidence: Vec<Removal>,
    pub snapshots: Vec<Removal>,
    /// Removed last, only after no retained generation references the object.
    pub media: Vec<Removal>,
    /// Reported, never removed: names outside the managed layout.
    pub unknown: Vec<PathBuf>,
    /// Bytes of superseded history retained beyond the current document.
    pub history_bytes: u64,
    /// Managed bytes this plan keeps (unknown entries are excluded).
    pub protected_bytes: u64,
    pub reclaimable_bytes: u64,
    pub over_budget: bool,
    /// Media collection was skipped because a retained document's closure is
    /// unknown, so no byte can be proved unreferenced.
    pub media_refused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionReceipt {
    pub removed_evidence: u64,
    pub removed_snapshots: u64,
    pub removed_media: u64,
    pub bytes: u64,
    pub over_budget: bool,
    pub media_refused: bool,
    pub remaining: RootUsage,
}

#[derive(Default)]
struct Inventory {
    usage: RootUsage,
    unknown: Vec<PathBuf>,
    snapshots: BTreeMap<GenerationId, u64>,
    evidence: BTreeMap<GenerationId, u64>,
    media: BTreeMap<AssetId, u64>,
}

#[derive(Clone, Copy)]
enum Managed {
    Evidence,
    Snapshot,
    Media,
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

    /// Read-only. Repeated calls leave the directory byte-for-byte unchanged.
    /// The current document, every generation this policy retains, their media
    /// closures, and unknown entries are never listed for removal.
    pub fn retention_plan(
        &mut self,
        policy: &RetentionPolicy,
    ) -> Result<RetentionPlan, StoreError> {
        self.fs.check_dir(&self.root)?;
        let inventory = self.inventory()?;
        let unbounded = RetentionPolicy {
            history_bytes: u64::MAX,
            ..*policy
        };
        let all = self.recovery_options_with(&unbounded)?;
        let bounded = self.recovery_options_with(policy)?;

        let (current_id, current_inspection, current_unknown) = match &bounded.current {
            Ok(Some(id)) => match self.inspect_committed()? {
                Some(inspection) if inspection.generation == *id => {
                    (Some(*id), Some(inspection), false)
                }
                _ => (None, None, true),
            },
            Ok(None) => (None, None, false),
            Err(_) => (None, None, true),
        };
        let mut pruned: Vec<GenerationId> = if current_unknown {
            Vec::new()
        } else {
            let retained: BTreeSet<_> = bounded.recoverable.iter().copied().collect();
            all.recoverable
                .iter()
                .copied()
                .filter(|id| !retained.contains(id))
                .collect()
        };
        pruned.sort_by_key(ToString::to_string);
        pruned.dedup();

        let remove_unconfirmed: BTreeSet<GenerationId> =
            if policy.unconfirmed == UnconfirmedSnapshots::Remove {
                all.unconfirmed.iter().copied().collect()
            } else {
                BTreeSet::new()
            };

        let mut media_known = !current_unknown;
        let mut protected = BTreeSet::new();
        let mut history_media = BTreeSet::new();
        let mut retained = BTreeSet::new();
        let mut history_snapshot_bytes = 0u64;
        if let Some(id) = current_id {
            retained.insert(id);
        }
        if let Some(inspection) = &current_inspection {
            protected.extend(
                inspection
                    .document
                    .assets
                    .iter()
                    .map(|asset| asset.reference.id),
            );
        }
        for &id in &bounded.recoverable {
            if Some(id) == current_id {
                continue;
            }
            let inspection = self.inspect_snapshot(id)?;
            retained.insert(id);
            history_snapshot_bytes = history_snapshot_bytes
                .saturating_add(inventory.snapshots.get(&id).copied().unwrap_or(0));
            for asset in &inspection.document.assets {
                let asset = asset.reference.id;
                if protected.insert(asset) {
                    history_media.insert(asset);
                }
            }
        }
        for &id in &all.unconfirmed {
            if remove_unconfirmed.contains(&id) {
                continue;
            }
            match inventory.snapshots.get(&id) {
                Some(bytes) => {
                    retained.insert(id);
                    history_snapshot_bytes = history_snapshot_bytes.saturating_add(*bytes);
                    self.protect_snapshot_assets(
                        id,
                        &inventory,
                        &mut protected,
                        &mut history_media,
                        &mut media_known,
                    )?;
                }
                None => media_known = false,
            }
        }
        let mut stale_evidence = Vec::new();
        for (id, _) in &all.rejected {
            match inventory.snapshots.get(id) {
                Some(bytes) => {
                    retained.insert(*id);
                    history_snapshot_bytes = history_snapshot_bytes.saturating_add(*bytes);
                    self.protect_snapshot_assets(
                        *id,
                        &inventory,
                        &mut protected,
                        &mut history_media,
                        &mut media_known,
                    )?;
                }
                None => {
                    let path = self.root.join(id.relative_path());
                    if inventory.unknown.iter().any(|unknown| unknown == &path) {
                        media_known = false;
                    } else {
                        stale_evidence.push(*id);
                    }
                }
            }
        }
        let history_media_bytes = history_media.iter().fold(0u64, |total, id| {
            total.saturating_add(inventory.media.get(id).copied().unwrap_or(0))
        });

        let mut evidence = Vec::new();
        for id in pruned.iter().chain(stale_evidence.iter()) {
            if let Some(bytes) = inventory.evidence.get(id) {
                evidence.push(Removal {
                    path: self.root.join(id.evidence_path()),
                    bytes: *bytes,
                });
            }
        }
        evidence.sort_by(|a, b| a.path.cmp(&b.path));
        evidence.dedup_by(|a, b| a.path == b.path);

        let mut snapshots = Vec::new();
        for id in pruned.iter().chain(remove_unconfirmed.iter()) {
            if let Some(bytes) = inventory.snapshots.get(id) {
                snapshots.push(Removal {
                    path: self.root.join(id.relative_path()),
                    bytes: *bytes,
                });
            }
        }
        snapshots.sort_by(|a, b| a.path.cmp(&b.path));
        snapshots.dedup_by(|a, b| a.path == b.path);

        let mut media = Vec::new();
        if media_known {
            for (id, bytes) in &inventory.media {
                if !protected.contains(id) {
                    media.push(Removal {
                        path: self.root.join(id.relative_path()),
                        bytes: *bytes,
                    });
                }
            }
        }

        let reclaimable_bytes = evidence
            .iter()
            .chain(snapshots.iter())
            .chain(media.iter())
            .fold(0u64, |total, removal| total.saturating_add(removal.bytes));
        let managed_bytes = inventory
            .usage
            .snapshot_bytes
            .saturating_add(inventory.usage.evidence_bytes)
            .saturating_add(inventory.usage.media_bytes);
        let history_bytes = history_snapshot_bytes.saturating_add(history_media_bytes);
        let over_budget = policy.history_bytes != u64::MAX
            && (!media_known || history_bytes > policy.history_bytes);
        let mut retained: Vec<_> = retained.into_iter().collect();
        retained.sort_by_key(ToString::to_string);
        Ok(RetentionPlan {
            usage: inventory.usage,
            retained,
            evidence,
            snapshots,
            media,
            unknown: inventory.unknown,
            history_bytes,
            protected_bytes: managed_bytes.saturating_sub(reclaimable_bytes),
            reclaimable_bytes,
            over_budget,
            media_refused: !media_known,
        })
    }

    /// Apply a freshly recomputed plan under the same exclusive writer lock a
    /// save uses. Evidence is removed before its snapshot; media is removed
    /// last, after no retained generation references it.
    pub fn collect(&mut self, policy: &RetentionPolicy) -> Result<CollectionReceipt, StoreError> {
        if !self.root.is_absolute() || self.root.parent().is_none() {
            return Err(StoreError::InvalidRoot);
        }
        self.fs.check_dir(&self.root)?;
        self.fs.ensure_dir(&self.private())?;
        let _lock = self.fs.lock(&self.private().join("lock"))?;
        let plan = self.retention_plan(policy)?;
        if policy.require_bound && plan.over_budget {
            return Err(StoreError::RetentionBoundUnmet);
        }
        for removal in &plan.evidence {
            self.verify_removal(removal, Managed::Evidence)?;
        }
        for removal in &plan.snapshots {
            self.verify_removal(removal, Managed::Snapshot)?;
        }
        for removal in &plan.media {
            self.verify_removal(removal, Managed::Media)?;
        }

        let mut removed_evidence = 0u64;
        let mut removed_snapshots = 0u64;
        let mut removed_media = 0u64;
        let mut bytes = 0u64;
        for removal in &plan.evidence {
            if self.remove_present(&removal.path)? {
                removed_evidence = removed_evidence.saturating_add(1);
                bytes = bytes.saturating_add(removal.bytes);
            }
        }
        self.sync_managed(&self.private().join("committed"))?;
        for removal in &plan.snapshots {
            if self.remove_present(&removal.path)? {
                removed_snapshots = removed_snapshots.saturating_add(1);
                bytes = bytes.saturating_add(removal.bytes);
            }
        }
        self.sync_managed(&self.private().join("versions"))?;
        for removal in &plan.media {
            if self.remove_present(&removal.path)? {
                removed_media = removed_media.saturating_add(1);
                bytes = bytes.saturating_add(removal.bytes);
            }
        }
        self.sync_managed(&self.root.join("media"))?;
        Ok(CollectionReceipt {
            removed_evidence,
            removed_snapshots,
            removed_media,
            bytes,
            over_budget: plan.over_budget,
            media_refused: plan.media_refused,
            remaining: self.inventory()?.usage,
        })
    }

    fn protect_snapshot_assets(
        &mut self,
        id: GenerationId,
        inventory: &Inventory,
        protected: &mut BTreeSet<AssetId>,
        history_media: &mut BTreeSet<AssetId>,
        media_known: &mut bool,
    ) -> Result<(), StoreError> {
        match self.snapshot_assets(id, inventory)? {
            Some(assets) => {
                for asset in assets {
                    if protected.insert(asset) {
                        history_media.insert(asset);
                    }
                }
            }
            None => *media_known = false,
        }
        Ok(())
    }

    fn snapshot_assets(
        &mut self,
        id: GenerationId,
        inventory: &Inventory,
    ) -> Result<Option<Vec<AssetId>>, StoreError> {
        if !inventory.snapshots.contains_key(&id) {
            return Ok(Some(Vec::new()));
        }
        let bytes = match self.fs.read(&self.root.join(id.relative_path())) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
            Err(e) => return Err(e.into()),
        };
        Ok(Document::decode(&bytes).ok().map(|document| {
            document
                .assets
                .iter()
                .map(|asset| asset.reference.id)
                .collect()
        }))
    }

    fn inventory(&mut self) -> Result<Inventory, StoreError> {
        self.fs.check_dir(&self.root)?;
        let mut inventory = Inventory::default();
        for path in self.fs.list(&self.root)? {
            if path.parent() != Some(self.root.as_path()) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                self.unknown_entry(path, &mut inventory);
                continue;
            };
            match name {
                DOCUMENT_NAME => match self.fs.size(&path) {
                    Ok(_) => {}
                    Err(_) => self.unknown_entry(path, &mut inventory),
                },
                "media" => match self.fs.check_dir(&path) {
                    Ok(()) => self.scan_media(&path, &mut inventory)?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => self.unknown_entry(path, &mut inventory),
                },
                PRIVATE => match self.fs.check_dir(&path) {
                    Ok(()) => self.scan_private(&path, &mut inventory)?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => self.unknown_entry(path, &mut inventory),
                },
                _ => self.unknown_entry(path, &mut inventory),
            }
        }
        Ok(inventory)
    }

    fn scan_private(
        &mut self,
        directory: &Path,
        inventory: &mut Inventory,
    ) -> Result<(), StoreError> {
        for path in self.fs.list(directory)? {
            if path.parent() != Some(directory) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                self.unknown_entry(path, inventory);
                continue;
            };
            match name {
                "versions" => match self.fs.check_dir(&path) {
                    Ok(()) => self.scan_versions(&path, inventory)?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => self.unknown_entry(path, inventory),
                },
                "committed" => match self.fs.check_dir(&path) {
                    Ok(()) => self.scan_evidence(&path, inventory)?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => self.unknown_entry(path, inventory),
                },
                "lock" | "staging" => {}
                _ => self.unknown_entry(path, inventory),
            }
        }
        Ok(())
    }

    fn scan_versions(
        &mut self,
        directory: &Path,
        inventory: &mut Inventory,
    ) -> Result<(), StoreError> {
        for path in self.fs.list(directory)? {
            if path.parent() != Some(directory) {
                continue;
            }
            let id = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(snapshot_name);
            match (id, self.fs.size(&path)) {
                (Some(id), Ok(bytes)) => {
                    inventory.snapshots.insert(id, bytes);
                    inventory.usage.snapshots = inventory.usage.snapshots.saturating_add(1);
                    inventory.usage.snapshot_bytes =
                        inventory.usage.snapshot_bytes.saturating_add(bytes);
                }
                _ => self.unknown_entry(path, inventory),
            }
        }
        Ok(())
    }

    fn scan_evidence(
        &mut self,
        directory: &Path,
        inventory: &mut Inventory,
    ) -> Result<(), StoreError> {
        for path in self.fs.list(directory)? {
            if path.parent() != Some(directory) {
                continue;
            }
            let id = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(evidence_name);
            match (id, self.fs.size(&path)) {
                (Some(id), Ok(bytes)) => {
                    inventory.evidence.insert(id, bytes);
                    inventory.usage.evidence = inventory.usage.evidence.saturating_add(1);
                    inventory.usage.evidence_bytes =
                        inventory.usage.evidence_bytes.saturating_add(bytes);
                }
                _ => self.unknown_entry(path, inventory),
            }
        }
        Ok(())
    }

    fn scan_media(
        &mut self,
        directory: &Path,
        inventory: &mut Inventory,
    ) -> Result<(), StoreError> {
        for path in self.fs.list(directory)? {
            if path.parent() != Some(directory) {
                continue;
            }
            let id = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(media_name);
            match (id, self.fs.size(&path)) {
                (Some(id), Ok(bytes)) => {
                    inventory.media.insert(id, bytes);
                    inventory.usage.media = inventory.usage.media.saturating_add(1);
                    inventory.usage.media_bytes = inventory.usage.media_bytes.saturating_add(bytes);
                }
                _ => self.unknown_entry(path, inventory),
            }
        }
        Ok(())
    }

    fn unknown_entry(&mut self, path: PathBuf, inventory: &mut Inventory) {
        let bytes = self.fs.size(&path).unwrap_or(0);
        inventory.usage.unknown = inventory.usage.unknown.saturating_add(1);
        inventory.usage.unknown_bytes = inventory.usage.unknown_bytes.saturating_add(bytes);
        inventory.unknown.push(path);
    }

    fn verify_removal(&mut self, removal: &Removal, kind: Managed) -> Result<(), StoreError> {
        let expected_parent = match kind {
            Managed::Evidence => self.private().join("committed"),
            Managed::Snapshot => self.private().join("versions"),
            Managed::Media => self.root.join("media"),
        };
        if removal.path.parent() != Some(expected_parent.as_path()) {
            return Err(StoreError::UnsafeRetentionPath);
        }
        let name = removal
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(StoreError::UnsafeRetentionPath)?;
        let named = match kind {
            Managed::Evidence => evidence_name(name).is_some(),
            Managed::Snapshot => snapshot_name(name).is_some(),
            Managed::Media => media_name(name).is_some(),
        };
        if !named {
            return Err(StoreError::UnsafeRetentionPath);
        }
        match self.fs.size(&removal.path) {
            Ok(bytes) if bytes == removal.bytes => Ok(()),
            Ok(_) => Err(StoreError::UnsafeRetentionPath),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn remove_present(&mut self, path: &Path) -> Result<bool, StoreError> {
        match self.fs.remove(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn sync_managed(&mut self, path: &Path) -> Result<(), StoreError> {
        match self.fs.check_dir(path) {
            Ok(()) => self.fs.sync_dir(path).map_err(Into::into),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Deterministic bounded subset of recoverable generations. Identity order
    /// is not chronology; the current document and any missing/invalid current
    /// keep the full set. No caller state is changed.
    pub(super) fn retain_history(
        &mut self,
        current: Option<GenerationId>,
        recoverable: &[GenerationId],
        policy: &RetentionPolicy,
    ) -> Result<Vec<GenerationId>, StoreError> {
        let Some(current) = current else {
            return Ok(recoverable.to_vec());
        };
        if policy.history_bytes == u64::MAX {
            return Ok(recoverable.to_vec());
        }
        let inspection = match self.inspect_committed()? {
            Some(inspection) if inspection.generation == current => inspection,
            _ => return Ok(recoverable.to_vec()),
        };
        let mut protected: BTreeSet<AssetId> = inspection
            .document
            .assets
            .iter()
            .map(|asset| asset.reference.id)
            .collect();
        let mut used = 0u64;
        let mut retained = Vec::new();
        for &id in recoverable {
            if id == current {
                retained.push(id);
                continue;
            }
            let inspection = self.inspect_snapshot(id)?;
            let snapshot_bytes = self.fs.size(&self.root.join(id.relative_path()))?;
            let mut extra = 0u64;
            let mut candidate = BTreeSet::new();
            for asset in &inspection.document.assets {
                let asset = asset.reference.id;
                if !protected.contains(&asset) && candidate.insert(asset) {
                    extra =
                        extra.saturating_add(self.fs.size(&self.root.join(asset.relative_path()))?);
                }
            }
            if used.saturating_add(snapshot_bytes).saturating_add(extra) <= policy.history_bytes {
                used = used.saturating_add(snapshot_bytes).saturating_add(extra);
                protected.extend(candidate);
                retained.push(id);
            }
        }
        Ok(retained)
    }
}

fn snapshot_name(name: &str) -> Option<GenerationId> {
    name.strip_suffix(".json").and_then(GenerationId::from_name)
}

fn evidence_name(name: &str) -> Option<GenerationId> {
    GenerationId::from_name(name)
}

fn media_name(name: &str) -> Option<AssetId> {
    name.strip_suffix(".wav")
        .and_then(|stem| stem.parse::<AssetId>().ok())
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
