//! Control builders styled from a `Theme`: pads, knobs, a fader, a meter, a waveform display and
//! buttons. They return plain `El`s; the instrument adds keys, handlers and state.

use crate::look::fx;
use crate::look::motion::{RippleShot, db};
use crate::look::theme::{FxLevel, Hex, Metal, Theme};
use rune::{
    Align, Color, El, MotionIntent, Shadow, Shape, Spring, TextStyle, Transition, canvas, col, el,
    row, text,
};

/// What draws: the theme, how much of the effect stack, and the motion preference.
#[derive(Clone, Copy)]
pub struct Skin<'a> {
    pub theme: &'a Theme,
    pub fx: FxLevel,
    pub reduced_motion: bool,
    /// Effect kinds allowed (bit `1 << kind`), to isolate one effect in measurement runs.
    pub mask: u32,
}

impl Skin<'_> {
    /// Whether this skin draws effect `kind`. `Light` keeps the still materials only.
    pub fn uses(&self, kind: u32) -> bool {
        let level = match self.fx {
            FxLevel::Full => true,
            FxLevel::Light => matches!(kind, fx::PLASTIC | fx::METAL),
            FxLevel::Off => false,
        };
        let motion = !(self.reduced_motion && kind == fx::RIPPLE);
        level && motion && self.mask & (1 << kind) != 0
    }
}

pub fn styled(value: impl Into<String>, style: TextStyle, color: Hex) -> El {
    text(value.into()).font(style).color(color.color())
}

pub fn caption(skin: &Skin, value: impl Into<String>) -> El {
    styled(value, skin.theme.type_.caption, skin.theme.palette.ink_dim)
}

/// A raised panel that stacks its children: the theme's face, bevel and drop shadow.
pub fn panel(skin: &Skin) -> El {
    let p = &skin.theme.palette;
    col()
        .radius(skin.theme.space.panel_radius)
        .gradient(p.panel[0].color(), p.panel[1].color())
        .border_gradient(1.0, p.edge[0].alpha(0.7), p.edge[1].alpha(0.9))
        .shadow(3.0, 10.0, p.shadow.alpha(0.55))
}

pub fn led(skin: &Skin, color: Hex, level: f32) -> El {
    let off = skin.theme.palette.inset.mix(color, 0.12);
    let on = level.clamp(0.0, 1.0);
    el().size(6.0, 6.0)
        .radius(3.0)
        .fill(off.mix(color, on).color())
        .shadow(0.0, 8.0 * on, color.alpha(0.7 * on))
        .motion(Spring::NONE)
        .a11y_hidden()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Danger,
    Toggle(bool),
}

pub fn button(skin: &Skin, key: &str, title: &str, tone: Tone) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let (face, ink) = match tone {
        Tone::Plain | Tone::Toggle(false) => (p.panel[0], p.ink),
        Tone::Danger => (p.panel[0].mix(p.danger, 0.22), p.ink),
        Tone::Toggle(true) => (p.panel[0].mix(p.accent, 0.28), p.ink),
    };
    let mut b = el()
        .key(key)
        .role("button")
        .label(title)
        .focusable(true)
        .h(30.0)
        .px(11.0)
        .center()
        .gap(6.0)
        .radius(t.space.control_radius)
        .gradient(
            face.mix(p.edge[0], 0.12).color(),
            face.mix(p.shadow, 0.12).color(),
        )
        .border_gradient(1.0, p.edge[0].alpha(0.8), p.edge[1].alpha(0.9))
        .shadow(1.5, 3.0, p.shadow.alpha(0.6))
        .hover(p.edge[0].alpha(0.10))
        .press(p.shadow.alpha(0.18))
        .focus_ring(2.0, 2.0, p.focus.color());
    if let Tone::Toggle(on) = tone {
        b = b
            .checked(on)
            .child(led(skin, p.accent, if on { 1.0 } else { 0.0 }));
    }
    b.child(styled(title, t.type_.caption, ink))
}

pub const BINS: usize = 96;
pub type Bins = [[f32; 2]; BINS];

