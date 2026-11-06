//! The effect compositor. Controls place `El::effect` slots; each slot's mark says which effect,
//! its data carries the few live numbers, and its presented rect and radius are the shape. The
//! compositor turns slots into rune-fx layers and draws them in painter order, so Rune keeps
//! layout, clipping, opacity and hit testing. Nothing here runs on the audio thread.

use crate::look::theme::{GlowKind, Hex, Material, Metal};
use rune::compose::{Compositor, DeviceKey, Frame};
use rune::render::{EffectPhase, EffectSite, KIND_EFFECT};
use rune::{El, effect};
use rune_fx::{
    Fx, FxFrame, FxLayer, FxOptions, FxRenderer, FxSlot, GlowParams, GlowStyle, GrainParams,
    LensParams, LiquidMetalParams, PlasticParams, PulseParams, PulseStyle, Shape, rgb,
};
use std::time::Instant;

pub mod probe;
pub use probe::Probe;

pub const PLASTIC: u32 = 1;
pub const GLOW: u32 = 2;
pub const RIPPLE: u32 = 3;
pub const METAL: u32 = 4;
pub const GRAIN: u32 = 5;
pub const GLASS: u32 = 6;
pub const KINDS: [(u32, &str); 6] = [
    (PLASTIC, "plastic"),
    (GLOW, "glow"),
    (RIPPLE, "ripple"),
    (METAL, "metal"),
    (GRAIN, "grain"),
    (GLASS, "glass"),
];

/// Every kind on: a mask bit per kind (`1 << kind`).
pub const ALL_KINDS: u32 = !0;

/// One light for the whole panel, from the upper left, so every surface agrees.
pub const LIGHT_ANGLE: f32 = -0.55;

fn slot(mark: u32, data: [f32; 4], radius: f32) -> El {
    effect(mark)
        .effect_data(data)
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .radius(radius)
        .a11y_hidden()
}

/// Moulded plastic or rubber filling the parent.
pub fn plastic(m: &Material, color: Hex, dome: f32, radius: f32) -> El {
    slot(PLASTIC, [color.slot(), m.roughness, dome, m.bevel], radius)
}

/// Light around the parent: `heat` 0..1 is the envelope, `intensity` the theme's strength.
pub fn glow(kind: GlowKind, color: Hex, intensity: f32, spread: f32, heat: f32, radius: f32) -> El {
    let style = match kind {
        GlowKind::Soft => 0,
        GlowKind::Neon => 1,
    };
    slot(
        GLOW | style << 8,
        [color.slot(), intensity, spread, heat],
        radius,
    )
    .effect_bleed(spread * 4.5)
}

/// One ripple confined to the parent's shape, from `at` (0..1 across and down). `strength` is
/// the hit's velocity; `opacity` fades a retired ripple out.
pub fn ripple(
    color: Hex,
    age: f32,
    at: [f32; 2],
    strength: f32,
    opacity: f32,
    life: f32,
    radius: f32,
) -> El {
    let q = |v: f32| ((v.clamp(0.0, 1.0) * 4095.0).round() as u32) & 0xfff;
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round();
    slot(
        RIPPLE | q(at[0]) << 8 | q(at[1]) << 20,
        [
            color.slot(),
            age,
            byte(strength) * 256.0 + byte(opacity),
            life,
        ],
        radius,
    )
}

/// (strength, opacity) packed by `ripple`: two bytes, exact in an `f32`.
fn unpack_ripple(v: f32) -> (f32, f32) {
    let v = v.clamp(0.0, 65_535.0) as u32;
    ((v >> 8) as f32 / 255.0, (v & 0xff) as f32 / 255.0)
}

/// Still liquid metal: a ring `border` points wide, or a whole face when `border` is 0.
pub fn metal(metal: Metal, border: f32, radius: f32) -> El {
    let variant = match metal {
        Metal::None | Metal::Chrome => 0.0,
        Metal::Gunmetal => 1.0,
        Metal::Brass => 2.0,
    };
    slot(METAL, [variant, border, 0.0, 0.0], radius)
}

/// Static film grain over the parent.
pub fn grain(amount: f32, radius: f32) -> El {
    slot(GRAIN, [amount, 0.0, 0.0, 0.0], radius)
}

/// Frosted, tinted glass over the parent: reads a backdrop copy.
pub fn glass(frost: f32, tint: Hex, tint_amount: f32, radius: f32) -> El {
    slot(GLASS, [frost, tint.slot(), tint_amount, 0.0], radius).effect_backdrop(true)
}

