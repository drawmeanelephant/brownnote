//! Motion as pure functions of time: light that rises fast and falls smoothly, meter ballistics
//! and bounded one-shot ripples. Views, tests and the compositor read the same numbers, and every
//! piece says when it has settled so the window can stop drawing.

use crate::look::theme::Motion;
use rune_fx::OneShots;

/// One 8-bit step: a light below this is dark.
pub const DARK: f32 = 1.0 / 255.0;

/// Ripples the whole window runs at once; a further hit anywhere retires the oldest with a short
/// fade. A fast double hit still shows two and a four-pad chord four, while a storm of hits
/// draws at most twice this many (the live ones and the ones fading out).
pub const RIPPLE_BUDGET: usize = 8;

/// A value that rises to its target over `attack` seconds (cubic ease-out, no overshoot) and falls
/// exponentially with time constant `release`. Retargeting starts from the current value, so it
/// never jumps.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Follower {
    from: f32,
    to: f32,
    at: f64,
}

impl Follower {
    pub fn value(&self, now: f64, attack: f32, release: f32) -> f32 {
        let dt = (now - self.at).max(0.0) as f32;
        if self.to >= self.from {
            let k = (dt / attack.max(1e-3)).min(1.0);
            self.from + (self.to - self.from) * (1.0 - (1.0 - k).powi(3))
        } else {
            self.to + (self.from - self.to) * (-dt / release.max(1e-3)).exp()
        }
    }

    pub fn set(&mut self, now: f64, to: f32, attack: f32, release: f32) {
        if to != self.to {
            *self = Follower {
                from: self.value(now, attack, release),
                to,
                at: now,
            };
        }
    }

    pub fn target(&self) -> f32 {
        self.to
    }
}

/// A pad's light: a velocity-scaled flash on each hit, plus a sustain held while its voice plays.
#[derive(Clone, Copy, Debug, Default)]
pub struct PadLight {
    hit_at: f64,
    velocity: f32,
    sustain: Follower,
}

/// How long after a hit the engine may take to report the voice before the sustain lets go.
const VOICE_GRACE: f64 = 0.06;

impl PadLight {
    pub fn hit(&mut self, now: f64, velocity: f32, m: &Motion) {
        self.hit_at = now;
        self.velocity = velocity.clamp(0.0, 1.0);
        self.sustain
            .set(now, m.sustain * self.velocity, m.attack, m.voice_release);
    }

    /// Feed the engine's voice count for this pad.
    pub fn voices(&mut self, now: f64, playing: bool, m: &Motion) {
        if !playing && now - self.hit_at > VOICE_GRACE {
            self.sustain.set(now, 0.0, m.attack, m.voice_release);
        }
    }

    /// Panic: let go of the sustain now (the flash still finishes its smooth release).
    pub fn release(&mut self, now: f64, m: &Motion) {
        self.sustain.set(now, 0.0, m.attack, m.voice_release);
    }

    pub fn flash(&self, now: f64, m: &Motion) -> f32 {
        let age = (now - self.hit_at) as f32;
        if self.velocity <= 0.0 || age < 0.0 {
            return 0.0;
        }
        let rise = (age / m.attack.max(1e-3)).min(1.0);
        let rise = 1.0 - (1.0 - rise).powi(3);
        self.velocity * rise * (-(age - m.attack).max(0.0) / m.release.max(1e-3)).exp()
    }

    pub fn level(&self, now: f64, m: &Motion) -> f32 {
        let level = self
            .flash(now, m)
            .max(self.sustain.value(now, m.attack, m.voice_release));
        if level < DARK { 0.0 } else { level.min(1.0) }
    }

    pub fn settled(&self, now: f64, m: &Motion) -> bool {
        self.sustain.target() == 0.0 && self.level(now, m) == 0.0
    }
}