/// Min/max per bin, normalized to the loudest sample; silence stays flat.
pub fn bins(data: &[f32]) -> Bins {
    let mut bins = [[0.0_f32; 2]; BINS];
    for (i, value) in data.iter().enumerate() {
        let bin = i * BINS / data.len();
        bins[bin][0] = bins[bin][0].min(*value);
        bins[bin][1] = bins[bin][1].max(*value);
    }
    let peak = bins.iter().flatten().fold(0.0_f32, |a, b| a.max(b.abs()));
    if peak > 0.0 {
        for value in bins.iter_mut().flatten() {
            *value /= peak;
        }
    }
    bins
}

pub struct Wave<'a> {
    pub bins: &'a Bins,
    pub ink: Color,
    pub grid: Option<Color>,
    pub stroke: f32,
}

pub fn waveform(w: &Wave) -> El {
    let (width, height) = (576.0, 60.0);
    let mut shape = Shape::new().view_box(width, height);
    if let Some(grid) = w.grid {
        for x in (0..=12).map(|i| i as f32 * 48.0) {
            shape = shape.stroke(vec![[x, 0.0], [x, height]], 0.5, grid);
        }
        for y in [10.0, 30.0, 50.0] {
            shape = shape.stroke(vec![[0.0, y], [width, y]], 0.5, grid);
        }
    }
    shape = shape.stroke(vec![[0.0, 30.0], [width, 30.0]], 0.7, w.ink.alpha(0.45));
    for (i, [low, high]) in w.bins.iter().enumerate() {
        let x = i as f32 * 6.0 + 3.0;
        let (top, bottom) = (30.0 - high * 27.0, 30.0 - low * 27.0);
        shape = shape.stroke(vec![[x, top], [x, bottom.max(top + 0.6)]], w.stroke, w.ink);
    }
    canvas(shape).w_full().grow(1.0).min_h(4.0).a11y_hidden()
}

/// The playback position over a waveform `width` points wide, `at` 0..1 across. It is its own
/// element rather than a stroke in the waveform, so it moves as one edge and the waveform's
/// pixels stay still.
pub fn playhead(at: f32, width: f32, color: Color, glow: bool) -> El {
    const W: f32 = 1.6;
    let line = el()
        .absolute(at.clamp(0.0, 1.0) * (width - W).max(0.0), 0.0)
        .w(W)
        .h_full()
        .fill(color)
        .motion(Spring::NONE)
        // It follows the voice, so a retrigger sends it back to the start mid-travel.
        .motion_intent(MotionIntent::IMPULSE)
        .a11y_hidden();
    if glow {
        line.shadow(0.0, 6.0, color.alpha(0.6))
    } else {
        line
    }
}

/// The sample display: LCD, smoked glass or dark glass by theme, ink on top.
pub fn display(skin: &Skin, key: &str, content: El) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let r = t.space.panel_radius - 2.0;
    let mut d = el()
        .key(key)
        .padding(12.0)
        .gap(18.0)
        .radius(r)
        .gradient(p.display[0].color(), p.display[1].color())
        .box_shadow(Shadow {
            x: 0.0,
            y: 2.0,
            blur: 8.0,
            spread: 0.0,
            color: Color::BLACK.alpha(0.55),
            inset: true,
        })
        .border_gradient(1.0, p.edge[1].alpha(0.9), p.edge[0].alpha(0.45));
    if t.materials.trim != Metal::None && skin.uses(fx::METAL) {
        d = d.child(fx::metal(t.materials.trim, 2.5, r));
    }
    if t.surfaces.display_grain > 0.0 && skin.uses(fx::GRAIN) {
        d = d.child(fx::grain(t.surfaces.display_grain, r));
    }
    d = d.child(content);
    if t.surfaces.glass && skin.fx != FxLevel::Off {
        d = d.child(
            el().absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .radius(r)
                .linear_gradient(
                    2.2,
                    [
                        (0.0, Color::WHITE.alpha(0.10)),
                        (0.42, Color::WHITE.alpha(0.035)),
                        (0.43, Color::WHITE.alpha(0.0)),
                        (1.0, Color::WHITE.alpha(0.0)),
                    ],
                )
                .a11y_hidden(),
        );
    }
    d
}