/// The rune-fx layer for one slot, or `None` for an unknown mark.
pub fn layer(mark: u32, d: [f32; 4], shape: Shape, reduced: bool) -> Option<FxLayer> {
    let color = |v: f32| rgb(Hex::from_slot(v).0);
    let lighter = |v: f32| rgb(Hex::from_slot(v).mix(Hex(0xffffff), 0.65).0);
    let fx = match mark & 0xff {
        PLASTIC => Fx::fill(PlasticParams {
            color: color(d[0]),
            roughness: d[1].clamp(0.05, 1.0),
            dome: d[2].clamp(-1.0, 1.0),
            bevel: d[3].max(0.0),
            angle: LIGHT_ANGLE,
        }),
        GLOW => Fx::glow(GlowParams {
            style: if mark >> 8 & 0xff == 1 {
                GlowStyle::Neon
            } else {
                GlowStyle::Soft
            },
            color: color(d[0]),
            core: lighter(d[0]),
            intensity: d[1],
            radius: d[2].max(1.0),
            heat: d[3].clamp(0.0, 1.0),
            hot: 0.35,
            occlusion: 0.5,
            ..GlowParams::default()
        }),
        RIPPLE => {
            let r = shape.rect;
            let at = [
                r.x + (mark >> 8 & 0xfff) as f32 / 4095.0 * r.w,
                r.y + (mark >> 20 & 0xfff) as f32 / 4095.0 * r.h,
            ];
            let (strength, opacity) = unpack_ripple(d[2]);
            return Some(
                Fx::pulse(PulseParams {
                    style: PulseStyle::Ripple { at },
                    color: color(d[0]),
                    core: lighter(d[0]),
                    intensity: 0.35 + 0.65 * strength,
                    width: 1.2 + 2.2 * strength,
                    reach: 0.0,
                    t0: 0.0,
                    duration: d[3].max(0.05),
                    ..PulseParams::default()
                })
                .on(shape)
                .over()
                .at(d[1])
                .opacity(opacity)
                .reduced(reduced),
            );
        }
        METAL => {
            let p = match d[0] as u32 {
                1 => LiquidMetalParams::gunmetal(),
                2 => LiquidMetalParams::gold(),
                _ => LiquidMetalParams::chrome(),
            };
            let p = LiquidMetalParams {
                flow: 0.0,
                speed: 0.0,
                angle: LIGHT_ANGLE,
                ..p
            }
            .anchor(rune_fx::FillAnchor::Element);
            if d[1] > 0.0 {
                Fx::fill(p.border(d[1]))
            } else {
                Fx::fill(p)
            }
        }
        GRAIN => Fx::grain(GrainParams {
            amount: d[0],
            fps: 0.0,
            feather: 0.0,
        }),
        GLASS => Fx::lens(LensParams {
            frost: d[0].clamp(0.0, 1.0),
            tint: color(d[1]),
            tint_amount: d[2],
            magnification: 1.0,
            dispersion: 0.04,
            depth: 6.0,
            bevel: 10.0,
            shadow: 0.0,
            ..LensParams::default()
        }),
        _ => return None,
    };
    Some(fx.on(shape).reduced(reduced))
}

/// The instrument tells the compositor whether its motion is still settling, so the window keeps
/// its display link only while something moves.
pub trait Settling {
    fn settling(&self, now: f64) -> bool;
}

/// Draws the frame: Rune's UI with rune-fx layers at their slots.
#[derive(Default)]
pub struct Look {
    fx: Option<(DeviceKey, FxRenderer)>,
    live: bool,
    probe: Option<Probe>,
    at: Vec<u32>,
    layers: Vec<FxLayer>,
    slots: Vec<FxSlot>,
}

impl Look {
    /// A compositor that also records CPU and GPU frame timings (see `Probe`).
    pub fn probed(label: &str) -> Look {
        Look {
            probe: Some(Probe::new(label)),
            ..Look::default()
        }
    }
}

