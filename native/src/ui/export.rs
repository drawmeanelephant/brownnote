//! DAW-neutral bundle export from the running app (#198).
//!
//! The writer, manifest and pinned adapter already exist; this module is the
//! off-thread action that invokes them. Capture happens on the control thread
//! (the same owned snapshot a save takes), and a single-flight worker freezes
//! instrument renders exactly as save does and writes the bundle. Pads,
//! transport and meters keep running on the audio thread throughout.
//!
//! One export at a time: a second request is refused, never queued. Dropping
//! the completion receiver and setting the cancel flag is how Escape or
//! closing the panel drops a request; the worker checks it before it starts
//! the bundle transaction, so a cancelled export leaves no destination.
//!
//! Media staging: the writer re-reads every manifest asset from
//! `<source_root>/media`. The captured kit's owned bytes are published through
//! the existing `SessionMedia`/`ProjectMedia` contract into a private
//! temporary root, so unsaved media exports exactly as it plays. There is no
//! second media writer and nothing is regenerated from source audio, a render
//! cache or a plugin (D4).
//!
//! D19: SoundCraft consumes this bundle as a pinned external destination. This
//! panel never launches it, and no plugin state is claimed portable (D21).
use super::{State, button, caption, ink, projects};
use brownnote_native::{
    edit::EditModel,
    export::{
        adapter::{self, AdapterError},
        bundle::{BundleError, BundleReceipt, BundleRequest, SystemWriter},
        manifest::{TimeOrigin, Timing},
    },
    import::ProjectMedia,
    performance::{LoopBars, TakeId, Tempo},
    project::session::{Error as SessionError, Snapshot},
    takes::Takes,
};
use rune::{El, Key, KeyEvent, TextEdit, TextStyle, Tx, col, row, text_input};
use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::Duration,
};

#[cfg(test)]
use std::sync::{Condvar, Mutex};

/// The bundle's audio rate (the writer contract, not the live device rate).
const BUNDLE_RATE: u32 = 48_000;

/// Test seam: inspect the staged media before the bundle transaction reads it.
#[cfg(test)]
type StageHook = Arc<dyn Fn(&Path) + Send + Sync>;

/// A typed reason nothing was published. Every variant names the next action.
#[derive(Debug)]
pub enum Refusal {
    /// Typed paths only (D7): there is no native file dialog.
    InvalidPath,
    DestinationExists(PathBuf),
    /// The bundle must not be written into the project whose media it exports.
    InsideSource(PathBuf),
    ProjectBusy,
    PassActive,
    WorkPending,
    /// The captured snapshot itself could not be taken; the text is its reason.
    Capture(String),
    Freeze(String),
    Staging(String),
    Tempo,
    Adapter(AdapterError),
    Bundle(BundleError),
    WorkerStopped,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => f.write_str("Enter an absolute destination directory path."),
            Self::DestinationExists(path) => write!(
                f,
                "{} already exists; export never overwrites a bundle. Choose a new directory.",
                path.display()
            ),
            Self::InsideSource(path) => write!(
                f,
                "Choose a destination outside the project directory {}; the bundle is written \
                 beside the project, never inside it.",
                path.display()
            ),
            Self::ProjectBusy => f.write_str(
                "Export refused: a project save, open or recovery is running. Nothing was written.",
            ),
            Self::PassActive => f.write_str(
                "Stop or cancel recording before exporting; a tentative pass is never exported. \
                 Nothing was written.",
            ),
            Self::WorkPending => f.write_str(
                "Finish or cancel sample, pedal, plugin or instrument work before exporting; a \
                 changed chain must finish rendering first. Nothing was written.",
            ),
            Self::Capture(reason) => {
                write!(f, "Export refused: {reason} Nothing was written.")
            }
            Self::Freeze(reason) => write!(
                f,
                "Instrument notes could not be frozen for export: {reason}. Nothing was written."
            ),
            Self::Staging(reason) => write!(
                f,
                "Cannot stage the captured media for export: {reason}. Nothing was written."
            ),
            Self::Tempo => f.write_str(
                "The captured takes and the bundle timing disagree on tempo; nothing was written.",
            ),
            Self::Adapter(error) => match error {
                AdapterError::Bundle(error) => f.write_str(&bundle_refusal(error)),
                other => write!(f, "Export refused: {other}. Nothing was written."),
            },
            Self::Bundle(error) => f.write_str(&bundle_refusal(error)),
            Self::WorkerStopped => {
                f.write_str("Export worker stopped; restart the application before exporting.")
            }
        }
    }
}