/// A blurred copy of display ink under the sharp one, like phosphor. Costs one blur pass.
pub fn bloom(skin: &Skin, sharp: El, soft: El) -> El {
    let k = skin.theme.surfaces.display_bloom;
    if k <= 0.0 || skin.fx != FxLevel::Full {
        return sharp;
    }
    el().grow(1.0)
        .min_w(0.0)
        .child(
            soft.absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .blur(2.2)
                .opacity(k)
                .a11y_hidden(),
        )
        .child(sharp)
}

pub struct PadView<'a> {
    pub index: usize,
    pub label: &'a str,
    pub key: char,
    pub light: Hex,
    /// Light envelope 0..1.
    pub level: f32,
    pub selected: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub missing: Option<&'a str>,
    pub voices: u32,
    pub pitch: f32,
    pub bins: &'a Bins,
    pub ripples: &'a [RippleShot],
}

pub fn pad(skin: &Skin, p: &PadView) -> El {
    let t = skin.theme;
    let pal = &t.palette;
    let r = t.space.pad_radius;
    let material = if p.missing.is_some() {
        t.materials.pad_missing
    } else {
        t.materials.pad
    };
    let level = if p.missing.is_some() { 0.0 } else { p.level };
    let face = if p.hovered && p.missing.is_none() {
        material.base.mix(pal.edge[0], 0.10)
    } else {
        material.base
    };
    let dome = material.dome - if p.pressed { 0.3 } else { 0.0 };
    let mut root = col()
        .key(format!("pad-{}", p.index))
        .role("button")
        .focusable(false)
        .label(format!(
            "{}{}",
            p.label,
            p.missing
                .map_or(String::new(), |e| format!(" · MISSING: {e}"))
        ))
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .padding(9.0)
        .gap(3.0)
        .radius(r)
        .motion(Spring::NONE)
        .scale(if p.pressed && !skin.reduced_motion {
            0.975
        } else {
            1.0
        })
        .scale_motion(Spring::snappy(0.14, 0.0))
        .shadow(
            if p.pressed { 1.0 } else { 3.0 },
            if p.pressed { 3.0 } else { 7.0 },
            pal.shadow.alpha(0.6),
        );
    if skin.uses(fx::PLASTIC) {
        root = root.child(fx::plastic(&material, face, dome, r));
    } else {
        root = root
            .gradient(
                face.mix(pal.edge[0], 0.10).color(),
                face.mix(pal.shadow, if p.pressed { 0.05 } else { 0.16 })
                    .color(),
            )
            .border_gradient(1.0, pal.edge[0].alpha(0.5), pal.edge[1].alpha(0.8));
    }
    // The hit lights a fill over the moulding instead of tinting the moulding: the material
    // stays still, and the attack and release stay a colour the motion checks can follow (an
    // effect's packed colour reads to them as a jump on every step).
    root = root.child(
        el().absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .radius(r)
            .fill(p.light.alpha(level * t.surfaces.lit_face))
            .motion_intent(MotionIntent::PULSE)
            .a11y_hidden(),
    );
    if !skin.uses(fx::GLOW) && level > 0.0 && skin.fx != FxLevel::Off {
        root = root.shadow(0.0, 18.0 * level, p.light.alpha(0.5 * level));
    } else if skin.fx == FxLevel::Off && level > 0.0 {
        root = root.shadow(0.0, 14.0 * level, p.light.alpha(0.35 * level));
    }
    if skin.uses(fx::RIPPLE) {
        // One container whether or not ripples run, so the labels after it keep their place
        // among the pad's children and never fade out and back in on a hit.
        root = root.child(
            el().absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .radius(r)
                .a11y_hidden()
                .children(p.ripples.iter().map(|shot| {
                    fx::ripple(
                        p.light,
                        shot.age,
                        shot.at,
                        shot.velocity,
                        shot.opacity * t.motion.ripple_opacity,
                        t.motion.ripple_life,
                        r,
                    )
                })),
        );
    }
    if p.selected {
        root = root.outline(2.0, 2.0, p.light.mix(pal.ink, 0.15).color());
    }
    let ink = if p.missing.is_some() {
        pal.on_pad_dim
    } else {
        pal.on_pad
    };
    // Captions brighten with the face so a lit pad never loses its small print.
    let small = pal.on_pad_dim.mix(pal.on_pad, level);
    let keycap = el()
        .size(19.0, 17.0)
        .center()
        .radius(4.0)
        .fill(face.mix(pal.shadow, 0.18).alpha(0.9))
        .border(1.0, pal.edge[1].alpha(0.35))
        .child(styled(
            p.key.to_ascii_uppercase().to_string(),
            t.type_.caption,
            pal.on_pad,
        ));
    let header = row()
        .gap(6.0)
        .center_y()
        .min_w(0.0)
        .child(led(
            skin,
            p.light,
            if p.missing.is_some() { 0.0 } else { level },
        ))
        .child(styled(
            format!("{:02}", p.index + 1),
            t.type_.caption,
            small,
        ))
        .child(styled(p.label, t.type_.label, ink).truncate());
    let wave_ink = pal
        .on_pad_dim
        .mix(p.light, 0.25 + 0.75 * level)
        .alpha(if p.missing.is_some() { 0.25 } else { 0.85 });
    let footer = row()
        .justify_between()
        .gap(6.0)
        .min_w(0.0)
        .child(if let Some(reason) = p.missing {
            let short = reason.rsplit_once(": ").map_or(reason, |(_, why)| why);
            row()
                .gap(6.0)
                .grow(1.0)
                .min_w(0.0)
                .child(
                    el().px(5.0)
                        .radius(3.0)
                        .fill(pal.danger.alpha(0.16))
                        .border(1.0, pal.danger.alpha(0.7))
                        .child(styled("MISSING", t.type_.caption, pal.danger)),
                )
                .child(styled(short, t.type_.caption, pal.on_pad_dim).truncate())
        } else {
            styled(format!("{:+.0} ST", p.pitch), t.type_.caption, small)
        })
        .child(
            row()
                .gap(6.0)
                .center_y()
                .child(el().w(28.0).children((p.voices > 0).then(|| {
                    styled(format!("▶ {}", p.voices), t.type_.caption, pal.on_pad)
                        .w(28.0)
                        .align(Align::End)
                        .key(format!("pad-{}-voices", p.index))
                        .transition(Transition::opacity())
                })))
                .child(keycap),
        );
    root = root
        .child(header)
        .child(waveform(&Wave {
            bins: p.bins,
            ink: wave_ink,
            grid: None,
            stroke: 1.4,
        }))
        .child(footer);
    if let Some(reason) = p.missing {
        root = root.help(reason).disabled(true);
    }
    root
}