/// Peak meter ballistics: instant attack, exponential fall, a held peak and a latched clip light.
#[derive(Clone, Copy, Debug, Default)]
pub struct Meter {
    pub level: f32,
    /// The held peak (linear). It stays where it landed and fades out in place.
    pub hold: f32,
    /// How lit the held peak's marker is, 0..1.
    pub hold_light: f32,
    hold_at: f64,
    clip_at: Option<f64>,
    now: f64,
}

pub const CLIP_LATCH: f64 = 1.5;
const FLOOR: f32 = 0.001;
const HOLD_FADE: f32 = 0.25;

impl Meter {
    /// Advance to `now` with the engine's peak since the last call (linear, 0..).
    pub fn feed(&mut self, now: f64, fresh: f32, m: &Motion) {
        let dt = (now - self.now).max(0.0) as f32;
        self.now = now;
        let fall = (-dt / m.meter_release.max(1e-3)).exp();
        self.level = fresh.max(self.level * fall);
        if self.level < FLOOR {
            self.level = 0.0;
        }
        if fresh >= self.hold && fresh >= FLOOR {
            self.hold = fresh;
            self.hold_at = now;
        }
        let faded = (now - self.hold_at - f64::from(m.peak_hold)).max(0.0) as f32;
        self.hold_light = (-faded / HOLD_FADE).exp();
        if self.hold < FLOOR || self.hold_light < 0.02 {
            self.hold = 0.0;
            self.hold_light = 0.0;
        }
        if fresh >= 0.999 {
            self.clip_at = Some(now);
        }
    }

    pub fn clipping(&self) -> bool {
        self.clip_at.is_some_and(|at| self.now - at < CLIP_LATCH)
    }

    pub fn settled(&self) -> bool {
        self.level == 0.0 && self.hold == 0.0 && !self.clipping()
    }
}

pub fn db(linear: f32) -> f32 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f32::NEG_INFINITY
    }
}

/// A ripple's start: which control, where on it it was hit (0..1 across, 0..1 down) and how
/// hard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ripple {
    pub on: usize,
    pub at: [f32; 2],
    pub velocity: f32,
}

/// One live ripple to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RippleShot {
    pub age: f32,
    pub at: [f32; 2],
    pub velocity: f32,
    pub opacity: f32,
}

/// The window's overlapping ripples, bounded by `RIPPLE_BUDGET` (rune-fx `OneShots`).
#[derive(Clone, Debug)]
pub struct Ripples(OneShots<Ripple>);

impl Default for Ripples {
    fn default() -> Self {
        Ripples(OneShots::new(RIPPLE_BUDGET))
    }
}

impl Ripples {
    pub fn hit(&mut self, now: f64, ripple: Ripple) {
        self.0.trigger(now as f32, ripple);
    }

