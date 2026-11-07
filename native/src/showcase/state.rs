//! The showcase instrument: the real kit, engine and output, plus the look's motion state. Audio
//! semantics are the main app's: the same commands, keys, velocity curve, choke and panic.

use brownnote_native::{
    engine::Command,
    kit::{DEFAULT_SAMPLES, Kit, PADS},
    look::{
        controls::{Bins, Skin, bins},
        fx::{ALL_KINDS, Settling},
        motion::{Meter, PadLight, Ripple, Ripples},
        theme::{FxLevel, Theme, ThemeId},
    },
    output::Output,
};
use rune::{Key, KeyEvent, Tx};
use std::{path::Path, sync::atomic::Ordering::Relaxed, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub gain: f32,
    pub pan: f32,
    pub pitch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Play,
    Lab,
}

/// A scheduled input for scenes and measurement runs, fired from `poll` through the same paths
/// as keys and pointer hits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cue {
    Hit {
        pad: usize,
        velocity: f32,
        at: [f32; 2],
    },
    Panic,
}

pub struct State {
    pub kit: Kit,
    pub output: Output,
    pub settings: [Settings; 16],
    pub selected: usize,
    pub master: f32,
    pub choke: bool,
    pub counts: [u32; 16],
    pub peak: f32,
    pub meter: Meter,
    pub now: f64,
    pub theme: ThemeId,
    pub fx: FxLevel,
    /// Forced on by `--reduced-motion`; the system setting also applies.
    pub reduced_motion: bool,
    pub mask: u32,
    pub view: View,
    pub lights: [PadLight; 16],
    pub ripples: Ripples,
    pub hover: Option<usize>,
    pub held: Option<usize>,
    /// When each pad was last hit, for the display's playhead.
    pub played: [f64; 16],
    pub cues: Vec<(f64, Cue)>,
    pub waves: [Bins; 5],
    wake: Option<rune::Waker>,
}

pub struct Options {
    pub theme: ThemeId,
    pub fx: FxLevel,
    pub reduced_motion: bool,
    pub mask: u32,
    pub view: View,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            theme: ThemeId::Bunker,
            fx: FxLevel::Full,
            reduced_motion: false,
            mask: ALL_KINDS,
            view: View::Play,
        }
    }
}

impl State {
    pub fn new(kit: Kit, output: Output, options: &Options) -> Self {
        let waves = std::array::from_fn(|i| bins(&kit.samples[i].data));
        Self {
            kit,
            output,
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
            meter: Meter::default(),
            now: 0.0,
            theme: options.theme,
            fx: options.fx,
            reduced_motion: options.reduced_motion,
            mask: options.mask,
            view: options.view,
            lights: [PadLight::default(); 16],
            ripples: Ripples::default(),
            hover: None,
            held: None,
            played: [f64::NEG_INFINITY; 16],
            cues: Vec::new(),
            waves,
            wake: None,
        }
    }

    pub fn headless(options: &Options) -> Self {
        let kit = Kit::load(Path::new(DEFAULT_SAMPLES));
        let output = Output::virtual_driver(
            kit.samples.clone(),
            "Headless: no audio device".into(),
            false,
        );
        State::new(kit, output, options)
    }

