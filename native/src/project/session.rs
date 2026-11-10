//! Application transactions. The worker owns I/O/decode; callers own the final swap.
use super::{
    Asset, AssetId, AssetRef, Document, LiveInstrumentDocument, MediaKind, PAD_COUNT, Pad, PadId,
    recovery::{OpenChoice, PreparedProject, RecoveryOptions},
    store::{
        CollectionReceipt, GenerationId, RetentionPlan, RetentionPolicy, SaveFailure, SaveReceipt,
        Store, StoreError,
    },
};
use crate::performance::TakeId;
use crate::{
    assets::{AssetRegistry, ChokeIdentity},
    edit::{EditModel, EditsDocument},
    engine::{Command, SLOTS, Slots},
    import::{Imported, MediaPublisher, ProjectMedia, SessionMedia},
    instrument::{Font, InstrumentDocument, InstrumentError, RenderDocument, RenderKey},
    kit::{KIT_SECONDS, decode_wav_max, waveform},
    live_audio,
    midi::{input::MappingsDocument, mapping::Mappings},
    output::Output,
    pedals::render::Chain,
    takes::{Takes, TakesDocument},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum Error {
    InvalidPath,
    Busy,
    WorkerStopped,
    Store(StoreError),
    Save(Box<SaveFailure>),
    Media,
    Decode(AssetId, String),
    Registry,
    /// Pad-playable assets and retained live takes do not fit the mixer.
    Capacity {
        pad_assets: usize,
        live_takes: usize,
    },
    Admission(String),
    RecoveryDestination,
    PassActive,
    Render(String),
    /// The current kit cannot be captured; the text is the visible reason.
    Unavailable(String),
    /// The saved project changed between capture and the recovery write.
    AutosaveStale,
    /// The machine-level recovery record could not be written.
    AutosaveRecord(io::ErrorKind),
    /// The kit has pending sample, pedal, plugin, or instrument work.
    WorkPending,
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
            Self::Capacity {
                pad_assets,
                live_takes,
            } => write!(
                f,
                "Project exceeds mixer slot capacity: {pad_assets} pad asset(s) and \
                 {live_takes} retained live take(s) need {} of {SLOTS} slots \
                 ({LIVE_TAKE_SLOTS} per take). Remove a pad sample or a live take.",
                pad_assets + live_takes * LIVE_TAKE_SLOTS
            ),
            Self::Admission(reason) => write!(f, "Project output was not admitted: {reason}"),
            Self::RecoveryDestination => write!(
                f,
                "Save recovered content to a new, nonexistent project directory."
            ),
            Self::PassActive => write!(f, "Stop or cancel recording before saving or opening."),
            Self::Render(reason) => write!(f, "Instrument notes could not be frozen: {reason}"),
            Self::Unavailable(reason) => f.write_str(reason),
            Self::AutosaveStale => write!(
                f,
                "The saved project changed while the recovery copy was prepared; \
                 nothing was written. Retry after the next change."
            ),
            Self::AutosaveRecord(kind) => write!(
                f,
                "Cannot record machine-level autosave metadata ({kind:?}); \
                 the previous recovery copy is unchanged."
            ),
            Self::WorkPending => write!(
                f,
                "Finish or cancel sample/pedal/instrument work before saving or opening."
            ),
        }
    }
}
impl std::error::Error for Error {}

/// A small immutable document snapshot. Encoded objects are append-only and
/// owned by this Arc, so edits/session replacement cannot change saved bytes.
pub struct Snapshot {
    pub document: Document,
    pub media: Arc<SessionMedia>,
    /// Notes the save worker still has to freeze, and the font to do it with.
    pub freeze: Option<Freeze>,
}

/// Per-note renders a save still needs (D4). Rendering runs on the project
/// worker, never the UI thread; without a font the notes stay unfrozen and
/// export reports them as having no retained audio.
pub struct Freeze {
    pub font: Option<Font>,
    pub keys: Vec<RenderKey>,
    /// The owner's render cache; each new render is recorded here too.
    pub cache: Arc<Mutex<BTreeMap<RenderKey, AssetId>>>,
}

