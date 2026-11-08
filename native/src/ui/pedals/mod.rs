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

/// The runtime resolver chooses the default writable machine cache.
pub fn default_cache() -> PathBuf {
    brownnote_native::paths::DEFAULT_CACHE.into()
}

/// The `brownnote` the app runs when `--brownnote` is absent.
pub fn default_bin() -> PathBuf {
    PathBuf::from(DEFAULT_BIN)
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
    /// A render that landed while every slot was busy, with the chain it was
    /// rendered from so a newer edit can discard it.
    pub pending: Option<(Chain, Rendered)>,
    /// The chain the last render was asked for. Comparing against it is how an
    /// edit that changes nothing keeps the answer it already has.
    requested: Option<Chain>,
    /// The chain the current slot was rendered from, and whether it clipped.
    landed: Option<(Chain, bool)>,
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
    /// Rendered slots the mixer still holds for us, waiting for an `Unload`
    /// the command queue had no room for.
    pub unloads: Vec<usize>,
    catalog_request: Latest<Result<Catalog, CatalogError>>,
}

impl Pedals {
    /// `bin` is both the render executable and the one the catalog is read
    /// from, so `--brownnote PATH` cannot load one build's pedals and render
    /// with another.
    pub fn with_bin(cache: PathBuf, bin: PathBuf) -> Self {
        let list_bin = bin.clone();
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
                requested: None,
                landed: None,
            }),
            tab: Tab::Shape,
            brownnote: bin,
            cache,
            auto_load: true,
            catalog_fn: Arc::new(move || brownnote::list(&list_bin, Duration::from_secs(5))),
            render_fn: Arc::new(render::render),
            add_open: false,
            unloads: Vec::new(),
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
                    let chain = s.pedals.pads[4].chain.clone();
                    s.pedals.pads[4].requested = Some(chain.clone());
                    s.pedals.pads[4].landed = Some((chain, false));
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
        queue_unload(s, slot);
    }
    s.pedals.pads[pad].slot = PADS[pad].sample;
    s.pedals.pads[pad].status = Status::Base;
    s.pedals.pads[pad].pending = None;
    s.pedals.pads[pad].requested = None;
    s.pedals.pads[pad].landed = None;
    s.rebuild_wave(pad, None);
}

/// Ask the mixer to drop a slot we no longer point at. It keeps holding the
/// sample until the voice reading it ends, so the slot only comes back through
/// `take_retired`. A full command queue must not lose the request.
fn queue_unload(s: &mut State, slot: usize) {
    if !s.pedals.unloads.contains(&slot) {
        s.pedals.unloads.push(slot);
    }
}

fn flush_unloads(s: &mut State) {
    let mut pending = std::mem::take(&mut s.pedals.unloads);
    pending.retain(|slot| !s.output.send(Command::Unload { slot: *slot }));
    s.pedals.unloads = pending;
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
    // An edit that leaves the chain identical has nothing new to ask for, and
    // saying RENDERING over an answer that is already in place would never
    // clear.
    if s.pedals.pads[pad].requested.as_ref() == Some(&chain) {
        return;
    }
    // An edit that goes back to the chain the pad already plays cancels the
    // request and puts its answer back, so Escape on a cancelled drag costs
    // nothing.
    if let Some((landed, clipped)) = s.pedals.pads[pad].landed.clone()
        && landed == chain
    {
        s.pedals.pads[pad].render.cancel();
        s.pedals.pads[pad].requested = Some(chain);
        let (seconds, _) = s.pad_audio(pad);
        s.pedals.pads[pad].status = Status::Ready { seconds, clipped };
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
    s.pedals.pads[pad].requested = Some(chain.clone());
    s.pedals.pads[pad].render.request(chain, move |chain| {
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

/// Land catalog answers, render answers, unloads and retired samples.
pub fn poll(s: &mut State) -> bool {
    let mut changed = false;
    if let Some(result) = s.pedals.catalog_request.take() {
        s.pedals.catalog = match result {
            Ok(catalog) => CatalogState::Ready(Arc::new(catalog)),
            Err(error) => CatalogState::Unavailable(error.to_string()),
        };
        changed = true;
    }
    flush_unloads(s);
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
                Ok(rendered) => {
                    // This answer is newer than anything waiting for a slot, and
                    // it belongs to the newest request.
                    let chain = s.pedals.pads[pad]
                        .requested
                        .clone()
                        .unwrap_or_else(|| s.pedals.pads[pad].chain.clone());
                    s.pedals.pads[pad].pending = None;
                    land(s, pad, Some(chain), rendered);
                }
                Err(error) => {
                    s.pedals.pads[pad].status = Status::Error(error.to_string());
                }
            }
        }
        retry_pending(s, pad);
    }
    changed
}

/// Try a render that was waiting for a free slot. Its chain must still be the
/// one the pad holds, or it would put the sound of an abandoned edit back.
fn retry_pending(s: &mut State, pad: usize) {
    let Some((chain, _)) = s.pedals.pads[pad].pending.as_ref() else {
        return;
    };
    if chain != &s.pedals.pads[pad].chain {
        s.pedals.pads[pad].pending = None;
        return;
    }
    let Some((chain, rendered)) = s.pedals.pads[pad].pending.take() else {
        return;
    };
    land(s, pad, Some(chain), rendered);
}

/// Give a render a slot and point the pad at it, or leave it waiting for a
/// later poll when the slot table or the command queue has no room. Nothing is
/// committed until the mixer has accepted the sample, so the slot stays
/// provably free for the retry.
fn land(s: &mut State, pad: usize, chain: Option<Chain>, rendered: Rendered) {
    let hold = |s: &mut State, rendered: Rendered| {
        let chain = chain
            .clone()
            .unwrap_or_else(|| s.pedals.pads[pad].chain.clone());
        s.pedals.pads[pad].pending = Some((chain, rendered));
    };
    let Some(slot) = free_slot(s) else {
        hold(s, rendered);
        return;
    };
    // A `Load` dropped on a full queue would leave the slot reserved by the UI
    // and empty in the mixer, which is a silent pad for good.
    if !s.output.send(Command::Load {
        slot,
        sample: rendered.sample.clone(),
    }) {
        hold(s, rendered);
        return;
    }
    let seconds = rendered.sample.data.len() as f32 / rendered.sample.rate as f32;
    let clipped = rendered.clipped.is_some_and(|count| count > 0);
    s.slots.set(slot, Some(rendered.sample.clone()));
    if let Some(previous) = s.pedals.pads[pad].rendered_slot.replace(slot)
        && previous != slot
    {
        queue_unload(s, previous);
        flush_unloads(s);
    }
    s.pedals.pads[pad].slot = slot;
    s.pedals.pads[pad].status = Status::Ready { seconds, clipped };
    s.pedals.pads[pad].landed = chain.map(|chain| (chain, clipped));
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
