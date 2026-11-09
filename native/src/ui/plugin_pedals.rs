//! Offline plugin pedals: a pad's plugin effect chain (CLAP now, LADSPA
//! through the same contract), rendered off the UI thread into retained
//! project audio.
//!
//! It follows the GuitarPedal workflow in `ui::pedals`: edits debounce into a
//! background render, the pad's next hit plays the landed render, and voices
//! already playing keep the slot they started with. A failed, cancelled or
//! stale render never replaces the sound a pad already has. A project saves
//! the recipe of the sound that plays ([`PluginChain`]), never unrendered
//! edits, and reopens without needing the plugin.
use crate::ui::{State, accent, button, caption, ink, media::pad_id};
use brownnote_native::{
    assets::{RegistryError, Replacement},
    engine::Sample,
    import::{Imported, retain},
    kit::PADS,
    pedals::offline::{
        self, Backend, Cancel, ErrorKind, HelperCommand, Helpers, Layout, Limits, Plugin,
        RenderError, RenderPolicy, Rendered, Request, Source,
        protocol::{ParamInfo, PluginInfo},
    },
    project::{
        AssetId,
        plugin::{
            Exact, MAX_STAGES, PluginChain, PluginParam, PluginStage, PolicyRecord, StateBytes,
        },
    },
};
use rune::{Color, Drag, El, Latest, Phase, TextEdit, TextStyle, col, el, row, text, text_input};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
#[path = "plugin_pedals/tests.rs"]
mod tests;

/// Every stage renders and is kept in stereo, then downmixed for the mono pad
/// voice. The export manifest says so.
pub const LAYOUT: Layout = Layout::Stereo;
/// Probes only learn identity and controls, so their rate does not matter.
const PROBE_RATE: u32 = 48_000;

pub type HelpersFn = Arc<dyn Fn() -> Helpers + Send + Sync>;
pub type RenderFn = Arc<
    dyn Fn(&Request, &Helpers, &Limits, &Cancel) -> Result<Rendered, RenderError> + Send + Sync,
>;
pub type ProbeFn = Arc<
    dyn Fn(&Helpers, Backend, &str, &Path, u32, &Limits, &Cancel) -> Result<PluginInfo, RenderError>
        + Send
        + Sync,
>;

