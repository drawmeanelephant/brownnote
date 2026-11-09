//! Application transactions. The worker owns I/O/decode; callers own the final swap.
use super::{
    AssetId, Document, MediaKind, PAD_COUNT, Pad, PadId,
    recovery::{OpenChoice, PreparedProject, RecoveryOptions},
    store::{GenerationId, SaveFailure, SaveReceipt, Store, StoreError},
};
use crate::{
    assets::{AssetRegistry, ChokeIdentity},
    edit::{EditModel, EditsDocument},
    engine::{Command, SLOTS, Slots},
    import::{Imported, MediaPublisher, ProjectMedia, SessionMedia},
    kit::{KIT_SECONDS, decode_wav_max, waveform},
    midi::{input::MappingsDocument, mapping::Mappings},
    output::Output,
    pedals::render::Chain,
    takes::{Takes, TakesDocument},
};
use std::{
    collections::BTreeMap,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

#[derive(Debug)]
pub enum Error {
    InvalidPath,
    Busy,
    WorkerStopped,
    Store(StoreError),
    Save(SaveFailure),
    Media,
    Decode(AssetId, String),
    Registry,
    Admission(String),
    RecoveryDestination,
    PassActive,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Save(error) => write!(f, "Save failed: {error}. Inspect before retrying."),
            Self::Decode(id, reason) => write!(f, "Cannot decode project media {id}: {reason}"),
            Self::Store(error) => write!(f, "{error}. Choose recovery or another project path."),
            Self::InvalidPath => write!(f, "Enter an absolute project directory path."),
            Self::Busy => write!(f, "A project operation is already queued."),
            Self::WorkerStopped => write!(f, "Project worker stopped; restart the application."),
            Self::Media => write!(
                f,
                "Cannot publish owned project media; saved document unchanged."
            ),
            Self::Registry => write!(
                f,
                "Project exceeds runtime sample capacity or has invalid assignments."
            ),
            Self::Admission(reason) => write!(f, "Project output was not admitted: {reason}"),
            Self::RecoveryDestination => write!(
                f,
                "Save recovered content to a new, nonexistent project directory."
            ),
            Self::PassActive => write!(f, "Stop or cancel recording before saving or opening."),
        }
    }
}
impl std::error::Error for Error {}

/// A small immutable document snapshot. Encoded objects are append-only and
/// owned by this Arc, so edits/session replacement cannot change saved bytes.
pub struct Snapshot {
    pub document: Document,
    pub media: Arc<SessionMedia>,
}

impl Snapshot {
    pub fn capture(
        registry: &AssetRegistry,
        pads: [Pad; PAD_COUNT],
        master: f32,
        choke: bool,
        media: Arc<SessionMedia>,
    ) -> Result<Self, Error> {
        let mut assets = BTreeMap::new();
        let mut pads = pads;
        for (index, pad) in pads.iter_mut().enumerate() {
            pad.sample = registry.pad(pad_id(index)).assignment.clone();
            if let Some(assignment) = &pad.sample {
                for id in [Some(assignment.source), assignment.retained]
                    .into_iter()
                    .flatten()
                {
                    let asset = registry.asset(id).ok_or(Error::Registry)?;
                    assets.insert(id, asset.imported.asset.clone());
                }
            }
        }
        let document = Document {
            pads,
            assets: assets.into_values().collect(),
            master,
            choke,
            ..Document::default()
        };
        document
            .validate()
            .map_err(|error| Error::Store(StoreError::Document(error)))?;
        Ok(Self { document, media })
    }

    /// Freeze the controller's project mappings alongside settings and media.
    pub fn with_midi_mappings(mut self, mappings: &Mappings) -> Self {
        self.document.midi_mappings = Some(MappingsDocument::from_mappings(mappings));
        self
    }

    /// Freeze committed takes and the current edit layer (D9, D15). Refuses a
    /// registry with an armed or recording pass: tentative capture never saves.
    pub fn with_takes(mut self, takes: &Takes, edits: &EditModel) -> Result<Self, Error> {
        if takes.has_pass() {
            return Err(Error::PassActive);
        }
        let edits = EditsDocument::from_model(edits);
        self.document.takes = Some(TakesDocument::from_takes(takes));
        self.document.edits = (!edits.is_empty()).then_some(edits);
        self.document
            .validate()
            .map_err(|error| Error::Store(StoreError::Document(error)))?;
        Ok(self)
    }
}

pub fn pad_id(index: usize) -> PadId {
    match PadId::try_from(index as u8) {
        Ok(pad) if index < PAD_COUNT => pad,
        _ => panic!("project pad index outside fixed bank"),
    }
}

pub struct Prepared {
    pub document: Document,
    pub mappings: Mappings,
    /// Restored committed takes and edit layer; `None` when none were saved.
    pub takes: Option<(Takes, EditModel)>,
    pub generation: GenerationId,
    pub registry: AssetRegistry,
    pub media: Arc<SessionMedia>,
}

