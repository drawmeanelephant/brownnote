//! Bounded worker for D3/D7 imports. No document writes or UI dependencies.
mod session;
use crate::{
    engine::Sample,
    kit::{Waveform, decode_wav, waveform},
    project::{Asset, AssetRef, MediaKind},
};
pub use session::SessionMedia;
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

#[derive(Clone, Debug)]
pub struct Imported {
    pub asset: Asset,
    pub sample: Sample,
    pub waveform: Waveform,
    pub label: String,
}

/// Encode required retained audio and transfer ownership on the render worker.
pub fn retain(
    sample: Sample,
    label: String,
    publisher: &dyn MediaPublisher,
) -> io::Result<Imported> {
    let bytes = crate::kit::encode_wav(&sample)?;
    let reference = AssetRef::of(&bytes);
    publisher.publish(&reference, &bytes)?;
    Ok(Imported {
        asset: Asset {
            reference,
            kind: MediaKind::RetainedRender,
        },
        waveform: waveform(&sample),
        sample,
        label,
    })
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Relaxed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportFailure {
    Cancelled,
    Read(String),
    Decode(String),
    Publish(String),
    WorkerStopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportError {
    pub path: PathBuf,
    pub failure: ImportFailure,
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.path.display(), self.failure)
    }
}
impl std::error::Error for ImportError {}

/// Injected readers return one owned snapshot, never a path to reopen.
pub type ReadFile = Arc<dyn Fn(&Path, &Cancellation) -> io::Result<Vec<u8>> + Send + Sync>;

/// #100's boundary: publish these exact encoded bytes under reference.id,
/// without replacing an existing object. Durable publishers sync media before
/// returning; SessionMedia explicitly owns unsaved bytes until adoption. No
/// project document mutation. Verify existing objects, not just their names.
pub trait MediaPublisher: Send + Sync {
    fn publish(&self, reference: &AssetRef, bytes: &[u8]) -> io::Result<()>;
}

/// Standalone immutable-media publisher. #100 may inject its coordinated
/// implementation through MediaPublisher; neither side owns the other's
/// transaction module. The root must be a private project directory, not cache.
pub struct ProjectMedia {
    root: PathBuf,
}

impl ProjectMedia {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl MediaPublisher for ProjectMedia {
    fn publish(&self, reference: &AssetRef, bytes: &[u8]) -> io::Result<()> {
        if AssetRef::of(bytes) != *reference {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "media identity mismatch",
            ));
        }
        let root = self.root.canonicalize()?;
        fs::create_dir_all(root.join("media"))?;
        let media = root.join("media").canonicalize()?;
        if !media.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "media escapes project",
            ));
        }
        let destination = root.join(reference.id.relative_path());
        let (temporary, mut file) = loop {
            let path = media.join(format!(
                ".import-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => break (Temporary(path), file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary.0, &destination) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&destination)?.file_type().is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "media is not a regular file",
                    ));
                }
                let mut existing = File::open(&destination)?;
                let mut actual = Vec::new();
                Read::by_ref(&mut existing)
                    .take(reference.bytes.saturating_add(1))
                    .read_to_end(&mut actual)?;
                if AssetRef::of(&actual) != *reference {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "existing media failed integrity",
                    ));
                }
                existing.sync_all()?;
            }
            Err(e) => return Err(e),
        }
        drop(temporary);
        File::open(media)?.sync_all()?;
        File::open(root)?.sync_all()
    }
}

struct Request {
    path: PathBuf,
    cancel: Cancellation,
    result: SyncSender<Result<Imported, ImportError>>,
}

pub struct ImportService {
    requests: SyncSender<Request>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    Busy,
    WorkerStopped,
}

pub struct ImportJob {
    path: PathBuf,
    cancel: Cancellation,
    result: Receiver<Result<Imported, ImportError>>,
    finished: bool,
}

