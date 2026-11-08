mod audio;
mod control;
mod graphics;
pub(crate) mod pedals;
mod view;
use brownnote_native::{
    engine::{Command, Slots},
    kit::{DEFAULT_SAMPLES, Kit, PADS},
    output::Output,
};
use graphics::{Envelope, envelope};
#[allow(unused_imports)]
pub use pedals::{CatalogState, FIRST_RENDER_SLOT, Status, Tab};
use rune::{Color, El, Key, KeyEvent, TextStyle, Tx, el, text};
use std::{path::Path, sync::atomic::Ordering::Relaxed, time::Duration};
pub use view::view;

#[derive(Clone, Copy)]
pub struct Settings {
    pub gain: f32,
    pub pan: f32,
    pub pitch: f32,
}

pub struct State {
    pub kit: Kit,
    pub output: Output,
    /// What each sample slot holds, kept so a fresh `Output` can be rebuilt
    /// after a device failure.
    pub slots: Slots,
    pub settings: [Settings; 16],
    pub selected: usize,
    pub master: f32,
    pub choke: bool,
    pub counts: [u32; 16],
    pub peak: f32,
    pub pedals: pedals::Pedals,
    /// The waveform of each pad's rendered sample, if it has one.
    pub render_waves: [Option<graphics::Envelope>; 16],
    now: f64,
    flashes: [f64; 16],
    wake: Option<rune::Waker>,
    waveforms: [Envelope; 5],
}

impl State {
    pub fn new(kit: Kit, headless: bool, no_audio: bool) -> Self {
        Self::with_cache(
            kit,
            headless,
            no_audio,
            pedals::default_cache(),
            pedals::default_bin(),
        )
    }

    pub fn with_cache(
        kit: Kit,
        headless: bool,
        no_audio: bool,
        pedal_cache: std::path::PathBuf,
        brownnote: std::path::PathBuf,
    ) -> Self {
        let waveforms = std::array::from_fn(|i| envelope(&kit.samples[i]));
        let pedals = pedals::Pedals::with_bin(pedal_cache, brownnote);
        let slots = Slots::from(kit.samples.clone());
        let output = if headless || no_audio {
            Output::virtual_driver(
                slots.clone(),
                if headless {
                    "Headless: no audio device".into()
                } else {
                    "Audio off (--no-audio)".into()
                },
                false,
            )
        } else {
            Output::open(slots.clone())
                .unwrap_or_else(|e| Output::virtual_driver(slots.clone(), e.to_string(), true))
        };
        Self {
            kit,
            output,
            slots,
            settings: std::array::from_fn(|i| Settings {
                gain: 1.0,
                pan: 0.0,
                pitch: PADS[i].semitones,
            }),
            selected: 0,
            master: 0.8,
            choke: true,
            counts: [0; 16],
            peak: 0.0,
            pedals,
            render_waves: std::array::from_fn(|_| None),
            now: 0.0,
            flashes: [0.0; 16],
            wake: None,
            waveforms,
        }
    }

    pub fn trigger(&mut self, pad: usize, velocity: f32) {
        let spec = &PADS[pad];
        if self.kit.errors[spec.sample].is_some() {
            return;
        }
        let settings = self.settings[pad];
        self.output.send(Command::Trigger {
            pad,
            slot: self.pedals.pads[pad].slot,
            group: spec.sample,
            velocity,
            gain: settings.gain,
            pan: settings.pan,
            semitones: settings.pitch,
        });
        self.selected = pad;
        self.flashes[pad] = self.now + 0.120;
    }

    /// The length and rate of what this pad triggers now: its rendered sample
    /// when it has one, otherwise the kit file.
    pub fn pad_audio(&self, pad: usize) -> (f32, u32) {
        let sample = self
            .slots
            .get(self.pedals.pads[pad].slot)
            .unwrap_or(&self.kit.samples[PADS[pad].sample]);
        (sample.data.len() as f32 / sample.rate as f32, sample.rate)
    }
}