/// The writer's own cases, named with the action they permit.
fn bundle_refusal(error: &BundleError) -> String {
    let reason = match error {
        BundleError::DestinationExists => {
            "the destination already exists; export never overwrites a bundle".into()
        }
        BundleError::MissingMedia(id) => format!(
            "retained project media {id} is missing from the captured kit; export never \
                 regenerates audio from a source, cache or plugin"
        ),
        BundleError::MediaSizeMismatch {
            id,
            expected,
            actual,
        } => format!(
            "captured media {id} is {actual} bytes but the document says {expected}; export \
                 never regenerates audio"
        ),
        BundleError::MediaHashMismatch { expected, actual } => format!(
            "captured media {expected} hashes to {actual}; the bytes changed after capture, \
                 so nothing was published"
        ),
        BundleError::Audio(brownnote_native::export::audio::AudioError::TooLong) => {
            "the bundle exceeds the writer's WAV length and allocation bounds".into()
        }
        BundleError::Audio(brownnote_native::export::audio::AudioError::Encode) => {
            "bundle audio could not be encoded (non-finite samples or RIFF bounds)".into()
        }
        BundleError::Audio(error) => format!("bundle audio failed: {error}"),
        BundleError::Midi(error) => format!("bundle MIDI failed: {error}"),
        BundleError::TimingOverflow(_) | BundleError::EventBeforeOrigin { .. } => {
            "a performance time does not fit the bundle timeline".into()
        }
        BundleError::Manifest(error) => format!("the manifest was not valid: {error}"),
        BundleError::TargetPartMismatch(id) => format!("event {id:?} has no matching part"),
        BundleError::UnknownEvent(id) => format!("event {id:?} is not in the captured takes"),
        BundleError::UnknownPad(pad) => format!("pad {pad} is not in the captured kit"),
        BundleError::Io { context, source } => {
            format!("writing the bundle failed during {context}: {source}")
        }
    };
    format!(
        "Export failed: {reason}. The destination was not published; the project, takes, edits and output are unchanged."
    )
}

/// The bundle writer's receipt, as the panel reports it.
fn receipt_text(receipt: &BundleReceipt) -> String {
    format!(
        "Exported bundle {} to {}. Manifest {} with {} event(s) and {} asset(s). The captured \
         kit was not modified.",
        receipt.manifest_id,
        receipt.destination.display(),
        receipt.manifest.version,
        receipt.event_count,
        receipt.asset_count
    )
}

/// Single-flight worker state. `generation` tags the request so an answer for
/// an older destination can never reach a newer one.
struct Job {
    generation: u64,
    result: Receiver<Outcome>,
    cancel: Arc<AtomicBool>,
}

enum Outcome {
    Cancelled,
    Exported(BundleReceipt),
    Refused(Refusal),
}

pub struct Export {
    pub visible: bool,
    pub destination: TextEdit,
    /// The receipt, the refusal, or what the panel is doing right now.
    pub notice: String,
    pub error: Option<String>,
    pending: Option<Job>,
    generation: u64,
    /// Test seam: hold the worker before publication so a cancellation or a
    /// second request can be observed without racing a fast local export.
    #[cfg(test)]
    pub hold: Option<Arc<Hold>>,
    /// Test seam: touch the staged media after publication into the private
    /// root but before the bundle transaction reads it back.
    #[cfg(test)]
    pub after_stage: Option<StageHook>,
}

impl Default for Export {
    fn default() -> Self {
        Self {
            visible: false,
            destination: TextEdit::default(),
            notice: "Choose a new absolute directory. Export never overwrites an existing bundle."
                .into(),
            error: None,
            pending: None,
            generation: 0,
            #[cfg(test)]
            hold: None,
            #[cfg(test)]
            after_stage: None,
        }
    }
}

impl Export {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

/// A cancellation the worker can observe before it starts the transaction.
#[cfg(test)]
pub struct Hold(Arc<(Mutex<bool>, Condvar)>);

#[cfg(test)]
impl Hold {
    pub fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }

    pub fn release(&self) {
        if let Ok(mut open) = self.0.0.lock() {
            *open = true;
        }
        self.0.1.notify_all();
    }