impl ImportService {
    /// One worker and one waiting request. Construction does no media I/O.
    pub fn new(publisher: Arc<dyn MediaPublisher>) -> io::Result<Self> {
        Self::with_reader(
            publisher,
            Arc::new(|path, cancel| {
                let mut file = File::open(path)?;
                if !file.metadata()?.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "not a regular file",
                    ));
                }
                let mut bytes = Vec::new();
                let mut buffer = [0; 64 * 1024];
                loop {
                    if cancel.is_cancelled() {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                    }
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        return Ok(bytes);
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
            }),
        )
    }

    pub fn with_reader(publisher: Arc<dyn MediaPublisher>, read: ReadFile) -> io::Result<Self> {
        let (requests, receiver) = mpsc::sync_channel::<Request>(1);
        thread::Builder::new()
            .name("sample-import".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    let result = prepare(&request.path, &request.cancel, &read, publisher.as_ref());
                    let _ = request.result.send(result);
                }
            })?;
        Ok(Self { requests })
    }

    /// None represents picker/path cancellation and never queues file work.
    pub fn import_sample(&self, path: Option<PathBuf>) -> Result<ImportJob, StartError> {
        let (result, receiver) = mpsc::sync_channel(1);
        let cancel = Cancellation::default();
        let job = ImportJob {
            path: path.clone().unwrap_or_default(),
            cancel: cancel.clone(),
            result: receiver,
            finished: false,
        };
        let Some(path) = path else {
            cancel.cancel();
            let _ = result.send(Err(ImportError {
                path: PathBuf::new(),
                failure: ImportFailure::Cancelled,
            }));
            return Ok(job);
        };
        match self.requests.try_send(Request {
            path,
            cancel,
            result,
        }) {
            Ok(()) => Ok(job),
            Err(TrySendError::Full(_)) => Err(StartError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(StartError::WorkerStopped),
        }
    }
}

impl ImportJob {
    /// Blocking completion for non-UI callers. UI polling must use take().
    /// Timeout returns None and leaves the job available for another attempt.
    pub fn wait_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<Result<Imported, ImportError>> {
        if self.finished || self.cancel.is_cancelled() {
            return self.take();
        }
        let result = match self.result.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => return None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ImportError {
                path: self.path.clone(),
                failure: ImportFailure::WorkerStopped,
            }),
        };
        self.finished = true;
        if self.cancel.is_cancelled() {
            return Some(Err(ImportError {
                path: self.path.clone(),
                failure: ImportFailure::Cancelled,
            }));
        }
        Some(result)
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Nonblocking UI-side completion. Cancellation also wins after publication;
    /// the immutable unreferenced object can be collected by a later store save.
    pub fn take(&mut self) -> Option<Result<Imported, ImportError>> {
        if self.finished {
            return None;
        }
        let result = if self.cancel.is_cancelled() {
            Err(ImportError {
                path: self.path.clone(),
                failure: ImportFailure::Cancelled,
            })
        } else {
            match self.result.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Err(ImportError {
                    path: self.path.clone(),
                    failure: ImportFailure::WorkerStopped,
                }),
            }
        };
        self.finished = true;
        Some(result)
    }
}

impl Drop for ImportJob {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn prepare(
    path: &Path,
    cancel: &Cancellation,
    read: &ReadFile,
    publisher: &dyn MediaPublisher,
) -> Result<Imported, ImportError> {
    let error = |failure| ImportError {
        path: path.to_path_buf(),
        failure,
    };
    let check = || {
        if cancel.is_cancelled() {
            Err(error(ImportFailure::Cancelled))
        } else {
            Ok(())
        }
    };
    check()?;
    let bytes = read(path, cancel).map_err(|e| {
        error(if cancel.is_cancelled() {
            ImportFailure::Cancelled
        } else {
            ImportFailure::Read(e.to_string())
        })
    })?;
    check()?;
    let sample = decode_wav(bytes.as_slice(), path)
        .map_err(|e| error(ImportFailure::Decode(e.to_string())))?;
    let waveform = waveform(&sample);
    let reference = AssetRef::of(&bytes);
    check()?;
    publisher
        .publish(&reference, &bytes)
        .map_err(|e| error(ImportFailure::Publish(e.to_string())))?;
    check()?;
    Ok(Imported {
        asset: Asset {
            reference,
            kind: MediaKind::Source,
        },
        sample,
        waveform,
        label: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    })
}