pub fn init() -> State {
    State::new(Kit::load(Path::new(DEFAULT_SAMPLES)), true, true)
}

pub fn setup(s: &mut State, wake: rune::Waker) {
    s.output.set_waker(wake.clone());
    s.pedals.set_waker(wake.clone());
    s.wake = Some(wake);
    s.pedals.load();
}

pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if event.mods.cmd || event.mods.ctrl || event.mods.alt || event.mods.logo {
        return false;
    }
    let pad = match &event.key {
        Key::Text(key) => PADS
            .iter()
            .position(|p| key.eq_ignore_ascii_case(&p.key.to_string())),
        _ => None,
    };
    if pad.is_none() && event.key != Key::Escape {
        return false;
    }
    if tx.key_repeat() {
        return true;
    }
    if event.key == Key::Escape {
        s.output.send(Command::Panic);
    } else if let Some(pad) = pad {
        s.trigger(pad, 1.0);
    }
    tx.wake_after(Duration::from_millis(1));
    true
}

pub fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let elapsed = (clock - s.now).max(0.0);
    let was_lit = s.flashes.map(|until| until > s.now);
    s.now = clock;
    let failed = s.recover_stream_error();
    s.output.advance(elapsed);
    let pedals_changed = pedals::poll(s);
    let counts = s.output.meters.counts();
    let fresh_peak = f32::from_bits(s.output.meters.peak.swap(0, Relaxed));
    let peak = fresh_peak.max(s.peak * (-elapsed as f32 * 7.0).exp());
    let peak = if peak < 0.001 { 0.0 } else { peak };
    let lit = s.flashes.map(|until| until > clock);
    let changed = failed
        || pedals_changed
        || counts != s.counts
        || peak != s.peak
        || was_lit != lit
        || (elapsed > 0.0 && was_lit.contains(&true));
    s.counts = counts;
    s.peak = peak;
    if lit.contains(&true) || counts.iter().any(|n| *n > 0) || peak > 0.0 {
        tx.wake_after(Duration::from_millis(33));
    }
    changed
}

type Scene = (&'static str, fn(&mut State));
pub const SCENES: &[Scene] = &[
    ("playing", |s| {
        for pad in [0, 4, 10] {
            s.trigger(pad, 1.0);
        }
    }),
    ("missing-samples", |s| {
        *s = State::new(
            Kit::load(Path::new("/nonexistent/brownnote-samples")),
            true,
            true,
        );
    }),
    ("pedals", |s| pedals::scene(s, pedals::SceneKind::Ready)),
    ("pedal-error", |s| {
        pedals::scene(s, pedals::SceneKind::Error)
    }),
    ("pedals-unavailable", |s| {
        pedals::scene(s, pedals::SceneKind::Unavailable)
    }),
    ("no-audio", |s| {
        s.output = Output::virtual_driver(
            s.kit.samples.clone(),
            "Audio off (--no-audio)".into(),
            false,
        );
    }),
];

fn ink(hex: &str) -> Color {
    rune::color::hex(hex).unwrap_or(Color::WHITE)
}

fn caption(value: impl Into<String>) -> El {
    text(value.into())
        .font(TextStyle::new(10.0, 550.0))
        .color(ink("#8c9e98"))
}

fn accent(sample: usize) -> Color {
    ink(["#c8f77b", "#ffac7e", "#73d9c1", "#b5a3f5", "#f097bf"][sample])
}

fn button(key: &str, title: &str) -> El {
    el().key(key)
        .role("button")
        .label(title)
        .focusable(true)
        .h(32.0)
        .px(12.0)
        .center()
        .radius(6.0)
        .gradient(ink("#303b37"), ink("#202a26"))
        .border_gradient(1.0, ink("#526059"), ink("#2b3631"))
        .shadow(2.0, 4.0, ink("#080e0b"))
        .hover(ink("#425249"))
        .child(
            text(title)
                .font(TextStyle::new(11.0, 650.0))
                .color(ink("#eeeeed")),
        )
}