/// More distinct notes than this are saved without renders for the rest.
pub const MAX_RENDERS: usize = 1024;

/// The two mono mixer slots holding one retained live take's stereo audio.
/// The slots are owned project media, not pad assignments; replay plays them
/// as one clip so the take still sounds after the plugin is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveTakeSlots {
    pub asset: AssetId,
    pub left: usize,
    pub right: usize,
    pub rate: u32,
    pub frames: u64,
}

/// Mixer slots one retained live take needs: one mono slot per channel.
pub const LIVE_TAKE_SLOTS: usize = 2;

/// The two slots a retained take occupies, packed after the pad-playable
/// assets in document order. An open decodes the same assets in that order,
/// so this is the only place the retained-take slot arithmetic lives.
pub fn live_take_slots(pad_assets: usize, index: usize) -> (usize, usize) {
    let left = pad_assets + index * LIVE_TAKE_SLOTS;
    (left, left + 1)
}

/// The mixer's whole capacity for pad audio and retained live takes: one slot
/// per pad-playable asset and two per retained take, inside the fixed pool.
/// Recording refuses the take this rejects, a save refuses the project, and an
/// open refuses a hand-built document, so the three cannot disagree about what
/// this machine can play.
pub fn live_take_capacity(pad_assets: usize, live_takes: usize) -> Result<(), Error> {
    if pad_assets > SLOTS || pad_assets + live_takes * LIVE_TAKE_SLOTS > SLOTS {
        return Err(Error::Capacity {
            pad_assets,
            live_takes,
        });
    }
    Ok(())
}

/// Machine-level autosave metadata, written beside the recovery copy and never
/// into the saved project. `project.json` gains no field (D6).
pub const AUTOSAVE_FILE: &str = "autosave.json";
pub const AUTOSAVE_VERSION: u32 = 1;

/// Which saved project and generation a recovery copy belongs to, and when it
/// was written. This is the only new file; the document format is unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutosaveRecord {
    pub version: u32,
    /// Canonical saved project root the copy belongs to.
    pub project: String,
    /// The saved generation current when the copy was captured.
    pub saved_generation: String,
    /// UNIX milliseconds when the copy was written.
    pub written_ms: u64,
    /// This copy root's own committed generation.
    pub copy_generation: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutosaveReceipt {
    pub generation: GenerationId,
    pub previous: Option<GenerationId>,
    pub saved_generation: GenerationId,
    pub written_ms: u64,
}

/// One project's recovery copy as the open path sees it. `options` is the copy
/// root's own honest store report; `offered` is the single evidenced
/// generation that differs from, and postdates, the saved version being opened.
#[derive(Clone, Debug)]
pub struct RecoveryCopy {
    pub root: PathBuf,
    pub options: RecoveryOptions,
    pub offered: Option<GenerationId>,
    /// Visible explanation of the offer or its refusal.
    pub note: String,
    pub written_ms: Option<u64>,
}

/// The machine-level directory holding every recovery copy of one project.
/// Derived from the canonical saved root, so moving a project never makes the
/// old directory a sibling a save would pick up.
pub fn recovery_copy_root(base: &Path, project: &Path) -> Result<PathBuf, Error> {
    let canonical = fs::canonicalize(project).map_err(|_| Error::InvalidPath)?;
    Ok(subdirectory(base, &canonical))
}