    pub fn theme(&self) -> &'static Theme {
        self.theme.theme()
    }

    pub fn skin(&self, system_reduced_motion: bool) -> Skin<'static> {
        Skin {
            theme: self.theme(),
            fx: self.fx,
            reduced_motion: self.reduced_motion || system_reduced_motion,
            mask: self.mask,
        }
    }

    /// The main app's trigger, plus light and ripple. `at` is where on the pad it was hit.
    pub fn trigger(&mut self, pad: usize, velocity: f32, at: [f32; 2]) {
        let spec = &PADS[pad];
        if self.kit.errors[spec.sample].is_some() {
            return;
        }
        let settings = self.settings[pad];
        self.output.send(Command::Trigger {
            pad,
            slot: spec.sample,
            group: spec.sample,
            velocity,
            gain: settings.gain,
            pan: settings.pan,
            semitones: settings.pitch,
        });
        self.selected = pad;
        let motion = &self.theme().motion;
        self.lights[pad].hit(self.now, velocity, motion);
        self.ripples.hit(
            self.now,
            Ripple {
                on: pad,
                at,
                velocity,
            },
        );
        self.played[pad] = self.now;
    }

    pub fn panic(&mut self) {
        self.output.send(Command::Panic);
        let motion = self.theme().motion;
        for light in &mut self.lights {
            light.release(self.now, &motion);
        }
    }

    pub fn retry(&mut self) {
        self.output = Output::open(self.kit.samples.clone()).unwrap_or_else(|e| {
            Output::virtual_driver(self.kit.samples.clone(), e.to_string(), true)
        });
        self.output.send(Command::SetMaster(self.master));
        self.output.send(Command::SetChoke(self.choke));
        if let Some(wake) = &self.wake {
            self.output.set_waker(wake.clone());
        }
    }

    pub fn cue(&mut self, at: f64, cue: Cue) {
        let i = self.cues.partition_point(|(t, _)| *t <= at);
        self.cues.insert(i, (at, cue));
    }

    /// Playback position 0..1 of the selected pad's latest hit while its voice plays.
    pub fn playhead(&self) -> Option<f32> {
        let pad = self.selected;
        if self.counts[pad] == 0 {
            return None;
        }
        let sample = &self.kit.samples[PADS[pad].sample];
        let seconds = sample.data.len() as f64 / f64::from(sample.rate.max(1));
        let speed = 2f64.powf(f64::from(self.settings[pad].pitch) / 12.0);
        let at = (self.now - self.played[pad]) * speed / seconds.max(1e-6);
        (0.0..=1.0).contains(&at).then_some(at as f32)
    }
}

impl Settling for State {
    fn settling(&self, now: f64) -> bool {
        let m = &self.theme().motion;
        self.lights.iter().any(|l| !l.settled(now, m))
            || !self.ripples.settled(now, m.ripple_life)
            || !self.meter.settled()
            || self.counts.iter().any(|n| *n > 0)
    }
}

pub fn setup(s: &mut State, wake: rune::Waker) {
    s.output.set_waker(wake.clone());
    s.wake = Some(wake);
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
    crate::latency::input(tx);
    if event.key == Key::Escape {
        s.panic();
    } else if let Some(pad) = pad {
        s.trigger(pad, 1.0, [0.5, 0.5]);
    }
    tx.wake_after(Duration::from_millis(1));
    true
}

pub fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let elapsed = (clock - s.now).max(0.0);
    let was_settling = s.settling(s.now);
    s.now = clock;
    let mut cued = false;
    while s.cues.first().is_some_and(|(t, _)| *t <= clock) {
        let (_, cue) = s.cues.remove(0);
        cued = true;
        match cue {
            Cue::Hit { pad, velocity, at } => s.trigger(pad, velocity, at),
            Cue::Panic => s.panic(),
        }
    }
    let error = s.output.take_error();
    let failed = error.is_some();
    if let Some(error) = error {
        s.output =
            Output::virtual_driver(s.kit.samples.clone(), format!("Audio error: {error}"), true);
        s.output.send(Command::SetMaster(s.master));
        s.output.send(Command::SetChoke(s.choke));
    }
    s.output.advance(elapsed);
    let counts = s.output.meters.counts();
    let fresh = f32::from_bits(s.output.meters.peak.swap(0, Relaxed));
    let motion = s.theme().motion;
    s.meter.feed(clock, fresh, &motion);
    for (pad, light) in s.lights.iter_mut().enumerate() {
        light.voices(clock, counts[pad] > 0, &motion);
    }
    s.ripples.prune(clock, motion.ripple_life);
    let changed = failed || cued || counts != s.counts || s.meter.level != s.peak;
    s.counts = counts;
    s.peak = s.meter.level;
    let settling = s.settling(clock);
    if settling {
        // Lights, ripples and the meter step on the frame clock: ask for the next frame so they
        // move at the display's rate. A fixed 16 ms wake drew them at 60 Hz on a 120 Hz panel.
        tx.wake_after(Duration::ZERO);
    } else if let Some((at, _)) = s.cues.first() {
        tx.wake_after(Duration::from_secs_f64((at - clock).max(0.001)));
    }
    changed || settling || was_settling
}

