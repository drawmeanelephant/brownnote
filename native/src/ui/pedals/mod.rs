//! The per-pad pedal chain: catalog state, background renders and the
//! handlers that edit a chain. `view` draws it.
//!
//! A pad's chain renders off the UI thread with `rune::Latest` and lands in
//! `poll`. The pad's next hit plays the rendered sample; a voice already
//! playing keeps the sample it started with.
mod view;

use super::State;
use brownnote_native::{
    engine::{Command, Returned, SLOTS, Sample},
    kit::PADS,
    pedals::{
        brownnote::{self, CatalogError, DEFAULT_BIN},
        catalog::{Catalog, Effect, MixLaw},
        render::{self, Chain, RenderError, RenderJob, Rendered, Stage},
    },
};
use rune::Latest;
use std::{path::PathBuf, sync::Arc, time::Duration};
pub use view::{header, strip, tag};

/// The cache `--pedal-cache` defaults to, under `native/target/`.
pub fn default_cache() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/pedal-cache"))
}

/// Renders wait for a pause in editing before they start.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
/// Slots 0..5 hold the kit's files and never unload; renders take free slots.
pub const FIRST_RENDER_SLOT: usize = 5;

pub type CatalogFn = Arc<dyn Fn() -> Result<Catalog, CatalogError> + Send + Sync>;
pub type RenderFn = Arc<dyn Fn(&RenderJob) -> Result<Rendered, RenderError> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Shape,
    Pedals,
}

pub enum CatalogState {
    Loading,
    Ready(Arc<Catalog>),
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Base,
    Rendering,
    Ready { seconds: f32, clipped: bool },
    Error(String),
}

pub struct PadPedals {
    pub chain: Chain,
    /// The stage whose pots are shown.
    pub focus: Option<usize>,
    pub render: Latest<Result<Rendered, RenderError>>,
    pub status: Status,
    /// The slot a trigger reads right now.
    pub slot: usize,
    /// The rendered slot this pad is using, if any.
    pub rendered_slot: Option<usize>,
    /// A render that landed while every slot was busy.
    pub pending: Option<Rendered>,
}

pub struct Pedals {
    pub catalog: CatalogState,
    pub pads: [PadPedals; 16],
    pub tab: Tab,
    pub brownnote: PathBuf,
    pub cache: PathBuf,
    /// Set false by tests and scenes, which install fakes and never run a
    /// process.
    pub auto_load: bool,
    pub catalog_fn: CatalogFn,
    pub render_fn: RenderFn,
    /// An in-app pedal list, used where the platform has no native menus.
    pub add_open: bool,
    catalog_request: Latest<Result<Catalog, CatalogError>>,
}

impl Pedals {
    pub fn new(cache: PathBuf) -> Self {
        let bin = PathBuf::from(DEFAULT_BIN);
        let list_bin = bin.clone();
        let list_cache = cache.clone();
        let _ = list_cache;
        Self {
            catalog: CatalogState::Loading,
            pads: std::array::from_fn(|pad| PadPedals {
                chain: Chain::default(),
                focus: None,
                render: Latest::default().debounce(DEBOUNCE),
                status: Status::Base,
                slot: PADS[pad].sample,
                rendered_slot: None,
                pending: None,
            }),
            tab: Tab::Shape,
            brownnote: bin,
            cache,
            auto_load: true,
            catalog_fn: Arc::new(move || brownnote::list(&list_bin, Duration::from_secs(5))),
            render_fn: Arc::new(render::render),
            add_open: false,
            catalog_request: Latest::default(),
        }
    }

    pub fn ready(&self) -> Option<Arc<Catalog>> {
        match &self.catalog {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        }
    }

    /// Ask brownnote for the catalog once. `setup` calls this; tests and
    /// scenes install their own answer and leave `auto_load` false.
    pub fn load(&self) {
        if !self.auto_load {
            return;
        }
        let list = self.catalog_fn.clone();
        self.catalog_request.request((), move |()| list());
    }

    pub fn set_waker(&mut self, waker: rune::Waker) {
        self.catalog_request.set_waker(waker.clone());
        for pad in &self.pads {
            pad.render.set_waker(waker.clone());
        }
    }
}

const FIXTURE: &str = include_str!("../../../tests/fixtures/bench-list.txt");

pub enum SceneKind {
    Ready,
    Error,
    Unavailable,
}