/// The configured helper executables. `BROWNNOTE_CLAP_HELPER` wins; otherwise
/// a `brownnote-plugin-host` next to the running executable is used.
pub fn default_helpers() -> Helpers {
    let program = std::env::var_os("BROWNNOTE_CLAP_HELPER")
        .map(PathBuf::from)
        .or_else(|| {
            let sibling = std::env::current_exe()
                .ok()?
                .with_file_name("brownnote-plugin-host");
            sibling.is_file().then_some(sibling)
        });
    match program {
        Some(program) => Helpers::new().with(
            Backend::Clap,
            HelperCommand {
                program,
                args: vec!["clap".into()],
            },
        ),
        None => Helpers::new(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub seconds: f32,
    /// What the last render measured; unknown for a sound reopened from a project.
    pub tail_seconds: Option<f32>,
    pub tail_capped: bool,
    pub clipped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Base,
    Rendering,
    Ready(Report),
    /// The newest render failed. The pad keeps playing what it had.
    Error(String),
}

/// One stage as edited, plus what this machine knows about it.
#[derive(Clone, Debug)]
pub struct StageEdit {
    pub record: PluginStage,
    /// Where the plugin is on this machine, once found.
    pub location: Option<PathBuf>,
    /// Generic controls, from a probe.
    pub controls: Option<Vec<ParamInfo>>,
    /// Why this stage cannot render here right now.
    pub problem: Option<String>,
    /// A different build of the plugin found while relinking.
    pub found: Option<(PluginInfo, PathBuf)>,
}

/// What determines a render's audio. Locations, names and hints do not.
#[derive(Clone, Debug, PartialEq)]
struct Spec {
    source: AssetId,
    stages: Vec<(Plugin, Vec<PluginParam>, Option<StateBytes>)>,
    policy: PolicyRecord,
}

struct Landing {
    imported: Imported,
    record: PluginChain,
    report: Report,
}

struct Landed {
    spec: Spec,
    record: PluginChain,
    report: Report,
}

type Answer = (Replacement, Result<Landing, RenderError>);

pub struct PadPlugins {
    pub stages: Vec<StageEdit>,
    pub focus: Option<usize>,
    pub status: Status,
    render: Latest<Answer>,
    cancel: Cancel,
    ticket: Option<Replacement>,
    requested: Option<Spec>,
    landed: Option<Landed>,
    /// A render that landed while every slot was busy.
    pending: Option<(Spec, Replacement, Landing)>,
}

impl Default for PadPlugins {
    fn default() -> Self {
        Self {
            stages: Vec::new(),
            focus: None,
            status: Status::Base,
            render: Latest::default().debounce(super::DEBOUNCE),
            cancel: Cancel::new(),
            ticket: None,
            requested: None,
            landed: None,
            pending: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Probe {
    Add {
        pad: usize,
        path: PathBuf,
    },
    Relink {
        pad: usize,
        index: usize,
        path: PathBuf,
    },
    Describe {
        pad: usize,
        index: usize,
        path: PathBuf,
    },
}

pub struct Plugins {
    pub pads: [PadPlugins; 16],
    /// The pedal strip shows plugin stages instead of GuitarPedal ones.
    pub shown: bool,
    pub adding: bool,
    pub path: TextEdit,
    pub plugin_id: TextEdit,
    /// Feedback for add and relink, which are not tied to a render.
    pub message: Option<String>,
    pub helpers_fn: HelpersFn,
    pub render_fn: RenderFn,
    pub probe_fn: ProbeFn,
    pub limits: Limits,
    probe: Latest<(Probe, Result<PluginInfo, RenderError>)>,
    probing: Option<Probe>,
}

impl Default for Plugins {
    fn default() -> Self {
        Self {
            pads: std::array::from_fn(|_| PadPlugins::default()),
            shown: false,
            adding: false,
            path: TextEdit::default(),
            plugin_id: TextEdit::default(),
            message: None,
            helpers_fn: Arc::new(default_helpers),
            render_fn: Arc::new(offline::render),
            probe_fn: Arc::new(|helpers, backend, id, location, rate, limits, cancel| {
                offline::probe(helpers, backend, id, location, rate, LAYOUT, limits, cancel)
            }),
            limits: Limits::default(),
            probe: Latest::default(),
            probing: None,
        }
    }
}

impl Plugins {
    pub fn set_waker(&mut self, waker: rune::Waker) {
        self.probe.set_waker(waker.clone());
        for pad in &self.pads {
            pad.render.set_waker(waker.clone());
        }
    }

    /// A fresh set for an opened project, keeping this machine's configuration.
    pub fn reopened(&self) -> Self {
        Self {
            shown: self.shown,
            helpers_fn: self.helpers_fn.clone(),
            render_fn: self.render_fn.clone(),
            probe_fn: self.probe_fn.clone(),
            limits: self.limits,
            ..Self::default()
        }
    }

    pub fn probing(&self) -> bool {
        self.probing.is_some()
    }
}

impl PadPlugins {
    /// The recipe of the sound this pad plays, with this machine's locations
    /// as hints. `None` when no plugin render is retained.
    pub fn document(&self) -> Option<PluginChain> {
        let landed = self.landed.as_ref()?;
        let mut record = landed.record.clone();
        for (index, stage) in record.stages.iter_mut().enumerate() {
            if let Some(edit) = self.stages.get(index)
                && edit.record.plugin == stage.plugin
                && let Some(location) = edit.location.as_ref().and_then(|l| l.to_str())
            {
                stage.location_hint = Some(location.to_owned());
            }
        }
        Some(record)
    }

    /// Reinstate a saved chain whose retained render is already playing.
    /// `source` is the decoded source the identity is checked against.
    pub fn restore(
        &mut self,
        record: Option<PluginChain>,
        source: Option<(AssetId, &Sample)>,
        seconds: Option<f32>,
    ) {
        *self = Self::default();
        let (Some(record), Some((source_id, sample)), Some(seconds)) = (record, source, seconds)
        else {
            return;
        };
        let matches = record.identity(source_of(sample)) == record.render;
        self.stages = record
            .stages
            .iter()
            .map(|stage| {
                let hint = stage.location_hint.as_ref().map(PathBuf::from);
                let present = hint.as_ref().filter(|path| path.exists()).cloned();
                StageEdit {
                    record: stage.clone(),
                    problem: present.is_none().then(|| missing(stage)),
                    location: present,
                    controls: None,
                    found: None,
                }
            })
            .collect();
        self.focus = Some(0);
        let spec = Spec {
            source: source_id,
            stages: record
                .stages
                .iter()
                .map(|stage| {
                    (
                        stage.plugin.clone(),
                        stage.params.clone(),
                        stage.state.clone(),
                    )
                })
                .collect(),
            policy: record.policy,
        };
        if !matches && let Some(first) = self.stages.first_mut() {
            first.problem.get_or_insert_with(|| {
                "The saved recipe does not match this source; the retained sound still plays."
                    .into()
            });
        }
        let report = Report {
            seconds,
            tail_seconds: None,
            tail_capped: false,
            clipped: false,
        };
        self.status = Status::Ready(report.clone());
        self.requested = Some(spec.clone());
        self.landed = Some(Landed {
            spec,
            record,
            report,
        });
    }
}

fn missing(stage: &PluginStage) -> String {
    match &stage.location_hint {
        Some(hint) => format!(
            "{} {} is not at {hint} on this machine. Locate it to edit or re-render; the retained sound keeps playing.",
            stage.plugin.backend, stage.plugin.id
        ),
        None => format!(
            "{} {} has no known location. Locate it to edit or re-render; the retained sound keeps playing.",
            stage.plugin.backend, stage.plugin.id
        ),
    }
}

fn source_of(sample: &Sample) -> Source {
    Source {
        channels: 1,
        rate: sample.rate,
        samples: sample.data.clone(),
    }
}

/// The current edit's spec, or `None` without a source.
fn spec(s: &State, pad: usize) -> Option<Spec> {
    let source = s.assets.source(pad_id(pad))?;
    Some(Spec {
        source: source.imported.asset.reference.id,
        stages: s.pedals.plugins.pads[pad]
            .stages
            .iter()
            .map(|stage| {
                (
                    stage.record.plugin.clone(),
                    stage.record.params.clone(),
                    stage.record.state.clone(),
                )
            })
            .collect(),
        policy: RenderPolicy::standard(LAYOUT, source.imported.sample.rate).into(),
    })
}

/// True when saving would record exactly the sound this pad plays.
pub fn is_current(s: &State, pad: usize) -> bool {
    let plugins = &s.pedals.plugins.pads[pad];
    if plugins.pending.is_some() || plugins.status == Status::Rendering {
        return false;
    }
    match &plugins.landed {
        Some(landed) => spec(s, pad).as_ref() == Some(&landed.spec),
        None => plugins.stages.is_empty(),
    }
}

pub fn active(s: &State, pad: usize) -> bool {
    !s.pedals.plugins.pads[pad].stages.is_empty() || s.pedals.plugins.pads[pad].landed.is_some()
}

/// A shared library is a LADSPA plugin only when a LADSPA helper exists; a
/// CLAP plugin can be a bare shared library too.
fn backend_for(path: &Path, helpers: &Helpers) -> Backend {
    match path.extension().and_then(|e| e.to_str()) {
        Some("so" | "dylib" | "dll") if helpers.get(Backend::Ladspa).is_some() => Backend::Ladspa,
        _ => Backend::Clap,
    }
}

fn start_probe(s: &mut State, probe: Probe, plugin_id: String, backend: Backend) {
    let plugins = &mut s.pedals.plugins;
    let helpers = (plugins.helpers_fn)();
    let probe_fn = plugins.probe_fn.clone();
    let limits = plugins.limits;
    plugins.probing = Some(probe.clone());
    plugins.probe.request(probe, move |probe| {
        let path = match &probe {
            Probe::Add { path, .. } | Probe::Relink { path, .. } | Probe::Describe { path, .. } => {
                path.clone()
            }
        };
        let result = probe_fn(
            &helpers,
            backend,
            &plugin_id,
            &path,
            PROBE_RATE,
            &limits,
            &Cancel::new(),
        );
        (probe, result)
    });
}

/// Load the plugin named by the path and ID fields as a new last stage.
pub fn add(s: &mut State, pad: usize) {
    let path = PathBuf::from(s.pedals.plugins.path.text.trim());
    let id = s.pedals.plugins.plugin_id.text.trim().to_string();
    let message = if !s.pedals.pads[pad].chain.stages.is_empty() {
        Some("Remove this pad's GuitarPedal pedals first: one retained sound has one recipe.")
    } else if s.pedals.plugins.pads[pad].stages.len() >= MAX_STAGES {
        Some("A pad holds at most four plugin stages.")
    } else if !path.is_absolute() {
        Some("Enter the plugin's absolute path.")
    } else if id.is_empty() {
        Some("Enter the plugin ID, for example com.airwindows.consolidated.")
    } else if s.pedals.plugins.probing() {
        Some("A plugin is still loading.")
    } else {
        None
    };
    if let Some(message) = message {
        s.pedals.plugins.message = Some(message.into());
        return;
    }
    s.pedals.plugins.message = Some(format!("Loading {id}…"));
    let backend = backend_for(&path, &(s.pedals.plugins.helpers_fn)());
    start_probe(s, Probe::Add { pad, path }, id, backend);
}

/// Point a stage at a plugin path on this machine. An identical build is
/// adopted silently; a different one is offered, never substituted.
pub fn relink(s: &mut State, pad: usize, index: usize) {
    let path = PathBuf::from(s.pedals.plugins.path.text.trim());
    let Some(stage) = s.pedals.plugins.pads[pad].stages.get(index) else {
        return;
    };
    if !path.is_absolute() {
        s.pedals.plugins.message = Some("Enter the plugin's absolute path.".into());
        return;
    }
    if s.pedals.plugins.probing() {
        s.pedals.plugins.message = Some("A plugin is still loading.".into());
        return;
    }
    let (id, backend) = (stage.record.plugin.id.clone(), stage.record.plugin.backend);
    s.pedals.plugins.message = Some(format!("Checking {}…", path.display()));
    start_probe(s, Probe::Relink { pad, index, path }, id, backend);
}

/// Use the different build a relink found. The pad keeps its sound until the
/// render with the new build lands.
pub fn adopt(s: &mut State, pad: usize, index: usize) {
    let Some(stage) = s.pedals.plugins.pads[pad].stages.get_mut(index) else {
        return;
    };
    let Some((info, path)) = stage.found.take() else {
        return;
    };
    stage.record.plugin = info.plugin;
    stage.record.name = info.name;
    stage.record.state = None;
    stage
        .record
        .params
        .retain(|param| info.params.iter().any(|p| p.id == param.id));
    stage.controls = Some(info.params);
    stage.location = Some(path);
    stage.problem = None;
    s.pedals.plugins.message = None;
    request(s, pad);
}

pub fn remove(s: &mut State, pad: usize, index: usize) {
    let plugins = &mut s.pedals.plugins.pads[pad];
    if index >= plugins.stages.len() {
        return;
    }
    plugins.stages.remove(index);
    plugins.focus = (!plugins.stages.is_empty()).then(|| index.min(plugins.stages.len() - 1));
    request(s, pad);
}

pub fn move_stage(s: &mut State, pad: usize, from: usize, to: usize) {
    let plugins = &mut s.pedals.plugins.pads[pad];
    if from >= plugins.stages.len() || to >= plugins.stages.len() {
        return;
    }
    let stage = plugins.stages.remove(from);
    plugins.stages.insert(to, stage);
    plugins.focus = Some(to);
    request(s, pad);
}

/// Set a generic control, clamped to the plugin's range.
pub fn set_param(s: &mut State, pad: usize, index: usize, id: u32, value: f64) {
    let Some(stage) = s.pedals.plugins.pads[pad].stages.get_mut(index) else {
        return;
    };
    let Some(info) = stage
        .controls
        .as_ref()
        .and_then(|controls| controls.iter().find(|p| p.id == id))
    else {
        return;
    };
    if !value.is_finite() {
        return;
    }
    let mut value = value.clamp(info.min, info.max);
    if info.stepped {
        value = value.round().clamp(info.min, info.max);
    }
    let params = &mut stage.record.params;
    match params.binary_search_by_key(&id, |param| param.id) {
        Ok(at) => params[at].value = Exact(value),
        Err(at) => params.insert(
            at,
            PluginParam {
                id,
                value: Exact(value),
            },
        ),
    }
    request(s, pad);
}

/// Put the chain back to the recipe of the sound the pad plays.
pub fn revert(s: &mut State, pad: usize) {
    cancel_work(s, pad);
    let plugins = &mut s.pedals.plugins.pads[pad];
    let Some(landed) = &plugins.landed else {
        plugins.stages.clear();
        plugins.focus = None;
        plugins.status = Status::Base;
        return;
    };
    let previous = std::mem::take(&mut plugins.stages);
    plugins.stages = landed
        .record
        .stages
        .iter()
        .map(|record| {
            let known = previous
                .iter()
                .find(|edit| edit.record.plugin == record.plugin);
            StageEdit {
                record: record.clone(),
                location: known.and_then(|edit| edit.location.clone()),
                controls: known.and_then(|edit| edit.controls.clone()),
                problem: known.and_then(|edit| edit.problem.clone()),
                found: None,
            }
        })
        .collect();
    plugins.focus = Some(0);
    plugins.requested = Some(landed.spec.clone());
    plugins.status = Status::Ready(landed.report.clone());
}

/// Stop a running or queued render. The pad keeps the sound it has.
pub fn cancel_work(s: &mut State, pad: usize) {
    let plugins = &mut s.pedals.plugins.pads[pad];
    plugins.cancel.cancel();
    plugins.render.cancel();
    plugins.pending = None;
    plugins.requested = None;
    if let Some(ticket) = plugins.ticket.take() {
        s.assets.cancel(ticket);
    }
    let plugins = &mut s.pedals.plugins.pads[pad];
    plugins.status = match &plugins.landed {
        Some(landed) => Status::Ready(landed.report.clone()),
        None => Status::Base,
    };
}

/// The pad's source was replaced, which also dropped its retained render.
pub fn source_changed(s: &mut State, pad: usize) {
    cancel_work(s, pad);
    let plugins = &mut s.pedals.plugins.pads[pad];
    plugins.landed = None;
    plugins.status = Status::Base;
    request(s, pad);
}

/// Ask for a render of the pad's current chain.
pub fn request(s: &mut State, pad: usize) {
    if s.pedals.plugins.pads[pad].stages.is_empty() {
        cancel_work(s, pad);
        if s.pedals.plugins.pads[pad].landed.take().is_some() {
            s.assets
                .reset_to_source(pad_id(pad), |command| s.output.send(command));
        }
        s.pedals.plugins.pads[pad].status = Status::Base;
        return;
    }
    let Some(spec) = spec(s, pad) else {
        return;
    };
    let plugins = &s.pedals.plugins.pads[pad];
    if plugins.requested.as_ref() == Some(&spec) {
        return;
    }
    if plugins
        .landed
        .as_ref()
        .is_some_and(|landed| landed.spec == spec)
    {
        cancel_work(s, pad);
        s.pedals.plugins.pads[pad].requested = Some(spec);
        return;
    }
    if let Some((index, stage)) = plugins
        .stages
        .iter()
        .enumerate()
        .find(|(_, stage)| stage.location.is_none())
    {
        let message = format!(
            "Plugin stage {} ({} {}) is not installed here; nothing was rendered.",
            index + 1,
            stage.record.plugin.backend,
            stage.record.plugin.id
        );
        cancel_work(s, pad);
        s.pedals.plugins.pads[pad].status = Status::Error(message);
        return;
    }
    let Some(source) = s.assets.source(pad_id(pad)) else {
        return;
    };
    let label = format!("{} + plugins", source.imported.label);
    let sample = source.imported.sample.clone();
    s.cancel_import(pad);
    let plugins = &mut s.pedals.plugins.pads[pad];
    plugins.cancel.cancel();
    plugins.cancel = Cancel::new();
    plugins.pending = None;
    let names: Vec<String> = plugins
        .stages
        .iter()
        .map(|s| s.record.name.clone())
        .collect();
    let chain = PluginChain {
        stages: plugins
            .stages
            .iter()
            .map(|stage| stage.record.clone())
            .collect(),
        policy: spec.policy,
        render: offline::Hash256([0; 32]),
    };
    let locations: Vec<PathBuf> = plugins
        .stages
        .iter()
        .filter_map(|stage| stage.location.clone())
        .collect();
    let request = chain.request(source_of(&sample), |index, _| locations[index].clone());
    let cancel = plugins.cancel.clone();
    plugins.status = Status::Rendering;
    plugins.requested = Some(spec);
    let ticket = s.assets.begin(pad_id(pad));
    let plugins = &mut s.pedals.plugins;
    plugins.pads[pad].ticket = Some(ticket);
    let (helpers, render_fn, limits) = (
        (plugins.helpers_fn)(),
        plugins.render_fn.clone(),
        plugins.limits,
    );
    let owned = s.media.owned.clone();
    plugins.pads[pad].render.request(request, move |request| {
        let result = render_fn(&request, &helpers, &limits, &cancel).and_then(|rendered| {
            let (sample, peak) = downmix(&rendered);
            let report = Report {
                seconds: sample.data.len() as f32 / sample.rate as f32,
                tail_seconds: Some(
                    rendered.frames().saturating_sub(rendered.source_frames) as f32
                        / rendered.rate as f32,
                ),
                tail_capped: rendered.tail_capped(),
                clipped: peak > 1.0,
            };
            let record = PluginChain::record(&request, &names, rendered.identity);
            retain(sample, label, owned.as_ref())
                .map(|imported| Landing {
                    imported,
                    record,
                    report,
                })
                .map_err(|error| RenderError {
                    stage: None,
                    plugin: None,
                    kind: ErrorKind::Failed {
                        code: "retain".into(),
                        message: error.to_string(),
                    },
                })
        });
        (ticket, result)
    });
}

/// The pad voice is mono: average the channels, like the WAV decoder does.
fn downmix(rendered: &Rendered) -> (Sample, f32) {
    let channels = usize::from(rendered.layout.channels());
    let data: Arc<[f32]> = rendered
        .samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    let peak = data.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    (
        Sample {
            data,
            rate: rendered.rate,
        },
        peak,
    )
}

/// Land probe and render answers, retry renders waiting for a slot, and
/// describe a shown stage whose controls are unknown.
pub fn poll(s: &mut State) -> bool {
    let mut changed = false;
    if let Some((probe, result)) = s.pedals.plugins.probe.take() {
        s.pedals.plugins.probing = None;
        land_probe(s, probe, result);
        changed = true;
    }
    for pad in 0..16 {
        if let Some((ticket, result)) = s.pedals.plugins.pads[pad].render.take() {
            changed = true;
            if s.assets.current(ticket) {
                s.pedals.plugins.pads[pad].ticket = None;
                match result {
                    Ok(landing) => {
                        let spec = s.pedals.plugins.pads[pad].requested.clone();
                        if let Some(spec) = spec {
                            land(s, pad, spec, ticket, landing);
                        }
                    }
                    Err(error) if error.kind == ErrorKind::Cancelled => {}
                    Err(error) => fail(s, pad, ticket, error),
                }
            }
        }
        changed |= retry_pending(s, pad);
    }
    changed |= describe_shown(s);
    changed
}

fn fail(s: &mut State, pad: usize, ticket: Replacement, error: RenderError) {
    s.assets.report_error(ticket, error.to_string());
    let plugins = &mut s.pedals.plugins.pads[pad];
    if let Some(index) = error.stage
        && let Some(stage) = plugins.stages.get_mut(index)
    {
        match &error.kind {
            ErrorKind::Unavailable { .. } | ErrorKind::ArtifactChanged { .. } => {
                stage.problem = Some(error.to_string());
                stage.location = None;
            }
            _ => {}
        }
    }
    // Not retried automatically: an edit, relink or adopt asks again.
    plugins.requested = None;
    plugins.status = Status::Error(error.to_string());
}

fn land(s: &mut State, pad: usize, spec: Spec, ticket: Replacement, landing: Landing) -> bool {
    match s
        .assets
        .replace_retained(ticket, &landing.imported, |command| s.output.send(command))
    {
        Ok(()) => {
            let plugins = &mut s.pedals.plugins.pads[pad];
            plugins.status = Status::Ready(landing.report.clone());
            plugins.landed = Some(Landed {
                spec,
                record: landing.record,
                report: landing.report,
            });
            true
        }
        Err(RegistryError::Full | RegistryError::QueueFull) => {
            s.pedals.plugins.pads[pad].pending = Some((spec, ticket, landing));
            false
        }
        Err(error) => {
            s.pedals.plugins.pads[pad].status =
                Status::Error(format!("Cannot assign the render: {error:?}"));
            true
        }
    }
}

fn retry_pending(s: &mut State, pad: usize) -> bool {
    let Some((spec, ticket, _)) = s.pedals.plugins.pads[pad].pending.as_ref() else {
        return false;
    };
    if Some(spec) != self::spec(s, pad).as_ref() || !s.assets.current(*ticket) {
        s.pedals.plugins.pads[pad].pending = None;
        return true;
    }
    let Some((spec, ticket, landing)) = s.pedals.plugins.pads[pad].pending.take() else {
        return false;
    };
    land(s, pad, spec, ticket, landing)
}

fn land_probe(s: &mut State, probe: Probe, result: Result<PluginInfo, RenderError>) {
    let relinking = matches!(probe, Probe::Relink { .. });
    let plugins = &mut s.pedals.plugins;
    match probe {
        Probe::Add { pad, path } => match result {
            Ok(info) => {
                if !s.pedals.pads[pad].chain.stages.is_empty()
                    || plugins.pads[pad].stages.len() >= MAX_STAGES
                {
                    plugins.message = Some("The pad changed while the plugin loaded.".into());
                    return;
                }
                plugins.message = Some(format!(
                    "Loaded {} ({} {}); rendering for the next hit.",
                    info.name, info.plugin.id, info.plugin.version
                ));
                plugins.adding = false;
                let record = PluginStage {
                    plugin: info.plugin.clone(),
                    name: info.name.clone(),
                    params: Vec::new(),
                    state: None,
                    location_hint: path.to_str().map(str::to_owned),
                };
                let pad_plugins = &mut plugins.pads[pad];
                pad_plugins.stages.push(StageEdit {
                    record,
                    location: Some(path),
                    controls: Some(info.params),
                    problem: None,
                    found: None,
                });
                pad_plugins.focus = Some(pad_plugins.stages.len() - 1);
                request(s, pad);
            }
            Err(error) => plugins.message = Some(error.to_string()),
        },
        Probe::Relink { pad, index, path } | Probe::Describe { pad, index, path } => {
            let Some(stage) = plugins.pads[pad].stages.get_mut(index) else {
                return;
            };
            match result {
                Ok(info) if info.plugin == stage.record.plugin => {
                    stage.location = Some(path);
                    stage.controls = Some(info.params);
                    stage.problem = None;
                    stage.found = None;
                    if relinking {
                        plugins.message = Some(format!("Relinked {}.", info.plugin.id));
                        // A render that failed for the missing plugin may run now.
                        plugins.pads[pad].requested = None;
                    }
                    request(s, pad);
                }
                Ok(info) => {
                    stage.problem = Some(format!(
                        "Found {} {} (binary {}…), but the sound was made with {} (binary {}…). Use it to re-render; the current sound plays until then.",
                        info.plugin.id,
                        info.plugin.version,
                        &info.plugin.artifact.to_string()[..12],
                        stage.record.plugin.version,
                        &stage.record.plugin.artifact.to_string()[..12],
                    ));
                    stage.location = None;
                    stage.found = Some((info, path));
                    plugins.message = None;
                }
                Err(error) => {
                    stage.problem = Some(error.to_string());
                    stage.location = None;
                    plugins.message = None;
                }
            }
        }
    }
}

/// Learn the controls of the focused stage of the shown pad, once.
fn describe_shown(s: &mut State) -> bool {
    if !s.pedals.plugins.shown || s.pedals.plugins.probing() {
        return false;
    }
    let pad = s.selected;
    let Some(index) = s.pedals.plugins.pads[pad].focus else {
        return false;
    };
    let Some(stage) = s.pedals.plugins.pads[pad].stages.get(index) else {
        return false;
    };
    if stage.controls.is_some() || stage.problem.is_some() {
        return false;
    }
    let Some(path) = stage.location.clone() else {
        return false;
    };
    let (id, backend) = (stage.record.plugin.id.clone(), stage.record.plugin.backend);
    start_probe(s, Probe::Describe { pad, index, path }, id, backend);
    true
}

// ---- view ----

fn status_color(status: &Status) -> Color {
    match status {
        Status::Error(_) => ink("#ffad92"),
        Status::Ready(_) => ink("#c8f77b"),
        _ => ink("#8c9e98"),
    }
}

pub fn status_text(s: &State, pad: usize) -> String {
    let plugins = &s.pedals.plugins.pads[pad];
    let mut text = match &plugins.status {
        Status::Base => "BASE SAMPLE".to_string(),
        Status::Rendering => "RENDERING…".into(),
        Status::Ready(report) => {
            let mut text = format!("READY {:.1} s", report.seconds);
            if let Some(tail) = report.tail_seconds {
                text.push_str(&format!(" · TAIL {tail:.1} s"));
            }
            if report.tail_capped {
                text.push_str(" · TAIL CAPPED");
            }
            if report.clipped {
                text.push_str(" · CLIPPED");
            }
            text
        }
        Status::Error(message) => format!(
            "ERROR · {} · PREVIOUS SOUND KEPT",
            super::first_line(message)
        ),
    };
    if plugins.stages.iter().any(|stage| stage.problem.is_some()) {
        text.push_str(" · PLUGIN MISSING");
    }
    text
}

/// The pedals header in plugin mode.
pub fn header(s: &State) -> El {
    let spec = &PADS[s.selected];
    let status = &s.pedals.plugins.pads[s.selected].status;
    row()
        .justify_between()
        .gap(10.0)
        .center_y()
        .child(
            caption(format!("PLUGINS {:02}  /  {}", s.selected + 1, spec.label))
                .color(accent(spec.sample)),
        )
        .child(
            caption(status_text(s, s.selected))
                .key("plugin-status")
                .role("status")
                .color(status_color(status))
                .truncate(),
        )
}

/// Switch the strip between GuitarPedal and plugin stages.
pub fn mode_switch(s: &State) -> El {
    let tab = |key: &'static str, label: &'static str, plugins: bool| {
        let active = s.pedals.plugins.shown == plugins;
        el().key(key)
            .role("tab")
            .label(label)
            .focusable(true)
            .checked(active)
            .h(16.0)
            .px(5.0)
            .center()
            .radius(3.0)
            .fill(if active {
                ink("#2f3d34")
            } else {
                ink("#1b251f")
            })
            .border(
                1.0,
                if active {
                    ink("#c8f77b")
                } else {
                    ink("#3b4c42")
                },
            )
            .child(caption(label).color(if active {
                ink("#eef4d8")
            } else {
                ink("#8c9e98")
            }))
            .on_press(move |s: &mut State| s.pedals.plugins.shown = plugins)
    };
    row()
        .gap(3.0)
        .child(tab("pedal-mode-guitarpedal", "GP", false))
        .child(tab("pedal-mode-plugins", "PLUGIN", true))
}

fn small(key: String, label: &str, title: &str, color: Color, enabled: bool) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .focusable(enabled)
        .disabled(!enabled)
        .h(16.0)
        .px(5.0)
        .center()
        .radius(3.0)
        .fill(ink("#101a15"))
        .border(1.0, ink("#3b4c42"))
        .child(caption(title).color(if enabled { color } else { ink("#4c5a51") }))
}

fn chip(s: &State, pad: usize, index: usize, stage: &StageEdit) -> El {
    let plugins = &s.pedals.plugins.pads[pad];
    let focused = plugins.focus == Some(index);
    let count = plugins.stages.len();
    let color = if stage.problem.is_some() {
        ink("#ffad92")
    } else {
        accent(PADS[pad].sample)
    };
    let name = if stage.record.name.is_empty() {
        stage.record.plugin.id.clone()
    } else {
        stage.record.name.clone()
    };
    let mut chip = row()
        .key(format!("plugin-{index}"))
        .role("button")
        .label(format!("Plugin {index}: {name}"))
        .focusable(true)
        .h(18.0)
        .px(6.0)
        .gap(4.0)
        .center_y()
        .radius(3.0)
        .fill(if focused {
            ink("#2c3a32")
        } else {
            ink("#20302a")
        })
        .border(1.0, if focused { color } else { ink("#3b4c42") })
        .on_press(move |s: &mut State| s.pedals.plugins.pads[pad].focus = Some(index))
        .child(
            text(name.clone())
                .font(TextStyle::new(9.5, 650.0))
                .color(if focused { color } else { ink("#cbd7c7") }),
        );
    for (kind, title, enabled) in [("left", "‹", index > 0), ("right", "›", index + 1 < count)]
    {
        chip = chip.child(
            small(
                format!("plugin-{index}-{kind}"),
                if kind == "left" {
                    "Move earlier"
                } else {
                    "Move later"
                },
                title,
                ink("#cbd7c7"),
                enabled,
            )
            .on_press(move |s: &mut State| {
                let to = if kind == "left" { index - 1 } else { index + 1 };
                move_stage(s, pad, index, to);
            }),
        );
    }
    chip.child(
        small(
            format!("plugin-{index}-remove"),
            &format!("Remove {name}"),
            "×",
            ink("#ffad92"),
            true,
        )
        .on_press(move |s: &mut State| remove(s, pad, index)),
    )
}

fn field(
    edit: &TextEdit,
    key: &str,
    label: &str,
    width: f32,
    get: fn(&mut State) -> &mut TextEdit,
) -> El {
    text_input(edit, TextStyle::new(10.0, 500.0))
        .key(key)
        .label(label)
        .single_line()
        .w(width)
        .h(20.0)
        .px(6.0)
        .fill(ink("#28382f"))
        .on_edit(get)
}

fn path_field(s: &State) -> El {
    field(
        &s.pedals.plugins.path,
        "plugin-path",
        "Plugin path",
        260.0,
        |s| &mut s.pedals.plugins.path,
    )
}

fn add_row(s: &State, pad: usize) -> El {
    let busy = s.pedals.plugins.probing();
    row()
        .key("plugin-add-form")
        .gap(6.0)
        .h(34.0)
        .center_y()
        .child(path_field(s))
        .child(field(
            &s.pedals.plugins.plugin_id,
            "plugin-id",
            "Plugin ID",
            170.0,
            |s| &mut s.pedals.plugins.plugin_id,
        ))
        .child(
            button("plugin-load", "LOAD")
                .h(20.0)
                .px(8.0)
                .disabled(busy)
                .focusable(!busy)
                .on_press(move |s: &mut State| add(s, pad)),
        )
}

fn relink_row(s: &State, pad: usize, index: usize, stage: &StageEdit) -> El {
    let busy = s.pedals.plugins.probing();
    let mut row = row()
        .key("plugin-recover")
        .gap(6.0)
        .h(34.0)
        .center_y()
        .child(path_field(s))
        .child(
            button("plugin-relink", "LOCATE")
                .h(20.0)
                .px(8.0)
                .disabled(busy)
                .focusable(!busy)
                .on_press(move |s: &mut State| relink(s, pad, index)),
        );
    if stage.found.is_some() {
        row = row.child(
            button("plugin-adopt", "USE FOUND BUILD")
                .h(20.0)
                .px(8.0)
                .on_press(move |s: &mut State| adopt(s, pad, index)),
        );
    }
    row
}

fn param_slider(pad: usize, index: usize, info: &ParamInfo, value: f64) -> El {
    let span = (info.max - info.min).max(f64::MIN_POSITIVE);
    let normalized = ((value - info.min) / span).clamp(0.0, 1.0) as f32;
    let (w, h, k) = (92.0, 14.0, 10.0);
    let travel = w - k;
    let (id, min, max, stepped) = (info.id, info.min, info.max, info.stepped);
    let to_value = move |x: f32| min + span * f64::from(((x - k / 2.0) / travel).clamp(0.0, 1.0));
    let step = if stepped { 1.0 } else { span / 100.0 };
    let display = if stepped {
        format!("{value:.0}")
    } else {
        format!("{value:.3}")
    };
    col()
        .w(w)
        .child(
            caption(info.name.to_uppercase())
                .color(ink("#8c9e98"))
                .truncate(),
        )
        .child(
            caption(display.clone())
                .key(format!("plugin-param-{id}-value"))
                .color(if value == info.default {
                    ink("#7f9187")
                } else {
                    ink("#edf4dc")
                }),
        )
        .child(
            el().key(format!("plugin-param-{id}"))
                .role("slider")
                .label(info.name.clone())
                .focusable(true)
                .range_value(value, min, max)
                .range_step(step)
                .a11y_value(display)
                .size(w, h)
                .on_step(move |s: &mut State, steps, _| {
                    set_param(s, pad, index, id, value + step * f64::from(steps));
                })
                .on_step_limit(move |s: &mut State, end, _| {
                    set_param(s, pad, index, id, if end { max } else { min });
                })
                .on_drag(move |s: &mut State, d: &mut Drag, tx| match d.phase {
                    Phase::Down => {
                        d.data = normalized;
                        set_param(s, pad, index, id, to_value(d.x));
                    }
                    Phase::Move => {
                        set_param(s, pad, index, id, to_value(d.x));
                        tx.instant();
                    }
                    Phase::Up => {
                        if d.cancelled() {
                            set_param(s, pad, index, id, min + span * f64::from(d.data));
                        }
                    }
                })
                .child(
                    el().key("track")
                        .size(w, 4.0)
                        .absolute(0.0, 5.0)
                        .radius(2.0)
                        .fill(ink("#101b14"))
                        .border(1.0, ink("#394a3c"))
                        .a11y_hidden(),
                )
                .child(
                    el().key("knob")
                        .size(k, k)
                        .absolute(normalized * travel, 2.0)
                        .radius(3.0)
                        .gradient(ink("#eef4d8"), ink("#9eac8b"))
                        .border_gradient(1.0, ink("#fcffe8"), ink("#748069"))
                        .a11y_hidden(),
                ),
        )
}

fn focused(s: &State, pad: usize) -> El {
    let plugins = &s.pedals.plugins.pads[pad];
    let Some((index, stage)) = plugins
        .focus
        .and_then(|index| plugins.stages.get(index).map(|stage| (index, stage)))
    else {
        return row()
            .key("plugin-controls")
            .h(34.0)
            .center_y()
            .child(caption(
                "ADD A PLUGIN BY PATH AND ID; IT RENDERS OFFLINE FOR THE NEXT HIT",
            ));
    };
    if let Some(problem) = &stage.problem {
        return col()
            .gap(2.0)
            .child(
                caption(problem.clone())
                    .key("plugin-problem")
                    .role("alert")
                    .color(ink("#ffad92"))
                    .truncate(),
            )
            .child(relink_row(s, pad, index, stage));
    }
    let Some(controls) = &stage.controls else {
        return row()
            .key("plugin-controls")
            .h(34.0)
            .center_y()
            .child(caption("LOADING CONTROLS…"));
    };
    let mut row = row().key("plugin-controls").gap(10.0).h(34.0).scroll_x();
    for info in controls {
        let value = stage
            .record
            .params
            .iter()
            .find(|param| param.id == info.id)
            .map_or(info.default, |param| param.value.0);
        row = row.child(param_slider(pad, index, info, value));
    }
    row
}

/// The strip body in plugin mode.
pub fn strip(s: &State) -> El {
    let pad = s.selected;
    let plugins = &s.pedals.plugins.pads[pad];
    let mut chips = row()
        .key("plugin-chain")
        .gap(4.0)
        .h(18.0)
        .center_y()
        .child(mode_switch(s));
    for (index, stage) in plugins.stages.iter().enumerate() {
        chips = chips.child(chip(s, pad, index, stage));
    }
    let full = plugins.stages.len() >= MAX_STAGES;
    chips = chips.child(
        small(
            "plugin-add".into(),
            "Add a plugin",
            "+ PLUGIN",
            ink("#cbd7c7"),
            !full,
        )
        .on_press(|s: &mut State| s.pedals.plugins.adding = !s.pedals.plugins.adding),
    );
    if plugins.status == Status::Rendering {
        chips = chips.child(
            small(
                "plugin-cancel".into(),
                "Cancel render",
                "CANCEL",
                ink("#ffad92"),
                true,
            )
            .on_press(move |s: &mut State| cancel_work(s, pad)),
        );
    }
    if plugins.landed.is_some() && !is_current(s, pad) && plugins.status != Status::Rendering {
        chips = chips.child(
            small(
                "plugin-revert".into(),
                "Revert to the sound that plays",
                "REVERT",
                ink("#cbd7c7"),
                true,
            )
            .on_press(move |s: &mut State| revert(s, pad)),
        );
    }
    if let Some(message) = &s.pedals.plugins.message {
        chips = chips.child(
            caption(message.clone())
                .key("plugin-message")
                .role("status")
                .truncate(),
        );
    }
    let body = if s.pedals.plugins.adding {
        add_row(s, pad)
    } else {
        focused(s, pad)
    };
    col().key("plugin-strip").gap(4.0).child(chips).child(body)
}

/// A pad's `PLUG n` tag, drawn on its face when it has plugin stages.
pub fn tag(s: &State, pad: usize) -> Option<El> {
    let plugins = &s.pedals.plugins.pads[pad];
    let count = plugins.stages.len();
    if count == 0 {
        return None;
    }
    let failed = matches!(plugins.status, Status::Error(_))
        || plugins.stages.iter().any(|stage| stage.problem.is_some());
    Some(
        caption(format!("PLUG {count}"))
            .key(format!("pad-{pad}-plug"))
            .color(if failed {
                ink("#ffad92")
            } else {
                ink("#9fd6b4")
            }),
    )
}