type Scene = (&'static str, fn(&mut State));

fn hit(pad: usize, velocity: f32) -> Cue {
    Cue::Hit {
        pad,
        velocity,
        at: [0.5, 0.25 + 0.5 * (1.0 - velocity) / 0.6],
    }
}

/// The comparison scenes. Each starts at t = 0; capture times are in the capture script.
pub const SCENES: &[Scene] = &[
    ("idle", |_| {}),
    ("soft-hit", |s| s.cue(0.0, hit(5, 0.45))),
    ("hard-hit", |s| s.cue(0.0, hit(5, 1.0))),
    ("overlap", |s| {
        for (t, pad, v) in [
            (0.00, 0, 0.9),
            (0.00, 10, 1.0),
            (0.04, 5, 0.6),
            (0.08, 0, 0.7),
            (0.12, 15, 0.5),
            (0.16, 0, 1.0),
            (0.18, 6, 0.8),
            (0.22, 0, 0.55),
        ] {
            s.cue(t, hit(pad, v));
        }
    }),
    ("panic", |s| {
        for pad in [0, 4, 9, 14] {
            s.cue(0.0, hit(pad, 1.0));
        }
        s.cue(0.30, Cue::Panic);
    }),
    ("focus", |_| {}),
    ("missing-samples", |s| {
        let options = Options {
            theme: s.theme,
            fx: s.fx,
            reduced_motion: s.reduced_motion,
            mask: s.mask,
            view: s.view,
        };
        let kit = Kit::load(Path::new("/nonexistent/brownnote-samples"));
        let output = Output::virtual_driver(
            kit.samples.clone(),
            "Headless: no audio device".into(),
            false,
        );
        *s = State::new(kit, output, &options);
    }),
    ("audio-failure", |s| {
        s.output = Output::virtual_driver(
            s.kit.samples.clone(),
            "Audio error: The requested audio device is not available. It may have been disconnected."
                .into(),
            true,
        );
    }),
    ("storm", |s| {
        // Worst case for the effect stack: every pad hit every 120 ms for 2 s, so the ripple
        // budget stays full while the meter and all lights are live.
        for k in 0..66 {
            for lane in 0..4 {
                let pad = (k * 4 + lane) % 16;
                let velocity = 0.45 + 0.55 * ((k + lane) % 5) as f32 / 4.0;
                s.cue(f64::from(k as u32) * 0.03, hit(pad, velocity));
            }
        }
    }),
    ("performance", |s| {
        // The motion clips: a soft hit, a hard hit, a fast repeat with overlaps, a chord, panic.
        for (t, cue) in [
            (0.15, hit(5, 0.45)),
            (0.60, hit(5, 1.0)),
            (1.00, hit(0, 0.9)),
            (1.06, hit(0, 0.7)),
            (1.12, hit(0, 1.0)),
            (1.14, hit(10, 0.8)),
            (1.20, hit(15, 0.55)),
            (1.70, hit(3, 1.0)),
            (1.70, hit(6, 1.0)),
            (1.70, hit(9, 1.0)),
            (1.70, hit(12, 1.0)),
            (2.05, Cue::Panic),
        ] {
            s.cue(t, cue);
        }
    }),
    ("lab", |s| {
        s.view = View::Lab;
        for (pad, velocity) in [(0, 1.0), (1, 1.0), (2, 1.0), (3, 1.0), (4, 1.0), (5, 1.0)] {
            s.cue(0.0, hit(pad, velocity));
        }
    }),
];