/// Scene state built from the fixture; a scene never runs brownnote.
pub fn scene(s: &mut State, kind: SceneKind) {
    s.pedals.auto_load = false;
    s.pedals.tab = Tab::Pedals;
    s.selected = 4;
    match kind {
        SceneKind::Unavailable => {
            s.pedals.catalog =
                CatalogState::Unavailable(CatalogError::NoBench(String::new()).to_string());
        }
        SceneKind::Ready | SceneKind::Error => {
            let Ok(catalog) = Catalog::parse(FIXTURE) else {
                return;
            };
            s.pedals.catalog = CatalogState::Ready(Arc::new(catalog));
            s.pedals.pads[4].chain = Chain {
                stages: vec![
                    Stage {
                        effect: "Rat Sketch".into(),
                        pots: vec![("Distortion".into(), 90)],
                        mix: None,
                    },
                    Stage {
                        effect: "Tape Echo".into(),
                        pots: Vec::new(),
                        mix: Some(40),
                    },
                ],
            };
            s.pedals.pads[4].focus = Some(1);
            match kind {
                SceneKind::Error => {
                    s.pedals.pads[4].status =
                        Status::Error("Rat Sketch / Distortion takes 0..120, not 200".into());
                }
                _ => {
                    let sample = s.kit.samples[PADS[4].sample].clone();
                    let slot = FIRST_RENDER_SLOT;
                    s.slots.set(slot, Some(sample.clone()));
                    s.output.send(Command::Load { slot, sample });
                    s.pedals.pads[4].slot = slot;
                    s.pedals.pads[4].rendered_slot = Some(slot);
                    s.pedals.pads[4].status = Status::Ready {
                        seconds: 0.4,
                        clipped: false,
                    };
                }
            }
        }
    }
}

pub(super) fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or_default().to_string()
}

pub fn status_text(status: &Status) -> String {
    match status {
        Status::Base => "BASE SAMPLE".into(),
        Status::Rendering => "RENDERING…".into(),
        Status::Ready { seconds, clipped } => {
            let mut text = format!("READY {seconds:.1} s");
            if *clipped {
                text.push_str(" · CLIPPED");
            }
            text
        }
        Status::Error(message) => format!("ERROR · {}", first_line(message)),
    }
}

fn free_slot(s: &State) -> Option<usize> {
    (FIRST_RENDER_SLOT..SLOTS).find(|slot| s.slots.is_empty(*slot))
}

/// Point a pad back at its base file and release any rendered slot.
fn reset_to_base(s: &mut State, pad: usize) {
    if let Some(slot) = s.pedals.pads[pad].rendered_slot.take() {
        s.output.send(Command::Unload { slot });
    }
    s.pedals.pads[pad].slot = PADS[pad].sample;
    s.pedals.pads[pad].status = Status::Base;
    s.pedals.pads[pad].pending = None;
    s.rebuild_wave(pad, None);
}

/// Ask for a render of this pad's chain, or cancel it when the chain is empty.
pub fn request(s: &mut State, pad: usize) {
    if s.pedals.ready().is_none() {
        return;
    }
    let chain = s.pedals.pads[pad].chain.clone();
    if chain.stages.is_empty() {
        s.pedals.pads[pad].render.cancel();
        reset_to_base(s, pad);
        return;
    }
    let Some(catalog) = s.pedals.ready() else {
        return;
    };
    let source = s.kit.samples[PADS[pad].sample].clone();
    let (brownnote, cache, render_fn) = (
        s.pedals.brownnote.clone(),
        s.pedals.cache.clone(),
        s.pedals.render_fn.clone(),
    );
    s.pedals.pads[pad].status = Status::Rendering;
    s.pedals.pads[pad]
        .render
        .request_if_changed(&chain, move |chain| {
            let job = RenderJob {
                brownnote,
                cache,
                catalog,
                source,
                chain,
                timeout: Duration::from_secs(10),
            };
            render_fn(&job)
        });
}

pub(super) fn effect<'a>(catalog: &'a Catalog, name: &str) -> Option<&'a Effect> {
    catalog.effect(name)
}

pub fn add_stage(s: &mut State, pad: usize, name: &str) {
    let Some(catalog) = s.pedals.ready() else {
        return;
    };
    let Some(found) = effect(&catalog, name) else {
        return;
    };
    if !found.routable() {
        return;
    }
    let chain = &mut s.pedals.pads[pad].chain;
    if chain.stages.len() >= brownnote_native::pedals::render::MAX_STAGES
        || chain.stages.iter().any(|stage| stage.effect == found.name)
    {
        return;
    }
    chain.stages.push(Stage {
        effect: found.name.clone(),
        pots: Vec::new(),
        mix: None,
    });
    s.pedals.pads[pad].focus = Some(chain.stages.len() - 1);
    s.pedals.add_open = false;
    request(s, pad);
}