/// The light under one pad, in a grid laid out like the pads and drawn before all of them, so a
/// glow spills into the gaps without painting over a neighbour's face.
pub fn pad_glow(skin: &Skin, light: Hex, level: f32) -> El {
    let t = skin.theme;
    let cell = el().grow(1.0).min_w(0.0).min_h(0.0);
    if level > 0.0 && skin.uses(fx::GLOW) {
        cell.child(fx::glow(
            t.motion.glow,
            light,
            t.motion.glow_intensity,
            t.motion.glow_radius,
            level,
            t.space.pad_radius,
        ))
    } else {
        cell
    }
}

pub struct KnobView<'a> {
    pub key: &'a str,
    pub label: &'a str,
    /// Position 0..1 along the travel.
    pub value: f32,
    pub display: &'a str,
    pub accent: Hex,
    /// The value arc starts at the top (pan) instead of the left end.
    pub bipolar: bool,
}

pub const KNOB: f32 = 54.0;
const CAP: f32 = 34.0;
const SWEEP: f32 = 2.356_194_5;

fn polar(c: f32, radius: f32, angle: f32) -> [f32; 2] {
    [c + radius * angle.sin(), c - radius * angle.cos()]
}

fn arc(c: f32, radius: f32, from: f32, to: f32) -> Vec<[f32; 2]> {
    let steps = ((to - from).abs() / 0.08).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|i| polar(c, radius, from + (to - from) * i as f32 / steps as f32))
        .collect()
}