fn subdirectory(base: &Path, canonical: &Path) -> PathBuf {
    base.join(AssetId::of(canonical.as_os_str().as_encoded_bytes()).to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub fn read_autosave(root: &Path) -> Result<AutosaveRecord, String> {
    let bytes = fs::read(root.join(AUTOSAVE_FILE))
        .map_err(|error| format!("autosave metadata is unreadable: {error}"))?;
    let record: AutosaveRecord = serde_json::from_slice(&bytes)
        .map_err(|error| format!("autosave metadata is malformed: {error}"))?;
    if record.version != AUTOSAVE_VERSION {
        return Err(format!(
            "autosave metadata version {} is not supported",
            record.version
        ));
    }
    Ok(record)
}

fn write_autosave(root: &Path, record: &AutosaveRecord) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|_| Error::AutosaveRecord(io::ErrorKind::InvalidData))?;
    let temporary = root.join(format!(".{AUTOSAVE_FILE}.tmp"));
    fs::write(&temporary, &bytes).map_err(|error| Error::AutosaveRecord(error.kind()))?;
    fs::File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| Error::AutosaveRecord(error.kind()))?;
    fs::rename(&temporary, root.join(AUTOSAVE_FILE))
        .map_err(|error| Error::AutosaveRecord(error.kind()))
}

/// Write one complete recovery generation through the same store transaction
/// a save uses. The saved project is read-only here; only the machine recovery
/// root is written, and the previous copy survives a failed attempt untouched.
pub fn autosave_copy(
    base: &Path,
    project: &Path,
    saved: GenerationId,
    mut snapshot: Snapshot,
) -> Result<AutosaveReceipt, Error> {
    snapshot.freeze()?;
    snapshot
        .document
        .validate()
        .map_err(|error| Error::Store(StoreError::Document(error)))?;
    let canonical = fs::canonicalize(project).map_err(|_| Error::InvalidPath)?;
    let root = subdirectory(base, &canonical);
    // The copy belongs to the generation that was current at capture time. A
    // save that won the race makes this copy stale, never a newer recovery.
    let current = Store::new(canonical.clone())
        .inspect_committed()
        .map_err(Error::Store)?;
    if current.map(|inspection| inspection.generation) != Some(saved) {
        return Err(Error::AutosaveStale);
    }
    match fs::create_dir(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(Error::Store(error.into())),
    }
    snapshot
        .media
        .adopt(&ProjectMedia::new(root.clone()))
        .map_err(|_| Error::Media)?;
    let receipt = Store::new(root.clone())
        .save(&snapshot.document, &[])
        .map_err(|error| Error::Save(Box::new(error)))?;
    let written_ms = now_ms();
    write_autosave(
        &root,
        &AutosaveRecord {
            version: AUTOSAVE_VERSION,
            project: canonical.to_string_lossy().into_owned(),
            saved_generation: saved.to_string(),
            written_ms,
            copy_generation: receipt.generation.to_string(),
        },
    )?;
    Ok(AutosaveReceipt {
        generation: receipt.generation,
        previous: receipt.previous,
        saved_generation: saved,
        written_ms,
    })
}