impl Prepared {
    /// Operates only on Store::open's verified, owned bytes, never source paths.
    pub fn decode(project: PreparedProject) -> Result<Self, Error> {
        let (inspection, bytes) = project.into_parts();
        let document = inspection.document;
        let mappings = document
            .mappings()
            .map_err(|error| Error::Store(StoreError::Document(error)))?;
        let takes = document
            .takes()
            .map_err(|error| Error::Store(StoreError::Document(error)))?;
        if document.assets.len() > SLOTS {
            return Err(Error::Registry);
        }
        let media = Arc::new(SessionMedia::default());
        let mut slots = Slots::empty();
        let mut imported = Vec::new();
        for (slot, asset) in document.assets.iter().enumerate() {
            let id = asset.reference.id;
            let encoded = bytes.get(&id).ok_or(Error::Media)?;
            let label = format!("{id}.wav");
            let sample = decode_wav_max(encoded.as_ref(), Path::new(&label), |rate| {
                if asset.kind == MediaKind::RetainedRender {
                    // PCM16 mono is the smallest supported encoded frame.
                    // Bound allocation by owned bytes, not D7's source limit:
                    // a retained source can itself acquire another pedal tail.
                    encoded.len() as u64 / 2
                } else {
                    u64::from(rate) * KIT_SECONDS
                }
            })
            .map_err(|error| Error::Decode(id, error.to_string()))?;
            media
                .publish(&asset.reference, encoded)
                .map_err(|_| Error::Media)?;
            slots.set(slot, Some(sample.clone()));
            imported.push(Imported {
                asset: asset.clone(),
                waveform: waveform(&sample),
                sample,
                label,
            });
        }
        let mut registry = AssetRegistry::new(slots);
        for (slot, asset) in imported.into_iter().enumerate() {
            registry
                .register_existing(slot, asset)
                .map_err(|_| Error::Registry)?;
        }
        for (index, pad) in document.pads.iter().enumerate() {
            if let Some(assignment) = &pad.sample {
                registry
                    .bind_existing(
                        pad_id(index),
                        assignment.clone(),
                        ChokeIdentity::Asset(assignment.source),
                    )
                    .map_err(|_| Error::Registry)?;
            }
        }
        Ok(Self {
            document,
            mappings,
            takes,
            generation: inspection.generation,
            registry,
            media,
        })
    }

    /// Admission affects only a separate candidate Output. On failure the
    /// caller must discard it; the active output/registry remain untouched.
    pub fn admit(&self, candidate: &mut Output) -> Result<(), Error> {
        if !candidate.send(Command::SetMaster(self.document.master))
            || !candidate.send(Command::SetChoke(self.document.choke))
        {
            return Err(Error::Admission("candidate command queue is full".into()));
        }
        Ok(())
    }
}

pub enum Request {
    Save {
        root: PathBuf,
        snapshot: Box<Snapshot>,
        new_root: bool,
    },
    Open {
        root: PathBuf,
        choice: OpenChoice,
    },
    Recovery {
        root: PathBuf,
    },
}

pub enum Completion {
    Saved(SaveReceipt),
    Opened(Box<Prepared>),
    Recovery(RecoveryOptions),
}

pub struct Job(Receiver<Result<Completion, Error>>);
impl Job {
    pub fn take(&self) -> Option<Result<Completion, Error>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(Error::WorkerStopped)),
        }
    }
}

type Work = (Request, SyncSender<Result<Completion, Error>>);
pub struct Service(SyncSender<Work>);
impl Service {
    /// One active request plus one waiting request, not one thread per click.
    pub fn new() -> std::io::Result<Self> {
        let (send, receive) = mpsc::sync_channel::<Work>(1);
        thread::Builder::new()
            .name("brownnote-project".into())
            .spawn(move || {
                while let Ok((request, answer)) = receive.recv() {
                    let result = execute(request);
                    let _ = answer.send(result);
                }
            })?;
        Ok(Self(send))
    }

    pub fn start(&self, request: Request) -> Result<Job, Error> {
        let (send, receive) = mpsc::sync_channel(1);
        self.0
            .try_send((request, send))
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::Busy,
                TrySendError::Disconnected(_) => Error::WorkerStopped,
            })?;
        Ok(Job(receive))
    }
}

fn execute(request: Request) -> Result<Completion, Error> {
    let root = match &request {
        Request::Save { root, .. } | Request::Open { root, .. } | Request::Recovery { root } => {
            root
        }
    };
    if !root.is_absolute() {
        return Err(Error::InvalidPath);
    }
    match request {
        Request::Save {
            root,
            snapshot,
            new_root,
        } => {
            snapshot
                .document
                .validate()
                .map_err(|error| Error::Store(StoreError::Document(error)))?;
            // Create only the explicitly selected root; its parent must exist.
            match fs::create_dir(&root) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if new_root {
                        return Err(Error::RecoveryDestination);
                    }
                }
                Err(error) => return Err(Error::Store(error.into())),
            }
            let mut store = Store::new(root.clone());
            // Reject corrupt current before even publishing extra media.
            store.inspect_committed().map_err(Error::Store)?;
            snapshot
                .media
                .adopt(&ProjectMedia::new(root))
                .map_err(|_| Error::Media)?;
            store
                .save(&snapshot.document, &[])
                .map(Completion::Saved)
                .map_err(Error::Save)
        }
        Request::Open { root, choice } => {
            let project = Store::new(root).open(choice).map_err(Error::Store)?;
            Prepared::decode(project).map(|prepared| Completion::Opened(Box::new(prepared)))
        }
        Request::Recovery { root } => Store::new(root)
            .recovery_options()
            .map(Completion::Recovery)
            .map_err(Error::Store),
    }
}

/// The app supplies its sixteen lightweight settings/chains, not another format.
pub fn pads(settings: [(f32, f32, f32, Chain); PAD_COUNT]) -> [Pad; PAD_COUNT] {
    settings.map(|(gain, pan, semitones, chain)| Pad {
        gain,
        pan,
        semitones,
        chain,
        ..Pad::default()
    })
}