/// A rotary knob with its caption and value. `control` gets the focusable element keyed `key`
/// to add its range, steps and drag.
pub fn knob(skin: &Skin, k: &KnobView, control: impl FnOnce(El) -> El) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let c = KNOB / 2.0;
    let angle = -SWEEP + 2.0 * SWEEP * k.value.clamp(0.0, 1.0);
    let mut ring = Shape::new().view_box(KNOB, KNOB);
    for i in 0..=10 {
        let a = -SWEEP + 2.0 * SWEEP * i as f32 / 10.0;
        ring = ring.stroke(
            vec![polar(c, 24.0, a), polar(c, 26.5, a)],
            1.0,
            p.ink_dim.alpha(0.7),
        );
    }
    ring = ring.stroke(arc(c, 21.0, -SWEEP, SWEEP), 2.5, p.inset.alpha(0.9));
    let start = if k.bipolar { 0.0 } else { -SWEEP };
    if (angle - start).abs() > 0.01 {
        ring = ring.stroke(arc(c, 21.0, start, angle), 2.5, k.accent.color());
    }
    let indicator = Shape::new().view_box(KNOB, KNOB).stroke(
        vec![polar(c, 6.0, angle), polar(c, 14.5, angle)],
        2.2,
        p.ink.color(),
    );
    let cap_at = (KNOB - CAP) / 2.0;
    let mut cap = el()
        .absolute(cap_at, cap_at)
        .size(CAP, CAP)
        .radius(CAP / 2.0)
        .shadow(2.0, 5.0, p.shadow.alpha(0.7));
    let material = t.materials.knob;
    if skin.uses(fx::PLASTIC) {
        cap = cap.child(fx::plastic(
            &material,
            material.base,
            material.dome,
            CAP / 2.0,
        ));
    } else {
        cap = cap
            .radial_gradient(
                [0.35, 0.3],
                CAP * 0.75,
                [
                    (0.0, material.base.mix(p.edge[0], 0.35).color()),
                    (1.0, material.base.mix(p.shadow, 0.25).color()),
                ],
            )
            .border(1.0, p.edge[1].alpha(0.6));
    }
    let mut body = el()
        .key(k.key)
        .role("slider")
        .label(k.label)
        .focusable(true)
        .size(KNOB, KNOB)
        .radius(KNOB / 2.0)
        .focus_ring(2.0, 2.0, p.focus.color())
        .child(
            canvas(ring)
                .absolute(0.0, 0.0)
                .size(KNOB, KNOB)
                .a11y_hidden(),
        );
    if t.materials.trim != Metal::None && skin.uses(fx::METAL) {
        let d = CAP + 5.0;
        body = body.child(
            el().absolute((KNOB - d) / 2.0, (KNOB - d) / 2.0)
                .size(d, d)
                .radius(d / 2.0)
                .child(fx::metal(t.materials.trim, 2.5, d / 2.0)),
        );
    }
    body = body.child(cap).child(
        canvas(indicator)
            .absolute(0.0, 0.0)
            .size(KNOB, KNOB)
            .a11y_hidden(),
    );
    col()
        .gap(4.0)
        .items_center()
        .child(caption(skin, k.label.to_ascii_uppercase()))
        .child(control(body))
        .child(
            styled(k.display, t.type_.value, p.ink)
                .w(KNOB + 12.0)
                .align(Align::Center)
                .key(format!("{}-value", k.key)),
        )
}

pub struct FaderView<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub value: f32,
    pub display: &'a str,
    pub accent: Hex,
}

pub const FADER_CAP: [f32; 2] = [34.0, 18.0];
pub const FADER_W: f32 = 44.0;