    fn wait(&self) {
        let (lock, ready) = &*self.0;
        let mut open = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*open {
            open = ready
                .wait(open)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// The private staging root the writer reads captured media from. Its drop is
/// the one cleanup path for success, refusal and cancellation alike.
struct Staging(PathBuf);

impl Staging {
    fn create() -> Result<Self, Refusal> {
        let base = std::env::temp_dir();
        for _ in 0..16 {
            let path = base.join(format!(
                "brownnote-export-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Refusal::Staging(error.to_string())),
            }
        }
        Err(Refusal::Staging(
            "no unique staging directory could be created".into(),
        ))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// One owned export request. It never re-reads live app state on the worker.
struct Work {
    snapshot: Snapshot,
    destination: PathBuf,
    /// The saved project this export belongs to, when there is one.
    source: Option<PathBuf>,
    tempo_milli_bpm: u32,
    loop_bars: LoopBars,
    cancel: Arc<AtomicBool>,
    #[cfg(test)]
    hold: Option<Arc<Hold>>,
    #[cfg(test)]
    after_stage: Option<StageHook>,
}

/// The captured state differs from the last success; `projects` owns the exact
/// same gate save uses (armed pass, pending work, changed chain).
fn capture(s: &State) -> Result<(Snapshot, u32, LoopBars), Refusal> {
    let snapshot = projects::recoverable_documents(s).map_err(capture_refusal)?;
    Ok((snapshot, s.takes.tempo().milli_bpm(), s.takes.loop_bars()))
}

fn capture_refusal(error: SessionError) -> Refusal {
    match error {
        SessionError::PassActive => Refusal::PassActive,
        SessionError::WorkPending => Refusal::WorkPending,
        other => Refusal::Capture(other.to_string()),
    }
}

/// An absolute, nonexistent destination outside the saved project directory.
fn destination(s: &State) -> Result<PathBuf, Refusal> {
    let path = PathBuf::from(&s.export.destination.text);
    if !path.is_absolute() {
        return Err(Refusal::InvalidPath);
    }
    if path.exists() {
        return Err(Refusal::DestinationExists(path));
    }
    if let Some(root) = s.projects.saved_root()
        && inside(&path, root)
    {
        return Err(Refusal::InsideSource(root.to_path_buf()));
    }
    Ok(path)
}

/// Start one export. A second request while one runs is refused, not queued.
pub fn start(s: &mut State) {
    if s.export.busy() {
        s.export.error = Some(
            "Export refused: an export is already running; finish it or cancel it first. \
             Nothing was written."
                .into(),
        );
        return;
    }
    if s.projects.busy() {
        s.export.error = Some(Refusal::ProjectBusy.to_string());
        return;
    }
    let destination = match destination(s) {
        Ok(destination) => destination,
        Err(refusal) => {
            s.export.error = Some(refusal.to_string());
            return;
        }
    };
    let (snapshot, tempo_milli_bpm, loop_bars) = match capture(s) {
        Ok(captured) => captured,
        Err(refusal) => {
            s.export.error = Some(refusal.to_string());
            return;
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let work = Work {
        snapshot,
        destination: destination.clone(),
        source: s.projects.saved_root().map(Path::to_path_buf),
        tempo_milli_bpm,
        loop_bars,
        cancel: cancel.clone(),
        #[cfg(test)]
        hold: s.export.hold.clone(),
        #[cfg(test)]
        after_stage: s.export.after_stage.clone(),
    };
    s.export.generation = s.export.generation.wrapping_add(1);
    let generation = s.export.generation;
    let (send, result) = mpsc::sync_channel(1);
    let spawned = thread::Builder::new()
        .name("brownnote-export".into())
        .spawn(move || {
            let _ = send.send(execute(work));
        });
    match spawned {
        Ok(_) => {
            s.export.pending = Some(Job {
                generation,
                result,
                cancel,
            });
            s.export.error = None;
            s.export.notice = format!(
                "Exporting the captured kit to {}. Pads, transport and meters keep working.",
                destination.display()
            );
            if let Some(wake) = &s.wake {
                wake();
            }
        }
        Err(_) => s.export.error = Some(Refusal::WorkerStopped.to_string()),
    }
}

/// Escape or the CANCEL EXPORT button: drop the request. The worker checks the
/// flag before it starts the bundle transaction, so nothing is published; a
/// transaction already underway still completes atomically.
pub fn cancel(s: &mut State) {
    let Some(job) = s.export.pending.take() else {
        s.export.notice = "No export is running.".into();
        return;
    };
    job.cancel.store(true, Relaxed);
    // An answer tagged for the dropped request can never reach a newer one.
    s.export.generation = s.export.generation.wrapping_add(1);
    s.export.error = None;
    s.export.notice =
        "Export cancelled before publication. The active kit, saved project and output are \
         unchanged; a bundle transaction already underway still completes atomically."
            .into();
}

/// Leaving the panel drops the pending request, as Escape does.
pub fn close(s: &mut State) {
    s.export.visible = false;
    if s.export.busy() {
        cancel(s);
    }
}

pub fn poll(s: &mut State) -> bool {
    let Some(job) = s.export.pending.take() else {
        return false;
    };
    let outcome = match job.result.try_recv() {
        Ok(outcome) => outcome,
        Err(TryRecvError::Empty) => {
            s.export.pending = Some(job);
            return false;
        }
        Err(TryRecvError::Disconnected) => Outcome::Refused(Refusal::WorkerStopped),
    };
    // A stale completion cannot replace a newer request or the session state.
    if job.generation != s.export.generation {
        return true;
    }
    match outcome {
        Outcome::Exported(receipt) => {
            s.export.notice = receipt_text(&receipt);
            s.export.error = None;
        }
        Outcome::Refused(refusal) => {
            s.export.error = Some(refusal.to_string());
            s.export.notice = "Nothing was written.".into();
        }
        Outcome::Cancelled => {
            s.export.notice = "Export cancelled before publication; nothing was written.".into();
            s.export.error = None;
        }
    }
    true
}

/// While a worker runs the panel asks for the next frame to drain it.
pub fn wants_wake(s: &State) -> bool {
    s.export.busy()
}

fn execute(work: Work) -> Outcome {
    let Work {
        mut snapshot,
        destination,
        source,
        tempo_milli_bpm,
        loop_bars,
        cancel,
        #[cfg(test)]
        hold,
        #[cfg(test)]
        after_stage,
    } = work;
    if cancelled(&cancel) {
        return Outcome::Cancelled;
    }
    // The save worker's own freeze step: notes render on this thread, never the
    // UI or audio thread, and the bytes join the captured project media.
    if let Err(error) = snapshot.freeze() {
        return Outcome::Refused(Refusal::Freeze(error.to_string()));
    }
    let (takes, edits) = match snapshot.document.takes() {
        Ok(Some(models)) => models,
        Ok(None) => match empty_takes(loop_bars, tempo_milli_bpm) {
            Ok(models) => models,
            Err(refusal) => return Outcome::Refused(refusal),
        },
        Err(error) => return Outcome::Refused(Refusal::Capture(error.to_string())),
    };
    if takes.tempo().milli_bpm() != tempo_milli_bpm {
        return Outcome::Refused(Refusal::Tempo);
    }
    // Re-check on the worker: the destination may have appeared while queued.
    if destination.exists() {
        return Outcome::Refused(Refusal::DestinationExists(destination.clone()));
    }
    if let Some(source) = &source
        && inside(&destination, source)
    {
        return Outcome::Refused(Refusal::InsideSource(source.clone()));
    }
    let staging = match Staging::create() {
        Ok(staging) => staging,
        Err(refusal) => return Outcome::Refused(refusal),
    };
    if snapshot
        .media
        .adopt(&ProjectMedia::new(staging.path().to_path_buf()))
        .is_err()
    {
        return Outcome::Refused(Refusal::Staging(
            "the captured media could not be published into the private staging root".into(),
        ));
    }
    #[cfg(test)]
    if let Some(hook) = &after_stage {
        hook(staging.path());
    }
    #[cfg(test)]
    if let Some(hold) = &hold {
        hold.wait();
    }
    if cancelled(&cancel) {
        return Outcome::Cancelled;
    }
    let request = BundleRequest {
        source_root: staging.path().to_path_buf(),
        destination: destination.clone(),
        origin: TimeOrigin { nanos: 0 },
        timing: Timing {
            tempo_milli_bpm,
            sample_rate_hz: BUNDLE_RATE,
        },
    };
    let result = adapter::write_project_bundle(
        &mut SystemWriter,
        &request,
        &snapshot.document,
        &takes,
        &edits,
        &BTreeMap::new(),
    );
    drop(staging);
    match result {
        Ok(receipt) => Outcome::Exported(receipt),
        Err(AdapterError::Bundle(error)) => Outcome::Refused(Refusal::Bundle(error)),
        Err(other) => Outcome::Refused(Refusal::Adapter(other)),
    }
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Relaxed)
}

/// A no-take project still has a tempo and loop length; the bundle timing uses
/// the tempo captured with the snapshot, not a second read of live state.
fn empty_takes(loop_bars: LoopBars, tempo_milli_bpm: u32) -> Result<(Takes, EditModel), Refusal> {
    let mut takes = Takes::new(loop_bars, TakeId(1));
    let tempo = Tempo::try_from(tempo_milli_bpm).map_err(|_| Refusal::Tempo)?;
    takes.set_tempo(tempo).map_err(|_| Refusal::Tempo)?;
    Ok((takes, EditModel::new()))
}

/// True when `destination` is `root` or lies inside it, comparing canonical
/// paths where they exist and the typed path otherwise (a destination is
/// normally nonexistent, so its deepest existing ancestor is compared).
fn inside(destination: &Path, root: &Path) -> bool {
    if destination.starts_with(root) {
        return true;
    }
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let destination = canonical_candidate(destination);
    destination == root || destination.starts_with(&root)
}

fn canonical_candidate(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(mut canonical) = fs::canonicalize(&existing) {
            for part in rest.iter().rev() {
                canonical.push(part);
            }
            return canonical;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Call before pad shortcuts, as the project panel does: typed text must never
/// also play a pad.
pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if !s.export.visible {
        return false;
    }
    if event.key == Key::Escape {
        if s.export.busy() {
            cancel(s);
        } else {
            close(s);
        }
        tx.wake_after(Duration::from_millis(33));
        return true;
    }
    if event.is_command('i') {
        return true;
    }
    matches!(&event.key, Key::Text(_))
        && !event.mods.cmd
        && !event.mods.ctrl
        && !event.mods.alt
        && !event.mods.logo
}

pub fn header(s: &State) -> El {
    button(
        "export-toggle",
        if s.export.visible {
            "BACK TO PADS"
        } else {
            "EXPORT"
        },
    )
    .h(16.0)
    .px(7.0)
    .checked(s.export.visible)
    .label("Export a DAW-neutral bundle")
    .on_press(|s: &mut State| {
        if s.export.visible {
            close(s);
        } else {
            // One panel slot at a time, so the view shows what was chosen.
            s.projects.visible = false;
            s.export.visible = true;
        }
    })
}

pub fn panel(s: &State) -> El {
    let busy = s.export.busy();
    let mut body = col()
        .key("export-panel")
        .role("group")
        .label("Export a DAW-neutral bundle")
        .grow(1.0)
        .min_h(0.0)
        .gap(8.0)
        .padding(12.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(caption("NEUTRAL BUNDLE · MANIFEST v3, notes.mid, media/ · OFF-THREAD"))
        .child(
            text_input(&s.export.destination, TextStyle::new(12.0, 500.0))
                .key("export-destination")
                .label("Absolute destination directory")
                .single_line()
                .h(32.0)
                .px(8.0)
                .fill(ink("#28382f"))
                .disabled(busy)
                .focusable(!busy)
                .on_edit(|s: &mut State| &mut s.export.destination)
                .on_input_key(|s: &mut State, event, tx| {
                    event.key == Key::Escape && on_key(s, event, tx)
                }),
        )
        .child(
            row()
                .gap(8.0)
                .child(
                    button("export-start", "EXPORT")
                        .disabled(busy)
                        .focusable(!busy)
                        .on_press(start),
                )
                .child(
                    button("export-cancel", "CANCEL EXPORT")
                        .focusable(busy)
                        .disabled(!busy)
                        .on_press(cancel),
                ),
        )
        .child(
            caption(&s.export.notice)
                .key("export-status")
                .role("status")
                .wrap(),
        )
        .child(caption(
            "One export at a time; a second request is refused, not queued. The export uses one \
             captured snapshot of the kit, takes, edits and retained audio: work done while it \
             runs is not half-included, and nothing is regenerated. The project, registry, takes \
             and output stay unchanged. SoundCraft consumes the bundle as a pinned external \
             destination; this panel never launches it (D19).",
        ).wrap());
    if let Some(error) = &s.export.error {
        body = body.child(
            caption(error)
                .key("export-error")
                .role("alert")
                .label(error)
                .wrap(),
        );
    }
    body
}

#[cfg(test)]
#[path = "export/tests.rs"]
mod tests;