pub fn remove_stage(s: &mut State, pad: usize, index: usize) {
    let chain = &mut s.pedals.pads[pad].chain;
    if index >= chain.stages.len() {
        return;
    }
    chain.stages.remove(index);
    s.pedals.pads[pad].focus = if chain.stages.is_empty() {
        None
    } else {
        Some(index.min(chain.stages.len() - 1))
    };
    if chain.stages.is_empty() {
        s.pedals.pads[pad].focus = None;
    }
    request(s, pad);
}

pub fn move_stage(s: &mut State, pad: usize, from: usize, to: usize) {
    let chain = &mut s.pedals.pads[pad].chain;
    if from >= chain.stages.len() || to >= chain.stages.len() {
        return;
    }
    let stage = chain.stages.remove(from);
    chain.stages.insert(to, stage);
    s.pedals.pads[pad].focus = Some(to);
    request(s, pad);
}

pub fn set_pot(s: &mut State, pad: usize, index: usize, label: &str, value: u8) {
    let Some(catalog) = s.pedals.ready() else {
        return;
    };
    let Some(stage) = s.pedals.pads[pad].chain.stages.get_mut(index) else {
        return;
    };
    let Some(effect) = effect(&catalog, &stage.effect) else {
        return;
    };
    let Some(pot) = effect.pots.iter().find(|pot| pot.label == label) else {
        return;
    };
    let value = value.min(pot.max);
    match stage.pots.iter_mut().find(|(l, _)| l == label) {
        Some((_, current)) => *current = value,
        None => stage.pots.push((label.to_string(), value)),
    }
    request(s, pad);
}

pub fn set_mix(s: &mut State, pad: usize, index: usize, mix: Option<u8>) {
    let Some(catalog) = s.pedals.ready() else {
        return;
    };
    let Some(stage) = s.pedals.pads[pad].chain.stages.get_mut(index) else {
        return;
    };
    let Some(effect) = effect(&catalog, &stage.effect) else {
        return;
    };
    if effect.mix == MixLaw::None {
        return;
    }
    stage.mix = mix.map(|value| value.min(120));
    request(s, pad);
}

/// Land catalog answers, render answers and retired samples.
pub fn poll(s: &mut State) -> bool {
    let mut changed = false;
    if let Some(result) = s.pedals.catalog_request.take() {
        s.pedals.catalog = match result {
            Ok(catalog) => CatalogState::Ready(Arc::new(catalog)),
            Err(error) => CatalogState::Unavailable(error.to_string()),
        };
        changed = true;
    }
    let retired = s.output.take_retired();
    for item in retired {
        if let Returned::Retired { slot, .. } = item {
            s.slots.set(slot, None);
            changed = true;
        }
    }
    for pad in 0..16 {
        let landed = s.pedals.pads[pad].render.take();
        if let Some(result) = landed {
            changed = true;
            match result {
                Ok(rendered) => land(s, pad, rendered),
                Err(error) => {
                    s.pedals.pads[pad].status = Status::Error(error.to_string());
                }
            }
        }
        if s.pedals.pads[pad].pending.is_some() {
            let pending = s.pedals.pads[pad].pending.take();
            if let Some(rendered) = pending {
                land(s, pad, rendered);
            }
            changed = true;
        }
    }
    changed
}

fn land(s: &mut State, pad: usize, rendered: Rendered) {
    let Some(slot) = free_slot(s) else {
        // Retirements still in flight; try again next poll.
        s.pedals.pads[pad].pending = Some(rendered);
        return;
    };
    let seconds = rendered.sample.data.len() as f32 / rendered.sample.rate as f32;
    let clipped = rendered.clipped.is_some_and(|count| count > 0);
    s.slots.set(slot, Some(rendered.sample.clone()));
    s.output.send(Command::Load {
        slot,
        sample: rendered.sample.clone(),
    });
    if let Some(previous) = s.pedals.pads[pad].rendered_slot.replace(slot)
        && previous != slot
    {
        s.output.send(Command::Unload { slot: previous });
    }
    s.pedals.pads[pad].slot = slot;
    s.pedals.pads[pad].status = Status::Ready { seconds, clipped };
    s.rebuild_wave(pad, Some(&rendered.sample));
}

impl State {
    fn rebuild_wave(&mut self, pad: usize, sample: Option<&Sample>) {
        self.render_waves[pad] = sample.map(super::graphics::envelope);
    }

    pub fn pad_wave(&self, pad: usize) -> &super::graphics::Envelope {
        self.render_waves[pad]
            .as_ref()
            .unwrap_or(&self.waveforms[PADS[pad].sample])
    }
}
