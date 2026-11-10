//! Explicit project paths and worker completions. See project-session.md for mounting.
use super::{Settings, State, button, caption, edit, ink, media, midi, pedals, transport};
use brownnote_native::{
    engine::Slots,
    import::ImportService,
    output::Output,
    paths::{Resolver, Resource},
    project::{
        Document,
        recovery::{OpenChoice, RecoveryOptions},
        session::{
            self, AutosaveReceipt, Completion, Error, Job, RecoveryCopy, Request, Service, Snapshot,
        },
        store::{CommitState, GenerationId},
    },
};
use rune::{El, Key, KeyEvent, TextEdit, TextStyle, Tx, col, row, text_input};
use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "projects/tests.rs"]
mod tests;

pub type OpenOutput = Rc<dyn Fn(Slots, &Output) -> Result<Output, String>>;

/// D6's idle interval: one complete recovery generation per unsaved minute.
pub const AUTOSAVE_SECS: u64 = 60;
/// The autosave comparison runs at most this often while the app is drawing.
const CHECK_EVERY: Duration = Duration::from_millis(1000);
/// A skipped tick or a failed write retries without waiting another minute.
const RETRY: Duration = Duration::from_millis(1000);
/// While a worker write runs, this lane asks for a frame to drain it.
const WORKER_POLL: Duration = Duration::from_millis(100);

enum Kind {
    Save,
    Open(OpenChoice, Box<Document>),
    Recovery,
}
struct Pending {
    root: PathBuf,
    kind: Kind,
    job: Job,
}

/// The saved project a recovery copy is measured against. A failed save or
/// open never replaces it (D6: dirty means "differs from the last success").
#[derive(Clone)]
struct Saved {
    root: PathBuf,
    generation: GenerationId,
    /// Exactly the document a successful save wrote or an open applied.
    document: Document,
}

pub struct Projects {
    pub visible: bool,
    pub path: TextEdit,
    pub notice: String,
    pub error: Option<String>,
    pub recovery: Option<(PathBuf, RecoveryOptions)>,
    /// The machine-level recovery copy of the last opened project, if any.
    pub recovery_copy: Option<RecoveryCopy>,
    pub recovery_note: String,
    /// Visible autosave state: dirtiness, skips, write results, failures.
    pub autosave_status: String,
    /// Test seam: the machine recovery directory. Unset resolves
    /// `paths::Resource::Recovery` on first use.
    pub recovery_base: Option<PathBuf>,
    /// Test seam for D6's idle interval.
    pub autosave_after: Duration,
    /// Test seam for the dirty-comparison cadence.
    pub check_every: Duration,
    /// Test seam for an injected output. The app leaves this unset and uses
    /// `State::open_project_output`, which mounts the live instrument at the
    /// opened stream's real rate and buffer.
    pub open_output: Option<OpenOutput>,
    service: Result<Service, String>,
    pending: Option<Pending>,
    autosave_job: Option<Job>,
    saved: Option<Saved>,
    dirty: bool,
    observed: Option<Document>,
    checked_at: Instant,
    due: Option<Instant>,
    timer: Timer,
    recovered_from: Option<PathBuf>,
}

impl Default for Projects {
    fn default() -> Self {
        Self {
            visible: false,
            path: TextEdit::default(),
            notice: "Explicit save/open. Unsaved changes are copied to a recovery directory."
                .into(),
            error: None,
            recovery: None,
            recovery_copy: None,
            recovery_note: String::new(),
            autosave_status: "Autosave needs a saved project; nothing to copy yet.".into(),
            recovery_base: None,
            autosave_after: Duration::from_secs(AUTOSAVE_SECS),
            check_every: CHECK_EVERY,
            service: Service::new().map_err(|error| error.to_string()),
            pending: None,
            autosave_job: None,
            saved: None,
            dirty: false,
            observed: None,
            checked_at: Instant::now(),
            due: None,
            timer: Timer::default(),
            recovered_from: None,
            open_output: None,
        }
    }
}