    /// The live ripples on control `on`.
    pub fn live(&self, now: f64, life: f32, on: usize) -> impl Iterator<Item = RippleShot> + '_ {
        let t = now as f32;
        self.0
            .live(t, life)
            .filter(move |(shot, _)| shot.data.on == on)
            .map(move |(shot, opacity)| RippleShot {
                age: (t - shot.t0).max(0.0),
                at: shot.data.at,
                velocity: shot.data.velocity,
                opacity,
            })
    }

    /// How many ripples draw anywhere.
    pub fn drawn(&self, now: f64, life: f32) -> usize {
        self.0.live(now as f32, life).count()
    }

    pub fn prune(&mut self, now: f64, life: f32) {
        self.0.prune(now as f32, life);
    }

    pub fn settled(&self, now: f64, life: f32) -> bool {
        !self.0.is_animating(now as f32, life)
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::look::theme::BUNKER;

    const M: &Motion = &BUNKER.motion;

    #[test]
    fn light_attacks_fast_scales_with_velocity_and_releases_without_a_cut() {
        let mut soft = PadLight::default();
        let mut hard = PadLight::default();
        soft.hit(1.0, 0.4, M);
        hard.hit(1.0, 1.0, M);
        let peak = 1.0 + f64::from(M.attack);
        assert!(hard.level(peak, M) > 0.99);
        assert!((soft.level(peak, M) - 0.4).abs() < 0.01);
        assert!(
            hard.level(1.0 + f64::from(M.attack) / 2.0, M) > 0.8,
            "fast attack"
        );
        hard.voices(1.5, false, M);
        let mut last = hard.level(1.5, M);
        let mut t = 1.5;
        while !hard.settled(t, M) {
            t += 1.0 / 120.0;
            let next = hard.level(t, M);
            assert!(next <= last + 1e-6, "release never rises");
            assert!(last - next < 0.08, "release never cuts: {last} -> {next}");
            last = next;
            assert!(t < 4.0, "settles");
        }
    }

    #[test]
    fn sustain_holds_while_the_voice_plays_and_only_lets_go_after_grace() {
        let mut light = PadLight::default();
        light.hit(0.0, 1.0, M);
        light.voices(0.02, false, M);
        assert!(
            light.level(1.0, M) > M.sustain * 0.99,
            "engine not caught up yet"
        );
        light.voices(1.0, true, M);
        assert!(!light.settled(5.0, M));
        light.voices(5.0, false, M);
        assert!(light.level(5.0, M) > M.sustain * 0.99);
        assert!(light.settled(5.0 + 30.0 * f64::from(M.voice_release), M));
    }

    #[test]
    fn meter_attacks_instantly_holds_peak_latches_clip_then_settles() {
        let mut meter = Meter::default();
        meter.feed(0.0, 0.5, M);
        assert_eq!(meter.level, 0.5);
        meter.feed(0.1, 0.0, M);
        assert!(meter.level < 0.5 && meter.level > 0.1);
        assert_eq!((meter.hold, meter.hold_light), (0.5, 1.0));
        meter.feed(0.2, 1.0, M);
        assert!(meter.clipping());
        let mut t = 0.2;
        let mut light = meter.hold_light;
        while !meter.settled() {
            t += 1.0 / 60.0;
            meter.feed(t, 0.0, M);
            if meter.hold > 0.0 {
                assert_eq!(meter.hold, 1.0, "the held peak fades where it landed");
            }
            assert!(meter.hold_light <= light, "and only fades");
            light = meter.hold_light;
            assert!(t < 5.0);
        }
        assert!(t >= 0.2 + CLIP_LATCH);
        assert_eq!(db(0.0), f32::NEG_INFINITY);
        assert!((db(0.5) + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn ripples_overlap_are_bounded_across_the_window_and_settle() {
        let mut ripples = Ripples::default();
        let hit = |on| Ripple {
            on,
            at: [0.5, 0.5],
            velocity: 1.0,
        };
        ripples.hit(0.0, hit(0));
        ripples.hit(0.01, hit(0));
        assert_eq!(
            ripples.live(0.02, M.ripple_life, 0).count(),
            2,
            "a double hit"
        );
        assert_eq!(ripples.live(0.02, M.ripple_life, 1).count(), 0);
        for i in 0..40 {
            ripples.hit(0.02 + f64::from(i) * 0.005, hit(i as usize % 16));
        }
        let life = M.ripple_life;
        // The last hit lands at 0.215 s.
        let live: Vec<_> = (0..16)
            .flat_map(|on| ripples.live(0.25, life, on))
            .collect();
        assert_eq!(live.len(), ripples.drawn(0.25, life));
        assert!(live.len() <= 2 * RIPPLE_BUDGET, "{}", live.len());
        assert_eq!(
            live.iter().filter(|s| s.opacity >= 1.0).count(),
            RIPPLE_BUDGET
        );
        assert!(!ripples.settled(0.25, life));
        assert!(ripples.settled(0.22 + f64::from(life), life));
        ripples.prune(10.0, life);
        assert_eq!(ripples.drawn(10.0, life), 0);
    }
}