/// A vertical fader that fills its parent's height, with its caption and value. `control` gets
/// the focusable element keyed `key` to add its range, steps and drag.
pub fn fader(skin: &Skin, f: &FaderView, control: impl FnOnce(El) -> El) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let v = f.value.clamp(0.0, 1.0);
    let [cw, ch] = FADER_CAP;
    let track_x = (FADER_W - 6.0) / 2.0;
    let half = ch / 2.0;
    let mut ticks = col()
        .pin(Some(2.0), Some(half), Some(2.0), Some(half))
        .justify_between();
    for i in 0..=10 {
        ticks = ticks.child(
            row()
                .justify_between()
                .child(
                    el().size(if i % 5 == 0 { 9.0 } else { 6.0 }, 1.0)
                        .fill(p.ink_dim.alpha(0.6)),
                )
                .child(
                    el().size(if i % 5 == 0 { 9.0 } else { 6.0 }, 1.0)
                        .fill(p.ink_dim.alpha(0.6)),
                ),
        );
    }
    let track = el()
        .pin(Some(track_x), Some(half), None, Some(half))
        .w(6.0)
        .radius(3.0)
        .fill(p.inset.color())
        .box_shadow(Shadow {
            x: 0.0,
            y: 1.0,
            blur: 3.0,
            spread: 0.0,
            color: Color::BLACK.alpha(0.6),
            inset: true,
        });
    let fill = col()
        .pin(Some(track_x + 1.0), Some(half), None, Some(half))
        .w(4.0)
        .child(el().grow(1.0 - v).min_h(0.0))
        .child(el().grow(v).min_h(0.0).radius(2.0).fill(f.accent.color()));
    let mut cap = el()
        .size(cw, ch)
        .radius(4.0)
        .shadow(3.0, 6.0, p.shadow.alpha(0.7));
    let material = t.materials.fader_cap;
    if skin.uses(fx::PLASTIC) {
        cap = cap.child(fx::plastic(&material, material.base, material.dome, 4.0));
    } else {
        cap = cap
            .gradient(
                material.base.mix(p.edge[0], 0.3).color(),
                material.base.mix(p.shadow, 0.2).color(),
            )
            .border_gradient(1.0, p.edge[0].alpha(0.6), p.edge[1].alpha(0.8));
    }
    cap = cap.child(
        el().absolute(5.0, ch / 2.0 - 1.0)
            .size(cw - 10.0, 2.0)
            .radius(1.0)
            .fill(f.accent.color())
            .a11y_hidden(),
    );
    let carriage = col()
        .pin(Some((FADER_W - cw) / 2.0), Some(0.0), None, Some(0.0))
        .w(cw)
        .child(el().grow(1.0 - v).min_h(0.0))
        .child(cap)
        .child(el().grow(v).min_h(0.0));
    let body = control(
        el().key(f.key)
            .role("slider")
            .label(f.label)
            .focusable(true)
            .w(FADER_W)
            .grow(1.0)
            .min_h(60.0)
            .radius(6.0)
            .focus_ring(2.0, 2.0, p.focus.color())
            .child(ticks)
            .child(track)
            .child(fill)
            .child(carriage),
    );
    col()
        .gap(5.0)
        .items_center()
        .min_h(0.0)
        .child(caption(skin, f.label.to_ascii_uppercase()))
        .child(body)
        .child(
            styled(f.display, t.type_.value, p.ink)
                .w(FADER_W + 20.0)
                .align(Align::Center)
                .key(format!("{}-value", f.key)),
        )
}

pub struct MeterView {
    /// Linear peak shown.
    pub level: f32,
    pub hold: f32,
    /// How lit the held peak is, 0..1.
    pub hold_light: f32,
    pub clip: bool,
}

const SEGMENTS: usize = 24;

fn segment_of(linear: f32) -> f32 {
    ((db(linear) + 48.0) / 48.0).clamp(0.0, 1.0) * SEGMENTS as f32
}

