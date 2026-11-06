//! The effect lab: each effect alone on one control, in the current theme, beside Rune's
//! primitives-only baseline. Pads here are the instrument's first six pads, so hits play and
//! light exactly as in the play view.

use crate::state::State;
use crate::view::{pad_view, playable, ripples};
use brownnote_native::look::{
    controls::{
        KnobView, Skin, Wave, bloom, caption, display, knob, pad, pad_glow, panel, styled, waveform,
    },
    fx,
    theme::{FxLevel, GlowKind, Metal, Theme},
};
use rune::{El, col, row};

struct Cell {
    title: &'static str,
    note: &'static str,
}

const PADS: [(Cell, u32); 6] = [
    (
        Cell {
            title: "BASELINE",
            note: "Rune primitives: gradient, border, shadow",
        },
        0,
    ),
    (
        Cell {
            title: "PLASTIC",
            note: "Moulded fill: roughness, dome, bevel",
        },
        1 << fx::PLASTIC,
    ),
    (
        Cell {
            title: "SOFT GLOW",
            note: "Light spills under the pad",
        },
        1 << fx::GLOW,
    ),
    (
        Cell {
            title: "NEON EDGE",
            note: "Emissive rim hugging the pad",
        },
        1 << fx::GLOW,
    ),
    (
        Cell {
            title: "RIPPLE",
            note: "One-shot from the hit point, 8 live per window",
        },
        1 << fx::RIPPLE,
    ),
    (
        Cell {
            title: "SHADOW GLOW",
            note: "Coloured box-shadow, no shader",
        },
        0,
    ),
];

fn cell(skin: &Skin, c: &Cell, key: String, specimen: El) -> El {
    let t = skin.theme;
    panel(skin)
        .key(key)
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .padding(10.0)
        .gap(6.0)
        .child(styled(c.title, t.type_.label, t.palette.ink))
        .child(caption(skin, c.note).truncate())
        .child(specimen)
}

fn line(cells: impl IntoIterator<Item = El>, gap: f32) -> El {
    row().grow(1.0).min_h(0.0).gap(gap).children(cells)
}

pub fn view(s: &State, base: &Skin) -> El {
    let t = base.theme;
    let soft = Theme {
        motion: brownnote_native::look::theme::Motion {
            glow: GlowKind::Soft,
            ..t.motion
        },
        ..*t
    };
    let neon = Theme {
        motion: brownnote_native::look::theme::Motion {
            glow: GlowKind::Neon,
            ..t.motion
        },
        ..*t
    };
    let trimmed = Theme {
        materials: brownnote_native::look::theme::Materials {
            trim: if t.materials.trim == Metal::None {
                Metal::Chrome
            } else {
                t.materials.trim
            },
            ..t.materials
        },
        surfaces: brownnote_native::look::theme::Surfaces {
            display_grain: t.surfaces.display_grain.max(0.05),
            display_bloom: t.surfaces.display_bloom.max(0.45),
            glass: false,
            ..t.surfaces
        },
        ..*t
    };
    let sheet = Theme {
        surfaces: brownnote_native::look::theme::Surfaces {
            glass: true,
            ..trimmed.surfaces
        },
        ..trimmed
    };
    let calm = base.reduced_motion;
    fn skin(theme: &Theme, fx: FxLevel, mask: u32, reduced_motion: bool) -> Skin<'_> {
        Skin {
            theme,
            fx,
            reduced_motion,
            mask,
        }
    }
    let gap = t.space.gap;
    let pads = PADS.iter().enumerate().map(|(i, (c, mask))| {
        let theme = match i {
            3 => &neon,
            2 => &soft,
            _ => t,
        };
        let level = if i == 0 { FxLevel::Off } else { FxLevel::Full };
        let skin = skin(theme, level, *mask, calm);
        let shots = ripples(s, &skin, i);
        let view = pad_view(s, &skin, i, &shots);
        let glow = pad_glow(&skin, view.light, view.level)
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .a11y_hidden();
        let specimen = col().grow(1.0).min_h(0.0).child(glow).child(playable(
            pad(&skin, &view),
            i,
            view.missing.is_some(),
        ));
        cell(&skin, c, format!("lab-{i}"), specimen)
    });
    let pads: Vec<El> = pads.collect();
    let mut pads = pads.into_iter();
    let first: Vec<El> = pads.by_ref().take(4).collect();
    let knob_cell = |key: &'static str, c: Cell, mask: u32| {
        let skin = skin(&trimmed, FxLevel::Full, mask, calm);
        let value = s.settings[s.selected].gain / 1.5;
        let display = format!("{:.0}%", value * 150.0);
        let specimen = row()
            .grow(1.0)
            .min_h(0.0)
            .justify_center()
            .center_y()
            .child(knob(
                &skin,
                &KnobView {
                    key,
                    label: "Gain",
                    value,
                    display: &display,
                    accent: t.palette.accent,
                    bipolar: false,
                },
                |body| body.focusable(false).a11y_hidden(),
            ));
        cell(&skin, &c, format!("{key}-cell"), specimen)
    };
    let second: Vec<El> = pads
        .chain([
            knob_cell(
                "lab-metal",
                Cell {
                    title: "METAL TRIM",
                    note: "Still liquid metal ring, one light",
                },
                1 << fx::METAL,
            ),
            knob_cell(
                "lab-knob",
                Cell {
                    title: "PLASTIC KNOB",
                    note: "Domed cap, same light angle",
                },
                1 << fx::PLASTIC,
            ),
        ])
        .collect();
    let wave = |ink| Wave {
        bins: &s.waves[0],
        ink,
        grid: Some(t.palette.display_dim.alpha(0.18)),
        stroke: 1.6,
    };
    let ink = t.palette.display_ink.color();
    let screen = |key: &'static str, c: Cell, theme: &Theme, mask: u32, extra: Option<El>| {
        let skin = skin(theme, FxLevel::Full, mask, calm);
        let content = if key == "lab-bloom" {
            bloom(
                &skin,
                waveform(&wave(ink)),
                waveform(&Wave {
                    stroke: 3.0,
                    ..wave(ink)
                }),
            )
        } else {
            waveform(&wave(ink))
        };
        let mut d = display(&skin, key, col().grow(1.0).min_w(0.0).child(content))
            .grow(1.0)
            .min_h(0.0);
        if let Some(extra) = extra {
            d = d.child(extra);
        }
        cell(&skin, &c, format!("{key}-cell"), d)
    };
    let r = t.space.panel_radius - 2.0;
    let third = [
        screen(
            "lab-grain",
            Cell {
                title: "GRAIN",
                note: "Static film grain on the display",
            },
            &trimmed,
            1 << fx::GRAIN,
            None,
        ),
        screen(
            "lab-bloom",
            Cell {
                title: "BLOOM",
                note: "Blurred under-copy of the ink",
            },
            &trimmed,
            0,
            None,
        ),
        screen(
            "lab-lens",
            Cell {
                title: "LENS GLASS",
                note: "Frosted lens over the trace (backdrop)",
            },
            &trimmed,
            1 << fx::GLASS,
            Some(
                fx::glass(0.35, t.palette.display[0], 0.25, r)
                    .w_full()
                    .h_full(),
            ),
        ),
        screen(
            "lab-sheet",
            Cell {
                title: "GLASS SHEET",
                note: "Gradient specular sheet, no shader",
            },
            &sheet,
            0,
            None,
        ),
    ];
    col()
        .key("lab")
        .grow(1.0)
        .min_h(0.0)
        .gap(gap)
        .child(line(first, gap))
        .child(line(second, gap))
        .child(line(third, gap))
}