impl Projects {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The root of the last successful save or open. Export (#198) refuses a
    /// destination inside it; nothing else reads it.
    pub(crate) fn saved_root(&self) -> Option<&std::path::Path> {
        self.saved.as_ref().map(|saved| saved.root.as_path())
    }

    fn arm(&mut self, at: Option<Instant>, wake: Option<rune::Waker>) {
        match (at, wake) {
            (Some(at), Some(wake)) => self.timer.arm(at, wake),
            _ => self.timer.disarm(),
        }
    }
}

/// One timer thread per app: it only wakes the event loop at the next autosave
/// deadline. Capture and storage still happen on the UI thread (small owned
/// snapshot) and the project worker (all I/O).
#[derive(Default)]
struct Timer {
    slot: Arc<(Mutex<TimerState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct TimerState {
    at: Option<Instant>,
    stop: bool,
}

impl Timer {
    fn arm(&mut self, at: Instant, wake: rune::Waker) {
        if self.thread.is_none() {
            self.thread = spawn_timer(self.slot.clone(), wake);
        }
        self.set(Some(at));
    }

    fn disarm(&self) {
        self.set(None);
    }

    fn set(&self, at: Option<Instant>) {
        if let Ok(mut state) = self.slot.0.lock() {
            state.at = at;
        }
        self.slot.1.notify_all();
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Ok(mut state) = self.slot.0.lock() {
            state.stop = true;
        }
        self.slot.1.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn spawn_timer(
    slot: Arc<(Mutex<TimerState>, Condvar)>,
    wake: rune::Waker,
) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("brownnote-autosave-timer".into())
        .spawn(move || {
            let (lock, ready) = &*slot;
            loop {
                let mut state = match lock.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if state.stop {
                    return;
                }
                let now = Instant::now();
                match state.at {
                    None => {
                        drop(
                            ready
                                .wait(state)
                                .unwrap_or_else(|poisoned| poisoned.into_inner()),
                        );
                    }
                    Some(at) if now >= at => {
                        state.at = None;
                        drop(state);
                        wake();
                    }
                    Some(at) => {
                        let (next, timeout) = ready
                            .wait_timeout(state, at - now)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state = next;
                        if timeout.timed_out() && state.at == Some(at) {
                            state.at = None;
                            drop(state);
                            wake();
                        }
                    }
                }
            }
        })
        .ok()
}

pub(crate) fn recoverable_documents(s: &State) -> Result<Snapshot, Error> {
    // Tentative capture never saves (D12): say so before the generic busy
    // reason, so a skipped autosave names the armed pass.
    if s.takes.has_pass() {
        return Err(Error::PassActive);
    }
    // A changed chain must not be saved with a retained sound from an older
    // edit. Keep playing that sound, but require render completion before save.
    if s.media.busy()
        || s.instrument.busy()
        || s.live.busy()
        || s.pedals
            .pads
            .iter()
            .any(|pad| !pad.project_chain_is_current())
        || (0..16).any(|pad| !pedals::plugin::is_current(s, pad))
    {
        return Err(Error::WorkPending);
    }
    let mut pads = session::pads(std::array::from_fn(|index| {
        let settings = s.settings[index];
        (
            settings.gain,
            settings.pan,
            settings.pitch,
            s.pedals.pads[index].chain.clone(),
        )
    }));
    // The recipe of the plugin sound that plays, never unrendered edits.
    for (index, pad) in pads.iter_mut().enumerate() {
        pad.plugins = s.pedals.plugins.pads[index].document();
    }
    let mut snapshot =
        Snapshot::capture(&s.assets, pads, s.master, s.choke, s.media.owned.clone())?;
    if let Some(mappings) = midi::mappings(&s.midi)
        && !mappings.entries().is_empty()
    {
        snapshot = snapshot.with_midi_mappings(&mappings);
    }
    if s.takes.has_pass() || s.takes.takes().next().is_some() {
        snapshot = snapshot.with_takes(&s.takes, &s.edit.model)?;
    }
    if let Some((instrument, font, renders)) = s.instrument.snapshot() {
        snapshot = snapshot.with_instrument(&instrument, font, renders, &s.takes, &s.edit.model)?;
    }
    if let Some(live) = s.live.for_save().map_err(Error::Unavailable)? {
        snapshot = snapshot.with_live_instrument(live)?;
    }
    Ok(snapshot)
}

fn snapshot(s: &State) -> Result<Snapshot, String> {
    recoverable_documents(s).map_err(|error| error.to_string())
}

/// What an open must not lose while its worker runs. The live plugin's opaque
/// state is refreshed by inspection (some plugins advance it on every save),
/// so it is not an observable edit; identity, parameters, enablement and
/// retained takes still participate.
fn user_state(document: &Document) -> Document {
    let mut document = document.clone();
    if let Some(live) = &mut document.live_instrument {
        live.state = None;
    }
    document
}

fn begin(s: &mut State, root: PathBuf, kind: Kind, request: Request) {
    if s.projects.busy() {
        s.projects.error = Some("A project operation is already in progress.".into());
        return;
    }
    let result = s
        .projects
        .service
        .as_ref()
        .map_err(|_| Error::WorkerStopped)
        .and_then(|service| service.start(request));
    match result {
        Ok(job) => {
            s.projects.pending = Some(Pending { root, kind, job });
            s.projects.error = None;
            s.projects.notice = "Project worker is running; active pads remain usable.".into();
            if let Some(wake) = &s.wake {
                wake();
            }
        }
        Err(error) => s.projects.error = Some(error.to_string()),
    }
}

fn path(s: &State) -> Result<PathBuf, String> {
    let path = PathBuf::from(&s.projects.path.text);
    if !path.is_absolute() {
        return Err(Error::InvalidPath.to_string());
    }
    Ok(path)
}

pub fn save(s: &mut State) {
    if s.projects.busy() {
        s.projects.error = Some("A project operation is already in progress.".into());
        return;
    }
    let result = path(s).and_then(|root| {
        if s.projects.recovered_from.as_ref() == Some(&root) {
            return Err("Recovered content must be saved to a different project directory.".into());
        }
        snapshot(s).map(|snapshot| (root, snapshot))
    });
    match result {
        Ok((root, snapshot)) => begin(
            s,
            root.clone(),
            Kind::Save,
            Request::Save {
                root,
                snapshot: Box::new(snapshot),
                new_root: s.projects.recovered_from.is_some(),
            },
        ),
        Err(error) => s.projects.error = Some(error),
    }
}

pub fn open(s: &mut State) {
    match path(s) {
        Ok(root) => open_choice(s, root, OpenChoice::Current),
        Err(error) => s.projects.error = Some(error),
    }
}

pub fn open_choice(s: &mut State, root: PathBuf, choice: OpenChoice) {
    if s.devices.busy() {
        s.projects.error = Some("Finish changing audio output before opening a project.".into());
        return;
    }
    match snapshot(s) {
        Ok(before) => {
            // One directory probe per process, on the control thread, never on
            // audio. A missing root only means this open has no copy to check.
            let recovery = recovery_base(s).ok();
            // The previous project's copy is not this open's answer.
            s.projects.recovery_copy = None;
            s.projects.recovery_note.clear();
            begin(
                s,
                root.clone(),
                Kind::Open(choice, Box::new(before.document)),
                Request::Open {
                    root,
                    choice,
                    recovery,
                },
            )
        }
        Err(error) => s.projects.error = Some(error),
    }
}

pub fn recover(s: &mut State) {
    match path(s) {
        Ok(root) => begin(s, root.clone(), Kind::Recovery, Request::Recovery { root }),
        Err(error) => s.projects.error = Some(error),
    }
}

pub fn cancel(s: &mut State) {
    if matches!(
        s.projects.pending.as_ref().map(|p| &p.kind),
        Some(Kind::Save)
    ) {
        s.projects.error =
            Some("A save already in progress cannot be cancelled; inspect its result.".into());
        return;
    }
    // Dropping the only completion receiver makes a late answer unapplyable.
    s.projects.pending = None;
    // A recovery write already on the worker still completes or does not
    // happen at all (one store transaction); dropping the receiver keeps its
    // result from touching any state. No partial file is possible.
    if s.projects.autosave_job.take().is_some() {
        s.projects.autosave_status =
            "Pending autosave dropped. Active kit and saved project unchanged.".into();
    }
    s.projects.notice = "Project open/recovery cancelled. Active kit unchanged.".into();
}

/// Leaving the panel drops the pending autosave completion (D6). The copy
/// write itself remains one complete store transaction or nothing.
pub fn close(s: &mut State) {
    s.projects.visible = false;
    if s.projects.autosave_job.take().is_some() {
        s.projects.autosave_status =
            "Pending autosave dropped. Active kit and saved project unchanged.".into();
    }
}

pub fn poll(s: &mut State) -> bool {
    let mut changed = finish_pending(s);
    changed |= finish_autosave(s);
    changed |= tick_autosave(s);
    changed
}

fn finish_autosave(s: &mut State) -> bool {
    let Some(job) = s.projects.autosave_job.take() else {
        return false;
    };
    let Some(result) = job.take() else {
        s.projects.autosave_job = Some(job);
        return false;
    };
    match result {
        Ok(Completion::Autosaved(receipt)) => {
            s.projects.autosave_status = autosave_success(&receipt);
            s.projects.due = None;
        }
        Err(error) => {
            s.projects.autosave_status = autosave_failure(&error);
            s.projects.due = Some(Instant::now() + RETRY);
        }
        Ok(_) => {
            s.projects.autosave_status =
                "Autosave returned an unexpected completion; nothing was applied.".into();
        }
    }
    true
}

fn autosave_success(receipt: &AutosaveReceipt) -> String {
    format!(
        "Recovery copy {} written for saved generation {}. The saved project was not touched; \
         unsaved changes are still unsaved.",
        receipt.generation, receipt.saved_generation
    )
}

/// Report the store's commit distinction instead of claiming success.
fn autosave_failure(error: &Error) -> String {
    match error {
        Error::Save(failure) => match failure.commit {
            CommitState::Unchanged => format!(
                "Autosave failed before any write ({failure}). The previous recovery copy \
                 and the saved project are unchanged."
            ),
            CommitState::Unknown(generation) => format!(
                "Autosave failed while publishing {generation}; the recovery copy may have \
                 changed ({failure}). Inspect it before retrying."
            ),
            CommitState::PublishedNotDurable(generation) => format!(
                "Recovery copy {generation} is visible but not proven durable ({failure}). \
                 The saved project is unchanged."
            ),
        },
        // The copy itself committed; only its machine record is missing, so it
        // is not offered. Never report this as a successful save.
        Error::AutosaveRecord(_) => format!(
            "Autosave wrote a complete recovery generation but could not record which saved \
             version it belongs to ({error}). It will not be offered; the saved project is \
             unchanged."
        ),
        other => format!(
            "Autosave wrote nothing: {other} The previous recovery copy and the saved project \
             are unchanged."
        ),
    }
}

/// A successful save or open is the only thing that clears dirtiness.
fn remember_saved(s: &mut State, root: PathBuf, generation: GenerationId, document: Document) {
    let document = user_state(&document);
    s.projects.saved = Some(Saved {
        root,
        generation,
        document: document.clone(),
    });
    s.projects.observed = Some(document);
    s.projects.dirty = false;
    s.projects.due = None;
    s.projects.checked_at = Instant::now();
    s.projects.autosave_status = format!("No unsaved changes since generation {generation}.");
}

/// D6: while the kit differs from the last successful save or open, write one
/// complete recovery generation per idle minute. Capture is a small owned
/// snapshot on this thread; the store transaction runs on the project worker.
fn tick_autosave(s: &mut State) -> bool {
    let now = Instant::now();
    if s.projects.autosave_job.is_some() {
        s.projects.arm(Some(now + WORKER_POLL), s.wake.clone());
        return false;
    }
    if s.projects.saved.is_none() {
        let was = s.projects.dirty;
        s.projects.dirty = false;
        s.projects.due = None;
        s.projects.observed = None;
        s.projects.timer.disarm();
        return was;
    }
    if s.projects.pending.is_some() {
        let status = "Autosave skipped: a project operation is running. Nothing was written.";
        let changed = s.projects.autosave_status != status;
        s.projects.autosave_status = status.into();
        if s.projects.dirty {
            s.projects.due = Some(now + RETRY);
        }
        s.projects.arm(s.projects.due, s.wake.clone());
        return changed;
    }

    let mut changed = false;
    let mut blocked = false;
    let mut captured = None;
    let gated = now < s.projects.checked_at + s.projects.check_every;
    if !gated {
        // The previous comparison is the last moment this document was known;
        // an edit cannot be newer than this check and older than that moment.
        let previous = s.projects.checked_at;
        s.projects.checked_at = now;
        match recoverable_documents(s) {
            Err(error) => {
                blocked = true;
                let status = format!("Autosave skipped: {error} Nothing was written.");
                changed |= s.projects.autosave_status != status;
                s.projects.autosave_status = status;
                if s.projects.dirty {
                    s.projects.due = Some(now + RETRY);
                }
            }
            Ok(snapshot) => {
                let document = user_state(&snapshot.document);
                let baseline = s.projects.saved.as_ref().map(|saved| &saved.document);
                let differs = baseline != Some(&document);
                let edited = s.projects.observed.as_ref() != Some(&document);
                if differs {
                    if !s.projects.dirty {
                        s.projects.autosave_status = format!(
                            "Unsaved changes: a recovery copy is written after {}s without \
                             a change.",
                            s.projects.autosave_after.as_secs()
                        );
                        changed = true;
                    }
                    if !s.projects.dirty || edited {
                        s.projects.due = Some(previous + s.projects.autosave_after);
                    }
                    s.projects.dirty = true;
                } else {
                    if s.projects.dirty {
                        s.projects.autosave_status =
                            "Unsaved changes cleared; the saved project is current.".into();
                        changed = true;
                    }
                    s.projects.dirty = false;
                    s.projects.due = None;
                }
                s.projects.observed = Some(document);
                if s.projects.dirty && s.projects.due.is_some_and(|due| now >= due) {
                    captured = Some(snapshot);
                }
            }
        }
    }

    if let Some(snapshot) = captured {
        match recovery_base(s) {
            Err(reason) => {
                s.projects.autosave_status =
                    format!("Autosave skipped: {reason} Nothing was written.");
                s.projects.due = Some(now + RETRY);
                changed = true;
            }
            Ok(base) => {
                let Some(saved) = &s.projects.saved else {
                    return changed;
                };
                let request = Request::Autosave {
                    project: saved.root.clone(),
                    saved: saved.generation,
                    base,
                    snapshot: Box::new(snapshot),
                };
                match s
                    .projects
                    .service
                    .as_ref()
                    .map_err(|_| Error::WorkerStopped)
                    .and_then(|service| service.start(request))
                {
                    Ok(job) => {
                        s.projects.autosave_job = Some(job);
                        s.projects.due = None;
                        s.projects.autosave_status =
                            "Writing one recovery copy of the unsaved project...".into();
                    }
                    Err(error) => {
                        s.projects.autosave_status =
                            format!("Autosave skipped: {error} Nothing was written.");
                        s.projects.due = Some(now + RETRY);
                    }
                }
                changed = true;
            }
        }
    }

    let mut next = if s.projects.autosave_job.is_some() {
        Some(now + WORKER_POLL)
    } else if s.projects.dirty {
        s.projects.due
    } else {
        None
    };
    if gated && !blocked {
        let check = s.projects.checked_at + s.projects.check_every;
        next = Some(next.map_or(check, |next| next.min(check)));
    }
    s.projects.arm(next, s.wake.clone());
    changed
}

/// Resolve the machine recovery directory once, off the audio thread.
fn recovery_base(s: &mut State) -> Result<PathBuf, String> {
    if let Some(base) = &s.projects.recovery_base {
        return Ok(base.clone());
    }
    let base = Resolver::current()
        .and_then(|paths| paths.resolve(Resource::Recovery, None))
        .map_err(|error| error.to_string())?;
    s.projects.recovery_base = Some(base.clone());
    Ok(base)
}

fn finish_pending(s: &mut State) -> bool {
    let Some(pending) = s.projects.pending.take() else {
        return false;
    };
    let Some(result) = pending.job.take() else {
        s.projects.pending = Some(pending);
        return false;
    };
    let result = result.and_then(|completion| {
        match (pending.kind, completion) {
            (
                Kind::Save,
                Completion::Saved {
                    receipt,
                    document,
                },
            ) => {
                s.projects.notice = format!(
                    "Saved generation {}. Edits made during the save are not included.",
                    receipt.generation
                );
                s.projects.recovered_from = None;
                remember_saved(s, pending.root, receipt.generation, *document);
            }
            (Kind::Recovery, Completion::Recovery(options)) => {
                s.projects.notice = "Choose an evidenced generation explicitly; hash order is not time order.".into();
                s.projects.recovery = Some((pending.root, options));
            }
            (Kind::Open(choice, before), Completion::Opened(prepared)) => {
                if s.devices.busy()
                    || snapshot(s)
                        .map(|now| user_state(&now.document) != user_state(&before))
                        .unwrap_or(true)
                {
                    return Err(Error::Admission(
                        "active kit or pending work changed while opening; retry explicitly".into(),
                    ));
                }
                let copy = prepared.recovery.clone();
                let copy_note = copy
                    .as_ref()
                    .map(|copy| copy.note.clone())
                    .or_else(|| prepared.recovery_note.clone())
                    .unwrap_or_default();
                let importer = ImportService::new(prepared.media.clone())
                    .map_err(|_| Error::WorkerStopped)?;
                // Stop the old helper and install the saved live instrument
                // before the stream opens, so a real output mounts its voice at
                // this stream's rate and buffer. A missing bundle is not an
                // open failure: the output still opens with the instrument
                // disabled and its retained takes still play.
                let previous_live = s.live.document.clone();
                s.live
                    .install_project(prepared.live.clone(), prepared.live_takes.clone());
                let hook = s.projects.open_output.clone();
                let opened = match hook {
                    Some(hook) => hook(prepared.registry.slots().clone(), &s.output),
                    None => s.open_project_output(prepared.registry.slots().clone()),
                };
                let mut output = match opened {
                    Ok(output) => output,
                    Err(error) => {
                        s.live.restore_document(previous_live);
                        return Err(Error::Admission(error));
                    }
                };
                if let Err(error) = prepared.admit(&mut output) {
                    s.live.restore_document(previous_live);
                    return Err(error);
                }
                let mut next_pedals =
                    pedals::Pedals::with_bin(s.pedals.cache.clone(), s.pedals.brownnote.clone());
                next_pedals.catalog_fn = s.pedals.catalog_fn.clone();
                next_pedals.render_fn = s.pedals.render_fn.clone();
                next_pedals.auto_load = s.pedals.auto_load;
                next_pedals.tab = s.pedals.tab;
                next_pedals.plugins = s.pedals.plugins.reopened();
                for (index, pad) in prepared.document.pads.iter().enumerate() {
                    let retained_seconds = if pad.sample.as_ref().is_some_and(|sample| sample.retained.is_some()) {
                        let sample = &prepared.registry.audible(session::pad_id(index))
                            .ok_or(Error::Registry)?.imported.sample;
                        Some(sample.data.len() as f32 / sample.rate as f32)
                    } else { None };
                    let source = prepared.registry.source(session::pad_id(index))
                        .map(|asset| (asset.imported.asset.reference.id, &asset.imported.sample));
                    next_pedals.plugins.pads[index].restore(pad.plugins.clone(), source, retained_seconds);
                    let pedal_seconds = if pad.plugins.is_some() { None } else { retained_seconds };
                    next_pedals.pads[index].restore_project_chain(pad.chain.clone(), pedal_seconds);
                }
                if let Some(wake) = &s.wake {
                    next_pedals.set_waker(wake.clone());
                }
                // All fallible validation/preparation/admission is above this
                // point. Cancel old workers only when committing the replacement.
                midi::set_mappings(s, prepared.mappings);
                for index in 0..16 {
                    s.cancel_import(index);
                    pedals::cancel_work(s, index);
                }
                next_pedals.catalog = std::mem::replace(
                    &mut s.pedals.catalog,
                    pedals::CatalogState::Loading,
                );
                s.pedals = next_pedals;
                s.settings = prepared.document.pads.clone().map(|pad| Settings {
                    gain: pad.gain, pan: pad.pan, pitch: pad.semitones,
                });
                s.master = prepared.document.master;
                s.choke = prepared.document.choke;
                s.media = media::Media::with_service(prepared.media, Ok(importer));
                s.assets = prepared.registry;
                s.install_output(output);
                s.instrument.restore(prepared.instrument);
                let (takes, model) = prepared.takes.unwrap_or_else(|| (super::fresh_takes(), Default::default()));
                let mut editor = edit::Editor::default();
                editor.model = model;
                s.takes = takes;
                s.edit = editor;
                transport::reset(s);
                s.flashes = [0.0; 16];
                s.projects.path = TextEdit::new(pending.root.to_string_lossy());
                s.projects.recovered_from = match choice {
                    OpenChoice::Current => None,
                    OpenChoice::Committed(_) => Some(pending.root.clone()),
                };
                s.projects.recovery = None;
                s.projects.recovery_copy = copy;
                s.projects.recovery_note = copy_note;
                s.projects.notice = format!(
                    "Opened generation {}. Playing voices stopped. Retained audio was not regenerated.",
                    prepared.generation
                );
                remember_saved(
                    s,
                    pending.root.clone(),
                    prepared.generation,
                    prepared.document.clone(),
                );
            }
            _ => return Err(Error::WorkerStopped),
        }
        Ok(())
    });
    s.projects.error = result.err().map(|error| error.to_string());
    true
}

/// Call before pad shortcuts. Text entry must never also trigger playable pads.
pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if !s.projects.visible {
        return false;
    }
    if event.key == Key::Escape {
        if s.projects.busy() {
            cancel(s);
        }
        if !s.projects.busy() {
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
        "projects-toggle",
        if s.projects.visible {
            "BACK TO PADS"
        } else {
            "PROJECT"
        },
    )
    .h(16.0)
    .px(7.0)
    .checked(s.projects.visible)
    .label("Save, open and recover projects")
    .on_press(|s: &mut State| {
        if s.projects.visible {
            close(s);
        } else {
            s.projects.visible = true;
        }
    })
}

pub fn panel(s: &State) -> El {
    let busy = s.projects.busy();
    let mut body = col()
        .key("projects-panel").role("group").label("Save, open and recover projects")
        .grow(1.0).min_h(0.0).gap(8.0).padding(12.0)
        .scroll(rune::ScrollSpec { stick_bottom: false })
        .child(caption("PROJECT DIRECTORY · EXPLICIT SAVE / OPEN"))
        .child(text_input(&s.projects.path, TextStyle::new(12.0, 500.0))
            .key("projects-path").label("Absolute project directory")
            .single_line().h(32.0).px(8.0).fill(ink("#28382f"))
            .disabled(busy).focusable(!busy)
            .on_edit(|s: &mut State| &mut s.projects.path)
            .on_input_key(|s: &mut State, event, tx| {
                event.key == Key::Escape && on_key(s, event, tx)
            }))
        .child(row().gap(8.0)
            .child(button("projects-save", "SAVE / SAVE AS").disabled(busy).focusable(!busy).on_press(save))
            .child(button("projects-open", "OPEN (REPLACE KIT)").disabled(busy).focusable(!busy).on_press(open))
            .child(button("projects-recover", "LIST RECOVERY").disabled(busy).focusable(!busy).on_press(recover))
            .child(button("projects-cancel", "CANCEL OPEN").on_press(cancel)))
        .child(caption(&s.projects.notice).key("projects-status").role("status").wrap())
        .child(
            caption(&s.projects.autosave_status)
                .key("projects-autosave")
                .role("status")
                .wrap(),
        )
        .child(caption("Opening replaces unsaved edits only after full validation. Recovery never overwrites a corrupt original. Unsaved changes are copied to a machine recovery directory after 60 seconds; a saved project is never modified by autosave.").wrap());
    if let Some(error) = &s.projects.error {
        body = body.child(
            caption(error)
                .key("projects-error")
                .role("alert")
                .label(error)
                .wrap(),
        );
    }
    if let Some((root, options)) = &s.projects.recovery
        && path(s).as_ref().ok() == Some(root)
    {
        body = body.child(caption(format!("Current: {:?}", options.current)).wrap());
        for generation in &options.recoverable {
            let generation = *generation;
            let root = root.clone();
            body = body.child(
                button(
                    &format!("projects-generation-{generation}"),
                    &format!("OPEN RECOVERY {generation}"),
                )
                .disabled(busy)
                .focusable(!busy)
                .on_press(move |s: &mut State| {
                    open_choice(s, root.clone(), OpenChoice::Committed(generation));
                }),
            );
        }
        for (generation, error) in &options.rejected {
            body = body.child(caption(format!("Rejected {generation}: {error}")).wrap());
        }
        body = body.child(
            caption(format!(
                "{} unconfirmed snapshots are not advertised as saved recovery choices.",
                options.unconfirmed.len()
            ))
            .wrap(),
        );
    }
    if s.projects.recovery_copy.is_some() || !s.projects.recovery_note.is_empty() {
        body = body
            .child(caption("RECOVERY COPY · MACHINE DIRECTORY, NEVER THE SAVED PROJECT").wrap());
        body = body.child(
            caption(&s.projects.recovery_note)
                .key("projects-recovery-copy")
                .role("status")
                .wrap(),
        );
    }
    if let Some(copy) = &s.projects.recovery_copy {
        if let Some(generation) = copy.offered {
            let root = copy.root.clone();
            body = body.child(
                button(
                    &format!("projects-recovery-copy-{generation}"),
                    &format!("OPEN RECOVERY COPY {generation}"),
                )
                .disabled(busy)
                .focusable(!busy)
                .on_press(move |s: &mut State| {
                    open_choice(s, root.clone(), OpenChoice::Committed(generation));
                }),
            );
        }
        let older = copy
            .options
            .recoverable
            .len()
            .saturating_sub(usize::from(copy.offered.is_some()));
        if older > 0 {
            body = body.child(
                caption(format!(
                    "{older} older retained copy generation(s) are evidenced but not offered."
                ))
                .wrap(),
            );
        }
        for (generation, error) in &copy.options.rejected {
            body = body.child(caption(format!("Rejected copy {generation}: {error}")).wrap());
        }
    }
    body
}
