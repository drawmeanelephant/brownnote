//! Explicit project paths and worker completions. See project-session.md for mounting.
use super::{Settings, State, button, caption, edit, ink, media, midi, pedals, transport};
use brownnote_native::{
    engine::Slots,
    import::ImportService,
    output::Output,
    project::{
        Document,
        recovery::{OpenChoice, RecoveryOptions},
        session::{self, Completion, Error, Job, Request, Service, Snapshot},
    },
};
use rune::{El, Key, KeyEvent, TextEdit, TextStyle, Tx, col, row, text_input};
use std::{path::PathBuf, rc::Rc, time::Duration};

#[cfg(test)]
#[path = "projects/tests.rs"]
mod tests;

pub type OpenOutput = Rc<dyn Fn(Slots, &Output) -> Result<Output, String>>;

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

pub struct Projects {
    pub visible: bool,
    pub path: TextEdit,
    pub notice: String,
    pub error: Option<String>,
    pub recovery: Option<(PathBuf, RecoveryOptions)>,
    /// Test seam for an injected output. The app leaves this unset and uses
    /// `State::open_project_output`, which mounts the live instrument at the
    /// opened stream's real rate and buffer.
    pub open_output: Option<OpenOutput>,
    service: Result<Service, String>,
    pending: Option<Pending>,
    recovered_from: Option<PathBuf>,
}

impl Default for Projects {
    fn default() -> Self {
        Self {
            visible: false,
            path: TextEdit::default(),
            notice: "Explicit save/open only. Autosave is not available yet.".into(),
            error: None,
            recovery: None,
            service: Service::new().map_err(|error| error.to_string()),
            pending: None,
            recovered_from: None,
            open_output: None,
        }
    }
}

impl Projects {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

fn snapshot(s: &State) -> Result<Snapshot, String> {
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
        return Err(
            "Finish or cancel sample/pedal/instrument work before saving or opening.".into(),
        );
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
    let mut snapshot = Snapshot::capture(&s.assets, pads, s.master, s.choke, s.media.owned.clone())
        .map_err(|error| error.to_string())?;
    if let Some(mappings) = midi::mappings(&s.midi)
        && !mappings.entries().is_empty()
    {
        snapshot = snapshot.with_midi_mappings(&mappings);
    }
    if s.takes.has_pass() || s.takes.takes().next().is_some() {
        snapshot = snapshot
            .with_takes(&s.takes, &s.edit.model)
            .map_err(|error| error.to_string())?;
    }
    if let Some((instrument, font, renders)) = s.instrument.snapshot() {
        snapshot = snapshot
            .with_instrument(&instrument, font, renders, &s.takes, &s.edit.model)
            .map_err(|error| error.to_string())?;
    }
    if let Some(live) = s.live.for_save()? {
        snapshot = snapshot
            .with_live_instrument(live)
            .map_err(|error| error.to_string())?;
    }
    Ok(snapshot)
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
        Ok(before) => begin(
            s,
            root.clone(),
            Kind::Open(choice, Box::new(before.document)),
            Request::Open { root, choice },
        ),
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
    s.projects.notice = "Project open/recovery cancelled. Active kit unchanged.".into();
}

pub fn poll(s: &mut State) -> bool {
    let Some(pending) = s.projects.pending.take() else {
        return false;
    };
    let Some(result) = pending.job.take() else {
        s.projects.pending = Some(pending);
        return false;
    };
    let result = result.and_then(|completion| {
        match (pending.kind, completion) {
            (Kind::Save, Completion::Saved(receipt)) => {
                s.projects.notice = format!(
                    "Saved generation {}. Edits made during the save are not included.",
                    receipt.generation
                );
                s.projects.recovered_from = None;
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
                    OpenChoice::Committed(_) => Some(pending.root),
                };
                s.projects.notice = format!(
                    "Opened generation {}. Playing voices stopped. Retained audio was not regenerated.",
                    prepared.generation
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
            s.projects.visible = false;
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
    .on_press(|s: &mut State| s.projects.visible = !s.projects.visible)
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
        .child(caption("Opening replaces unsaved edits only after full validation. Recovery never overwrites a corrupt original. Autosave is not implemented.").wrap());
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
    body
}