/// A vertical peak meter, −48..0 dBFS, with a held peak and a latched clip light.
pub fn meter(skin: &Skin, m: &MeterView) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let lit = segment_of(m.level);
    let hold = (m.hold > 0.0).then(|| (segment_of(m.hold).ceil() as usize).max(1) - 1);
    let mut bars = col().grow(1.0).min_h(0.0).w(14.0).gap(2.0);
    for i in (0..SEGMENTS).rev() {
        let color = if i >= SEGMENTS - 2 {
            p.danger
        } else if i >= SEGMENTS - 6 {
            p.warn
        } else {
            p.ok
        };
        let on = (lit - i as f32).clamp(0.0, 1.0);
        let held = if hold == Some(i) {
            0.75 * m.hold_light
        } else {
            0.0
        };
        let k = on.max(held);
        let mut seg = el()
            .grow(1.0)
            .min_h(1.0)
            .w_full()
            .radius(1.5)
            .motion(Spring::NONE)
            .fill(p.inset.mix(color, 0.16 + 0.84 * k).color());
        if k > 0.0 && skin.fx == FxLevel::Full {
            seg = seg.shadow(0.0, 5.0 * k, color.alpha(0.45 * k));
        }
        bars = bars.child(seg.a11y_hidden());
    }
    let value = db(m.level);
    col()
        .key("output-meter")
        .role("meter")
        .label("Output peak")
        .range_value(f64::from(value.max(-60.0)), -60.0, 0.0)
        .gap(5.0)
        .items_center()
        .min_h(0.0)
        .child(caption(skin, "OUT"))
        .child(
            el().key("clip")
                .size(14.0, 6.0)
                .radius(2.0)
                .motion(Spring::NONE)
                .fill(if m.clip {
                    p.danger.color()
                } else {
                    p.inset.mix(p.danger, 0.2).color()
                })
                .a11y_hidden(),
        )
        .child(bars)
        .child(
            styled(
                if m.level == 0.0 {
                    "−∞".to_owned()
                } else {
                    format!("{value:.1}")
                },
                t.type_.value,
                p.ink,
            )
            .w(44.0)
            .align(Align::Center)
            .key("meter")
            .a11y_value(format!("{:.3}", m.level)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::look::theme::BUNKER;

    #[test]
    fn bins_preserve_transients_and_silence_without_inventing_signal() {
        assert_eq!(bins(&[]), [[0.0; 2]; BINS]);
        assert_eq!(bins(&[0.0; 1000]), [[0.0; 2]; BINS]);
        let mut data = vec![0.0; 960];
        data[9] = -0.5;
        data[955] = 1.0;
        let b = bins(&data);
        assert_eq!(b[0], [-0.5, 0.0]);
        assert_eq!(b[95], [0.0, 1.0]);
    }

    #[test]
    fn effects_light_keeps_still_materials_and_reduced_motion_drops_ripples() {
        let skin = |fx, reduced_motion| Skin {
            theme: &BUNKER,
            fx,
            reduced_motion,
            mask: fx::ALL_KINDS,
        };
        let full = skin(FxLevel::Full, false);
        assert!(full.uses(fx::RIPPLE) && full.uses(fx::GLOW) && full.uses(fx::PLASTIC));
        let light = skin(FxLevel::Light, false);
        assert!(light.uses(fx::PLASTIC) && light.uses(fx::METAL));
        assert!(!light.uses(fx::GLOW) && !light.uses(fx::RIPPLE) && !light.uses(fx::GRAIN));
        assert!(!skin(FxLevel::Off, false).uses(fx::PLASTIC));
        let calm = skin(FxLevel::Full, true);
        assert!(!calm.uses(fx::RIPPLE) && calm.uses(fx::GLOW));
        let only_glow = Skin {
            mask: 1 << fx::GLOW,
            ..full
        };
        assert!(only_glow.uses(fx::GLOW) && !only_glow.uses(fx::PLASTIC));
    }

    #[test]
    fn meter_segments_cover_minus_48_to_zero() {
        assert_eq!(segment_of(0.0), 0.0);
        assert_eq!(segment_of(1.0), SEGMENTS as f32);
        assert!((segment_of(10f32.powf(-24.0 / 20.0)) - 12.0).abs() < 1e-3);
    }
}
