//! Read-only open preparation and explicit, evidenced recovery.
//! No caller state or on-disk document is changed, including on success.
//! The listing's recovered set uses the same retention bound as collection;
//! see `native/docs/project-retention.md`.
use super::{
    AssetId, Document,
    store::{FileSystem, GenerationId, Inspection, RetentionPolicy, Store, StoreError},
};
use std::{collections::BTreeMap, io, path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenChoice {
    Current,
    /// Only generations witnessed as current are eligible, not staged candidates.
    Committed(GenerationId),
}

/// Owned integrity-validated input for the next runtime preparation step.
/// Decode required audio and admit the whole replacement before swapping UI
/// state. No paths need to be reopened, and retained audio must not be rebuilt.
#[derive(Clone, Debug)]
pub struct PreparedProject {
    inspection: Inspection,
    media: BTreeMap<AssetId, Arc<[u8]>>,
}

impl PreparedProject {
    pub fn document(&self) -> &Document {
        &self.inspection.document
    }

    pub fn inspection(&self) -> &Inspection {
        &self.inspection
    }

    pub fn media(&self) -> &BTreeMap<AssetId, Arc<[u8]>> {
        &self.media
    }

    pub fn into_parts(self) -> (Inspection, BTreeMap<AssetId, Arc<[u8]>>) {
        (self.inspection, self.media)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryOptions {
    pub current: Result<Option<GenerationId>, StoreError>,
    /// Complete, integrity-validated historical choices, sorted by identity.
    /// Identity order is not time order; none is an automatic recommendation.
    pub recoverable: Vec<GenerationId>,
    pub rejected: Vec<(GenerationId, StoreError)>,
    /// Snapshot names without commit evidence, including legacy-store snapshots.
    /// Never offered as previously saved projects.
    pub unconfirmed: Vec<GenerationId>,
}

impl<F: FileSystem> Store<F> {
    pub fn open(&mut self, choice: OpenChoice) -> Result<PreparedProject, StoreError> {
        let inspection = match choice {
            OpenChoice::Current => self
                .inspect_committed()?
                .ok_or(StoreError::NoCommittedProject)?,
            OpenChoice::Committed(id) => {
                self.check_evidence(id)?;
                self.inspect_snapshot(id)?
            }
        };
        // Keep exactly the bytes verified here. The runtime must not reopen
        // mutable paths after this worker returns.
        let mut media = BTreeMap::new();
        for asset in &inspection.document.assets {
            let bytes = self.read_media(&asset.reference)?;
            media.insert(asset.reference.id, Arc::from(bytes));
        }
        Ok(PreparedProject { inspection, media })
    }

    pub fn recovery_options(&mut self) -> Result<RecoveryOptions, StoreError> {
        self.recovery_options_with(&RetentionPolicy::default())
    }

    /// The recovery listing and a bounded collection must agree. Callers that
    /// collect with a custom policy should list with the same policy.
    pub fn recovery_options_with(
        &mut self,
        policy: &RetentionPolicy,
    ) -> Result<RecoveryOptions, StoreError> {
        let current = self
            .inspect_committed()
            .map(|inspection| inspection.map(|value| value.generation));
        self.fs.check_dir(&self.root)?;
        let mut options = RecoveryOptions {
            current,
            recoverable: Vec::new(),
            rejected: Vec::new(),
            unconfirmed: Vec::new(),
        };
        if !self.optional_directory(&self.private())? {
            return Ok(options);
        }
        let committed = self.private().join("committed");
        let mut witnessed = Vec::new();
        if self.optional_directory(&committed)? {
            for path in self.fs.list(&committed)? {
                if path.parent() == Some(committed.as_path())
                    && let Some(id) = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(GenerationId::from_name)
                {
                    witnessed.push(id);
                }
            }
        }
        witnessed.sort_by_key(ToString::to_string);
        witnessed.dedup();
        for &id in &witnessed {
            match self
                .check_evidence(id)
                .and_then(|()| self.inspect_snapshot(id))
            {
                Ok(_) => options.recoverable.push(id),
                Err(error) => options.rejected.push((id, error)),
            }
        }
        let versions = self.private().join("versions");
        if self.optional_directory(&versions)? {
            for path in self.fs.list(&versions)? {
                if path.parent() == Some(versions.as_path())
                    && let Some(id) = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|name| name.strip_suffix(".json"))
                        .and_then(GenerationId::from_name)
                    && !witnessed.contains(&id)
                    && options.current != Ok(Some(id))
                {
                    options.unconfirmed.push(id);
                }
            }
        }
        options.unconfirmed.sort_by_key(ToString::to_string);
        options.unconfirmed.dedup();
        let current = options.current.as_ref().ok().copied().flatten();
        options.recoverable = self.retain_history(current, &options.recoverable, policy)?;
        Ok(options)
    }

    fn optional_directory(&mut self, path: &Path) -> Result<bool, StoreError> {
        match self.fs.check_dir(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn check_evidence(&mut self, id: GenerationId) -> Result<(), StoreError> {
        self.fs.check_dir(&self.root)?;
        if !self.optional_directory(&self.private())?
            || !self.optional_directory(&self.private().join("committed"))?
        {
            return Err(StoreError::UncommittedGeneration(id));
        }
        match self.fs.read(&self.root.join(id.evidence_path())) {
            Ok(bytes) if bytes.is_empty() => Ok(()),
            Ok(_) => Err(StoreError::InvalidCommitEvidence(id)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(StoreError::UncommittedGeneration(id))
            }
            Err(e) => Err(e.into()),
        }
    }
}