/// Read the recovery copy of one saved project without changing anything.
/// `opened` is the generation the caller is about to apply; a copy that is
/// identical to it, older than it, or recorded for another project is never
/// offered.
pub fn recovery_copy(
    base: &Path,
    project: &Path,
    opened: GenerationId,
) -> Result<Option<RecoveryCopy>, Error> {
    let canonical = fs::canonicalize(project).map_err(|_| Error::InvalidPath)?;
    let root = subdirectory(base, &canonical);
    if fs::metadata(&root).is_err() {
        return Ok(None);
    }
    let options = Store::new(root.clone())
        .recovery_options()
        .map_err(Error::Store)?;
    let record = match read_autosave(&root) {
        Ok(record) => record,
        Err(note) => {
            return Ok(Some(RecoveryCopy {
                root,
                offered: None,
                note: format!("{note}; the copy was not offered."),
                written_ms: None,
                options,
            }));
        }
    };
    let current = options.current.as_ref().ok().copied().flatten();
    let copy = current.filter(|generation| generation.to_string() == record.copy_generation);
    let (offered, note) = match copy {
        None => (
            None,
            match &options.current {
                Err(error) => format!(
                    "The recovery copy's current document is invalid ({error}); it was not offered."
                ),
                _ => "The recovery copy's stored generation does not match its record; \
                      it was not offered."
                    .to_string(),
            },
        ),
        Some(_) if record.project != canonical.to_string_lossy() => (
            None,
            "The machine recovery directory holds a copy of another project; it was not offered."
                .to_string(),
        ),
        Some(copy) if !options.recoverable.contains(&copy) => {
            let rejected = options
                .rejected
                .iter()
                .find(|(generation, _)| *generation == copy)
                .map(|(_, error)| error.to_string())
                .unwrap_or_else(|| "its evidence is incomplete".into());
            (
                None,
                format!("The recovery copy is rejected and was not offered: {rejected}"),
            )
        }
        Some(copy) if copy == opened => (
            None,
            "The recovery copy matches the saved version; it was not offered.".to_string(),
        ),
        Some(_) if record.saved_generation != opened.to_string() => (
            None,
            "The recovery copy is older than the saved version; it was not offered.".to_string(),
        ),
        Some(copy) => (
            Some(copy),
            format!(
                "An unsaved recovery copy of saved generation {opened} was written \
                 after it. It is offered, never applied."
            ),
        ),
    };
    Ok(Some(RecoveryCopy {
        root,
        options,
        offered,
        note,
        written_ms: Some(record.written_ms),
    }))
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
        Ok(Self {
            document,
            media,
            freeze: None,
        })
    }

    /// Add the instrument, its source, and the renders the current effective
    /// notes still use. Renders no note uses any more are dropped; missing
    /// ones are listed for [`Snapshot::freeze`] on the worker.
    pub fn with_instrument(
        mut self,
        instrument: &InstrumentDocument,
        font: Option<Font>,
        cache: Arc<Mutex<BTreeMap<RenderKey, AssetId>>>,
        takes: &Takes,
        edits: &EditModel,
    ) -> Result<Self, Error> {
        let needed = instrument
            .needed(takes, edits)
            .map_err(|_| Error::Registry)?;
        let mut document = instrument.clone();
        document
            .renders
            .retain(|render| needed.contains(&render.key()));
        let keys = needed
            .into_iter()
            .filter(|key| document.render(*key).is_none())
            .take(MAX_RENDERS.saturating_sub(document.renders.len()))
            .collect();
        let mut ids = vec![(document.source, MediaKind::InstrumentSource)];
        ids.extend(
            document
                .renders
                .iter()
                .map(|render| (render.asset, MediaKind::InstrumentRender)),
        );
        for (id, kind) in ids {
            self.add_asset(id, kind)?;
        }
        self.document.instrument = Some(document);
        self.freeze = Some(Freeze { font, keys, cache });
        self.document
            .validate()
            .map_err(|error| Error::Store(StoreError::Document(error)))?;
        Ok(self)
    }

    /// Add the live instrument, its stable identity and every retained take
    /// clip it played into. A disabled record still keeps both. A save that
    /// would write a project this machine cannot open is refused here, with
    /// the counts, rather than producing an unopenable document.
    pub fn with_live_instrument(mut self, document: LiveInstrumentDocument) -> Result<Self, Error> {
        let pad_assets = self
            .document
            .assets
            .iter()
            .filter(|asset| asset.kind.is_pad_audio())
            .count();
        live_take_capacity(pad_assets, document.takes.len())?;
        for take in &document.takes {
            self.add_asset(take.asset, MediaKind::LiveTakeRender)?;
        }
        self.document.live_instrument = Some(document);
        self.document
            .validate()
            .map_err(|error| Error::Store(StoreError::Document(error)))?;
        Ok(self)
    }

    fn add_asset(&mut self, id: AssetId, kind: MediaKind) -> Result<(), Error> {
        if self.document.assets.iter().any(|a| a.reference.id == id) {
            return Ok(());
        }
        let bytes = self.media.get(id).ok_or(Error::Media)?;
        self.document.assets.push(Asset {
            reference: AssetRef::of(&bytes),
            kind,
        });
        Ok(())
    }

    /// Worker side: render the listed notes into owned session media.
    pub fn freeze(&mut self) -> Result<(), Error> {
        let Some(Freeze {
            font: Some(font),
            keys,
            cache,
        }) = self.freeze.take()
        else {
            return Ok(());
        };
        let Some(gain) = self.document.instrument.as_ref().map(|i| i.gain) else {
            return Ok(());
        };
        for key in keys {
            let bytes = font
                .render(key, gain)
                .map_err(|error| Error::Render(error.to_string()))?;
            let reference = AssetRef::of(&bytes);
            self.media
                .publish(&reference, &bytes)
                .map_err(|_| Error::Media)?;
            self.add_asset(reference.id, MediaKind::InstrumentRender)?;
            if let Ok(mut cache) = cache.lock() {
                cache.insert(key, reference.id);
            }
            if let Some(instrument) = &mut self.document.instrument {
                instrument.renders.push(RenderDocument {
                    pitch: key.pitch,
                    velocity: key.velocity,
                    gate_frames: key.gate_frames,
                    asset: reference.id,
                });
            }
        }
        self.document
            .validate()
            .map_err(|error| Error::Store(StoreError::Document(error)))
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
    /// The saved instrument and whether this build could prepare its source.
    pub instrument: Option<(InstrumentDocument, Result<Font, InstrumentError>)>,
    /// The saved live instrument, whether or not this machine can load it.
    pub live: Option<LiveInstrumentDocument>,
    /// Retained live take audio, already decoded into unassigned mixer slots.
    pub live_takes: BTreeMap<TakeId, LiveTakeSlots>,
    /// The machine-level recovery copy of this project, when one exists.
    pub recovery: Option<RecoveryCopy>,
    /// Why the copy could not be checked at all; the open itself still succeeded.
    pub recovery_note: Option<String>,
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
        let pad_assets = document
            .assets
            .iter()
            .filter(|asset| asset.kind.is_pad_audio())
            .count();
        let live_takes_saved = document
            .live_instrument
            .as_ref()
            .map_or(0, |live| live.takes.len());
        live_take_capacity(pad_assets, live_takes_saved)?;
        let media = Arc::new(SessionMedia::default());
        let mut slots = Slots::empty();
        let mut imported = Vec::new();
        for asset in &document.assets {
            let id = asset.reference.id;
            let encoded = bytes.get(&id).ok_or(Error::Media)?;
            if !asset.kind.is_pad_audio() {
                media
                    .publish(&asset.reference, encoded)
                    .map_err(|_| Error::Media)?;
                continue;
            }
            let slot = imported.len();
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
        // Retained live takes decode to two mono channels per take. Their
        // slots stay unassigned (External), so no pad or import can take them.
        let mut live_takes = BTreeMap::new();
        if let Some(live) = &document.live_instrument {
            for (index, take) in live.takes.iter().enumerate() {
                let encoded = bytes.get(&take.asset).ok_or(Error::Media)?;
                let clip = live_audio::decode(encoded, encoded.len() as u64 / 8 + 1)
                    .map_err(|reason| Error::Decode(take.asset, reason))?;
                if clip.rate != take.rate || clip.frames != take.frames {
                    return Err(Error::Decode(
                        take.asset,
                        format!(
                            "retained take is {} frames at {} Hz but the document says {} at {} Hz",
                            clip.frames, clip.rate, take.frames, take.rate
                        ),
                    ));
                }
                media
                    .publish(&AssetRef::of(encoded), encoded)
                    .map_err(|_| Error::Media)?;
                let (left, right) = live_take_slots(pad_assets, index);
                slots.set(
                    left,
                    Some(crate::engine::Sample {
                        data: clip.left.clone(),
                        rate: clip.rate,
                    }),
                );
                slots.set(
                    right,
                    Some(crate::engine::Sample {
                        data: clip.right.clone(),
                        rate: clip.rate,
                    }),
                );
                if let Some(id) = take.id() {
                    live_takes.insert(
                        id,
                        LiveTakeSlots {
                            asset: take.asset,
                            left,
                            right,
                            rate: clip.rate,
                            frames: clip.frames,
                        },
                    );
                }
            }
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
        // A source this build cannot prepare still opens the project: notes
        // and renders stay intact, and the app reports the instrument as silent.
        let instrument = document.instrument.clone().map(|instrument| {
            let font = bytes
                .get(&instrument.source)
                .ok_or(InstrumentError::NoSampleData)
                .and_then(|source| Font::prepare(source.as_ref(), Some(instrument.preset())));
            (instrument, font)
        });
        let live = document.live_instrument.clone();
        Ok(Self {
            document,
            mappings,
            takes,
            generation: inspection.generation,
            registry,
            media,
            instrument,
            live,
            live_takes,
            recovery: None,
            recovery_note: None,
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
        /// Machine recovery root to check for a copy; `None` checks none.
        recovery: Option<PathBuf>,
    },
    Recovery {
        root: PathBuf,
    },
    /// One complete recovery generation (D6). Runs on the same worker as save.
    Autosave {
        project: PathBuf,
        saved: GenerationId,
        base: PathBuf,
        snapshot: Box<Snapshot>,
    },
    /// Plan or apply bounded retention on the project worker. The store lock
    /// still excludes a concurrent save; the worker serializes open/list.
    Retention {
        root: PathBuf,
        policy: RetentionPolicy,
        apply: bool,
    },
}

pub enum Completion {
    /// The written document, so the caller can track dirtiness from exactly
    /// what a successful save left on disk (including freshly frozen renders).
    Saved {
        receipt: SaveReceipt,
        document: Box<Document>,
    },
    Opened(Box<Prepared>),
    Recovery(RecoveryOptions),
    Autosaved(AutosaveReceipt),
    /// `receipt` is present only when the request asked to apply the plan.
    Retained {
        plan: RetentionPlan,
        receipt: Option<CollectionReceipt>,
    },
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
        Request::Save { root, .. }
        | Request::Open { root, .. }
        | Request::Recovery { root }
        | Request::Retention { root, .. } => root,
        Request::Autosave { project, base, .. } => {
            if !project.is_absolute() || !base.is_absolute() {
                return Err(Error::InvalidPath);
            }
            project
        }
    };
    if !root.is_absolute() {
        return Err(Error::InvalidPath);
    }
    match request {
        Request::Save {
            root,
            mut snapshot,
            new_root,
        } => {
            snapshot.freeze()?;
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
            let receipt = store
                .save(&snapshot.document, &[])
                .map_err(|error| Error::Save(Box::new(error)))?;
            Ok(Completion::Saved {
                receipt,
                document: Box::new(snapshot.document),
            })
        }
        Request::Open {
            root,
            choice,
            recovery,
        } => {
            let project = Store::new(root.clone())
                .open(choice)
                .map_err(Error::Store)?;
            let mut prepared = Prepared::decode(project)?;
            // A recovery-copy problem never fails a valid open; it is reported.
            // The caller resolves the machine recovery root; without one this
            // open simply has no copy to check.
            if let Some(base) = recovery {
                match recovery_copy(&base, &root, prepared.generation) {
                    Ok(copy) => prepared.recovery = copy,
                    Err(error) => {
                        prepared.recovery_note =
                            Some(format!("The recovery copy was not checked: {error}"))
                    }
                }
            }
            Ok(Completion::Opened(Box::new(prepared)))
        }
        Request::Autosave {
            project,
            saved,
            base,
            snapshot,
        } => autosave_copy(&base, &project, saved, *snapshot).map(Completion::Autosaved),
        Request::Recovery { root } => Store::new(root)
            .recovery_options()
            .map(Completion::Recovery)
            .map_err(Error::Store),
        Request::Retention {
            root,
            policy,
            apply,
        } => {
            let mut store = Store::new(root);
            let plan = store.retention_plan(&policy).map_err(Error::Store)?;
            let receipt = if apply {
                Some(store.collect(&policy).map_err(Error::Store)?)
            } else {
                None
            };
            Ok(Completion::Retained { plan, receipt })
        }
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