impl<S: Settling + 'static> Compositor<S> for Look {
    fn features(&self) -> wgpu::Features {
        if self.probe.is_some() {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        }
    }

    fn animating(&self) -> bool {
        self.live
    }

    fn summary(&mut self, _device: &wgpu::Device) -> Option<String> {
        self.probe.as_ref().map(Probe::summary)
    }

    fn render(&mut self, f: &mut Frame<'_, S>) {
        let started = Instant::now();
        let (device, queue, size) = (f.device, f.queue, f.size);
        let reduced = f.rt.reduced_motion;
        self.at.clear();
        self.layers.clear();
        self.slots.clear();
        for (n, i) in f.rt.instances.iter().enumerate() {
            if i.params[1] != KIND_EFFECT {
                continue;
            }
            let mark = f.rt.marks.get(n).copied().unwrap_or(0);
            let k = i.params[3];
            let [x, y, w, h] = i.rect.map(|v| v / k);
            let r = i.radii.iter().fold(0.0f32, |a, v| a.max(v.abs())) / k;
            let Some(layer) = layer(mark, i.uv, Shape::rounded(x, y, w, h, r), reduced) else {
                continue;
            };
            self.at.push(n as u32);
            self.layers.push(layer);
            self.slots
                .push(FxSlot::default().scale(k).clip(i.clip, i.fade));
        }
        let key = f.device_key();
        if self.fx.as_ref().is_none_or(|(k, _)| *k != key) {
            self.fx = Some((
                key,
                FxRenderer::new(device, f.format, FxOptions { dither: true }),
            ));
        }
        let Some((_, fx)) = self.fx.as_mut() else {
            return;
        };
        fx.set_srgb_blend(f.srgb_blend());
        fx.add_format(device, wgpu::TextureFormat::Rgba16Float);
        let built = fx.built_pipelines();
        let stats = fx.prepare_with_slots(
            device,
            queue,
            &FxFrame {
                viewport: [size.0, size.1],
                scale: 1.0,
                layers: &self.layers,
                backdrop: None,
                sources: &[],
            },
            &self.slots,
        );
        let clear = f.rt.clear_color();
        if let Some(probe) = self.probe.as_mut() {
            probe.begin(device, queue, f.encoder, f.target, clear);
        }
        f.ui.prepare(device, queue, &mut f.rt.text.atlas, &f.rt.instances, size);
        let at = &self.at;
        f.ui.draw_with_backdrop(
            f.encoder,
            f.target,
            clear,
            &mut |phase, s: &EffectSite<'_>| {
                let Some(k) = at.iter().position(|n| *n == s.index) else {
                    return;
                };
                match phase {
                    EffectPhase::Backdrop(enc) => {
                        if let Some(backdrop) = s.backdrop {
                            fx.blur_backdrop(device, queue, enc, backdrop, k);
                        }
                    }
                    EffectPhase::Draw(pass) => fx.draw_layer_in(pass, s.format, s.origin, k),
                }
            },
        );
        self.live = f.rt.state.settling(f.rt.clock);
        if let Some(probe) = self.probe.as_mut() {
            probe.end(f.encoder, f.target);
            let compiled = fx.built_pipelines().saturating_sub(built);
            probe.frame(
                f.update_ms,
                started.elapsed().as_secs_f64() * 1000.0,
                stats.drawn,
                stats.pixels,
                compiled,
                self.live,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Shape {
        Shape::rounded(10.0, 20.0, 100.0, 50.0, 8.0)
    }

    #[test]
    fn slots_map_to_layers_and_unknown_marks_draw_nothing() {
        assert!(layer(0, [0.0; 4], shape(), false).is_none());
        assert!(layer(99, [0.0; 4], shape(), false).is_none());
        for (kind, _) in KINDS {
            assert!(layer(kind, [Hex(0x808080).slot(), 0.5, 0.5, 0.5], shape(), false).is_some());
        }
    }

    #[test]
    fn ripple_origin_is_unpacked_inside_the_shape_and_ends_with_its_life() {
        let mark = |x: f32, y: f32| {
            let q = |v: f32| (v * 4095.0).round() as u32;
            RIPPLE | q(x) << 8 | q(y) << 20
        };
        assert_eq!(unpack_ripple(255.0 * 256.0 + 128.0), (1.0, 128.0 / 255.0));
        let Some(live) = layer(mark(0.25, 1.0), [0.0, 0.1, 65_535.0, 0.5], shape(), false) else {
            panic!("ripple");
        };
        let Fx::Pulse(PulseParams {
            style: PulseStyle::Ripple { at },
            ..
        }) = live.fx
        else {
            panic!("pulse");
        };
        assert!(
            (at[0] - 35.0).abs() < 0.05 && (at[1] - 70.0).abs() < 0.05,
            "{at:?}"
        );
        assert!(live.is_animating());
        assert_eq!(live.opacity, 1.0);
        let done = layer(mark(0.5, 0.5), [0.0, 0.6, 65_535.0, 0.5], shape(), false);
        assert!(done.is_some_and(|l| !l.is_animating()));
    }

    #[test]
    fn glow_style_rides_in_the_mark_and_heat_is_clamped() {
        let Some(l) = layer(GLOW | 1 << 8, [0.0, 1.0, 10.0, 7.0], shape(), true) else {
            panic!("glow");
        };
        let Fx::Glow(p) = l.fx else { panic!("glow") };
        assert_eq!(p.style, GlowStyle::Neon);
        assert_eq!(p.heat, 1.0);
        assert!(l.reduced_motion);
    }
}
