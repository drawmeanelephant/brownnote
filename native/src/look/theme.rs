//! Theme tokens for one art direction: colour, type, materials, spacing and motion. Controls read
//! these and nothing else, so a new direction is a new `Theme` value, not new control code.

use rune::{Color, TextStyle};

/// An sRGB `0xRRGGBB` colour token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hex(pub u32);

impl Hex {
    pub fn color(self) -> Color {
        let channel =
            |shift: u32| rune::color::srgb_decode(((self.0 >> shift) & 0xff) as f32 / 255.0);
        Color {
            r: channel(16),
            g: channel(8),
            b: channel(0),
            a: 1.0,
        }
    }

    pub fn alpha(self, a: f32) -> Color {
        self.color().alpha(a)
    }

    /// `t` of the way to `other`, in linear light.
    pub fn mix(self, other: Hex, t: f32) -> Hex {
        let a = self.color().mix(other.color(), t.clamp(0.0, 1.0));
        let enc = |c: f32| (rune::color::srgb_encode(c) * 255.0).round() as u32;
        Hex(enc(a.r) << 16 | enc(a.g) << 8 | enc(a.b))
    }

    /// The token as an effect-slot float: every 24-bit value is exact in an `f32`.
    pub fn slot(self) -> f32 {
        (self.0 & 0xff_ffff) as f32
    }

    pub fn from_slot(value: f32) -> Hex {
        Hex(value.clamp(0.0, 16_777_215.0) as u32)
    }

    /// WCAG relative luminance, for contrast checks.
    pub fn luminance(self) -> f32 {
        self.color().luminance()
    }
}

/// WCAG contrast ratio between two opaque colours.
pub fn contrast(a: Hex, b: Hex) -> f32 {
    let (hi, lo) = {
        let (x, y) = (a.luminance(), b.luminance());
        if x > y { (x, y) } else { (y, x) }
    };
    (hi + 0.05) / (lo + 0.05)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeId {
    Bunker,
    SpaceProgram,
    Afterhours,
}

impl ThemeId {
    pub const ALL: [ThemeId; 3] = [ThemeId::Bunker, ThemeId::SpaceProgram, ThemeId::Afterhours];

    pub fn theme(self) -> &'static Theme {
        match self {
            ThemeId::Bunker => &BUNKER,
            ThemeId::SpaceProgram => &SPACE_PROGRAM,
            ThemeId::Afterhours => &AFTERHOURS,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            ThemeId::Bunker => "bunker",
            ThemeId::SpaceProgram => "space",
            ThemeId::Afterhours => "afterhours",
        }
    }

    pub fn parse(name: &str) -> Option<ThemeId> {
        ThemeId::ALL.into_iter().find(|id| {
            id.slug().eq_ignore_ascii_case(name) || id.theme().name.eq_ignore_ascii_case(name)
        })
    }
}

/// How much of the effect stack draws. `Off` is plain Rune primitives only, the baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxLevel {
    Full,
    Light,
    Off,
}

impl FxLevel {
    pub const ALL: [FxLevel; 3] = [FxLevel::Full, FxLevel::Light, FxLevel::Off];

    pub fn slug(self) -> &'static str {
        match self {
            FxLevel::Full => "full",
            FxLevel::Light => "light",
            FxLevel::Off => "off",
        }
    }

    pub fn parse(name: &str) -> Option<FxLevel> {
        FxLevel::ALL
            .into_iter()
            .find(|level| level.slug().eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Window background, top and bottom.
    pub backdrop: [Hex; 2],
    /// Raised panel faces, top and bottom.
    pub panel: [Hex; 2],
    /// Bevel light (top edge) and dark (bottom edge).
    pub edge: [Hex; 2],
    /// Recessed wells: fader tracks, display surrounds.
    pub inset: Hex,
    pub shadow: Hex,
    pub ink: Hex,
    pub ink_dim: Hex,
    pub on_pad: Hex,
    pub on_pad_dim: Hex,
    pub accent: Hex,
    pub focus: Hex,
    /// Display glass, top and bottom.
    pub display: [Hex; 2],
    pub display_ink: Hex,
    pub display_dim: Hex,
    pub ok: Hex,
    pub warn: Hex,
    pub danger: Hex,
    /// Light colour per sample: what a pad glows when it plays.
    pub samples: [Hex; 5],
}

#[derive(Clone, Copy, Debug)]
pub struct Typography {
    pub title: TextStyle,
    pub label: TextStyle,
    pub caption: TextStyle,
    pub value: TextStyle,
    pub readout: TextStyle,
}

/// Moulded surface for the plastic fill (`rune_fx::PlasticParams`). `dome` below zero dishes the
/// face like a rubber pad; `roughness` 0 is polished, 1 matte.
#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub base: Hex,
    pub roughness: f32,
    pub dome: f32,
    pub bevel: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metal {
    None,
    Chrome,
    Gunmetal,
    Brass,
}

#[derive(Clone, Copy, Debug)]
pub struct Materials {
    pub pad: Material,
    pub pad_missing: Material,
    pub knob: Material,
    pub fader_cap: Material,
    /// Ring around knob caps and display bezels.
    pub trim: Metal,
}

#[derive(Clone, Copy, Debug)]
pub struct Space {
    pub outer: f32,
    pub gap: f32,
    pub pad_gap: f32,
    pub pad_radius: f32,
    pub panel_radius: f32,
    pub control_radius: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlowKind {
    /// Light spilling out under the pad.
    Soft,
    /// An emissive edge hugging the pad.
    Neon,
}

#[derive(Clone, Copy, Debug)]
pub struct Motion {
    /// Seconds for a hit's light to reach its peak.
    pub attack: f32,
    /// Time constant of a hit's flash, seconds.
    pub release: f32,
    /// Light held while a pad's voice plays, as a fraction of its hit velocity.
    pub sustain: f32,
    /// Time constant of the sustain light once the voice ends.
    pub voice_release: f32,
    pub ripple_life: f32,
    /// Ripple strength over the pad's labels: below 1 on light faces, where light washes out.
    pub ripple_opacity: f32,
    pub glow: GlowKind,
    pub glow_radius: f32,
    pub glow_intensity: f32,
    /// Meter fall time constant, seconds.
    pub meter_release: f32,
    pub peak_hold: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Surfaces {
    /// Static grain over displays (0 = none).
    pub display_grain: f32,
    /// Blurred under-copy of display ink, like phosphor (0 = none).
    pub display_bloom: f32,
    /// Smoked-glass specular sheet over displays.
    pub glass: bool,
    /// How much of a fully lit pad's face its light covers (0 = only its surroundings).
    pub lit_face: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub id: ThemeId,
    pub name: &'static str,
    pub tagline: &'static str,
    pub palette: Palette,
    pub type_: Typography,
    pub materials: Materials,
    pub space: Space,
    pub motion: Motion,
    pub surfaces: Surfaces,
}

const MOTION: Motion = Motion {
    attack: 0.012,
    release: 0.20,
    sustain: 0.30,
    voice_release: 0.30,
    ripple_life: 0.55,
    ripple_opacity: 0.8,
    glow: GlowKind::Soft,
    glow_radius: 15.0,
    glow_intensity: 1.0,
    meter_release: 0.14,
    peak_hold: 0.8,
};

const SPACE: Space = Space {
    outer: 18.0,
    gap: 10.0,
    pad_gap: 9.0,
    pad_radius: 10.0,
    panel_radius: 12.0,
    control_radius: 7.0,
};

pub const BUNKER: Theme = Theme {
    id: ThemeId::Bunker,
    name: "Bunker",
    tagline: "DISHED RUBBER  /  ACID LCD",
    palette: Palette {
        backdrop: [Hex(0x1d201e), Hex(0x0e100f)],
        panel: [Hex(0x2b2f2c), Hex(0x1e211f)],
        edge: [Hex(0x4a514b), Hex(0x090a09)],
        inset: Hex(0x0d0f0e),
        shadow: Hex(0x040504),
        ink: Hex(0xe4e9df),
        ink_dim: Hex(0x949d95),
        on_pad: Hex(0xdfe5da),
        on_pad_dim: Hex(0x9aa39b),
        accent: Hex(0xb6ff3b),
        focus: Hex(0xf0ffc2),
        display: [Hex(0x102010), Hex(0x081208)],
        display_ink: Hex(0xb6ff3b),
        display_dim: Hex(0x58804a),
        ok: Hex(0xb6ff3b),
        warn: Hex(0xffc23b),
        danger: Hex(0xff5c3b),
        samples: [
            Hex(0xb6ff3b),
            Hex(0xe2ff4a),
            Hex(0x7dff6a),
            Hex(0x4dffc0),
            Hex(0xd0ff8c),
        ],
    },
    type_: Typography {
        title: TextStyle::new(24.0, 800.0).mono().tracking(-0.5),
        label: TextStyle::new(12.0, 700.0).mono(),
        caption: TextStyle::new(10.0, 600.0).mono().tracking(0.4),
        value: TextStyle::new(11.0, 650.0).mono().features("'tnum' 1"),
        readout: TextStyle::new(20.0, 700.0).mono(),
    },
    materials: Materials {
        pad: Material {
            base: Hex(0x3b403c),
            roughness: 0.92,
            dome: -0.55,
            bevel: 9.0,
        },
        pad_missing: Material {
            base: Hex(0x262927),
            roughness: 1.0,
            dome: -0.25,
            bevel: 6.0,
        },
        knob: Material {
            base: Hex(0x2f3430),
            roughness: 0.85,
            dome: 0.55,
            bevel: 6.0,
        },
        fader_cap: Material {
            base: Hex(0x454b46),
            roughness: 0.8,
            dome: 0.25,
            bevel: 4.0,
        },
        trim: Metal::Gunmetal,
    },
    space: Space {
        pad_radius: 9.0,
        ..SPACE
    },
    motion: Motion {
        glow_radius: 16.0,
        glow_intensity: 1.15,
        ripple_opacity: 0.7,
        ..MOTION
    },
    surfaces: Surfaces {
        display_grain: 0.05,
        display_bloom: 0.55,
        glass: false,
        lit_face: 0.2,
    },
};

pub const SPACE_PROGRAM: Theme = Theme {
    id: ThemeId::SpaceProgram,
    name: "Space Program",
    tagline: "IVORY PANEL  /  SMOKED GLASS",
    palette: Palette {
        backdrop: [Hex(0xe8e1d2), Hex(0xd3c9b5)],
        panel: [Hex(0xf4efe4), Hex(0xe3dccd)],
        edge: [Hex(0xffffff), Hex(0xb3a995)],
        inset: Hex(0xcbc1ad),
        shadow: Hex(0x7d725f),
        ink: Hex(0x29251f),
        ink_dim: Hex(0x6a6256),
        on_pad: Hex(0x29251f),
        on_pad_dim: Hex(0x665e52),
        accent: Hex(0xff5f0f),
        focus: Hex(0x1d4ed8),
        display: [Hex(0x2e2924), Hex(0x15120f)],
        display_ink: Hex(0xffa860),
        display_dim: Hex(0x8a7461),
        ok: Hex(0x2f8f46),
        warn: Hex(0xb87800),
        danger: Hex(0xc8330c),
        samples: [
            Hex(0xff5f0f),
            Hex(0xff9a1f),
            Hex(0xe8431c),
            Hex(0x2b8fd9),
            Hex(0xf2b33a),
        ],
    },
    type_: Typography {
        title: TextStyle::new(25.0, 850.0).tracking(-0.6),
        label: TextStyle::new(12.0, 750.0).tracking(0.1),
        caption: TextStyle::new(10.0, 650.0).tracking(0.8),
        value: TextStyle::new(11.5, 650.0).features("'tnum' 1"),
        readout: TextStyle::new(21.0, 750.0).features("'tnum' 1"),
    },
    materials: Materials {
        pad: Material {
            base: Hex(0xf1ebde),
            roughness: 0.3,
            dome: 0.28,
            bevel: 7.0,
        },
        pad_missing: Material {
            base: Hex(0xd8d0c0),
            roughness: 0.9,
            dome: 0.0,
            bevel: 4.0,
        },
        knob: Material {
            base: Hex(0x2c2823),
            roughness: 0.28,
            dome: 0.6,
            bevel: 5.0,
        },
        fader_cap: Material {
            base: Hex(0x34302a),
            roughness: 0.3,
            dome: 0.3,
            bevel: 4.0,
        },
        trim: Metal::Chrome,
    },
    space: Space {
        pad_radius: 11.0,
        ..SPACE
    },
    motion: Motion {
        glow_radius: 12.0,
        glow_intensity: 0.7,
        ripple_opacity: 0.5,
        ..MOTION
    },
    surfaces: Surfaces {
        display_grain: 0.0,
        display_bloom: 0.35,
        glass: true,
        lit_face: 0.3,
    },
};

pub const AFTERHOURS: Theme = Theme {
    id: ThemeId::Afterhours,
    name: "Afterhours",
    tagline: "CHARCOAL  /  VIOLET EDGE",
    palette: Palette {
        backdrop: [Hex(0x18171c), Hex(0x0b0a0e)],
        panel: [Hex(0x211f27), Hex(0x18171c)],
        edge: [Hex(0x37343f), Hex(0x07070a)],
        inset: Hex(0x0c0b10),
        shadow: Hex(0x030305),
        ink: Hex(0xe8e5f2),
        ink_dim: Hex(0x928ea3),
        on_pad: Hex(0xdcd8e8),
        on_pad_dim: Hex(0x8f8aa2),
        accent: Hex(0xa47dff),
        focus: Hex(0xe0d4ff),
        display: [Hex(0x14121b), Hex(0x0b0a10)],
        display_ink: Hex(0xc3a6ff),
        display_dim: Hex(0x6e6390),
        ok: Hex(0x7fe0b4),
        warn: Hex(0xffc46b),
        danger: Hex(0xff5d7e),
        samples: [
            Hex(0xa47dff),
            Hex(0xd47dff),
            Hex(0x7d8dff),
            Hex(0xff7dc9),
            Hex(0x7dd6ff),
        ],
    },
    type_: Typography {
        title: TextStyle::new(24.0, 600.0).tracking(-0.4),
        label: TextStyle::new(12.0, 600.0).tracking(0.2),
        caption: TextStyle::new(10.0, 550.0).tracking(0.9),
        value: TextStyle::new(11.0, 600.0).features("'tnum' 1"),
        readout: TextStyle::new(20.0, 500.0).features("'tnum' 1"),
    },
    materials: Materials {
        pad: Material {
            base: Hex(0x2a2931),
            roughness: 0.72,
            dome: 0.1,
            bevel: 5.0,
        },
        pad_missing: Material {
            base: Hex(0x1d1c22),
            roughness: 1.0,
            dome: 0.0,
            bevel: 3.0,
        },
        knob: Material {
            base: Hex(0x26252c),
            roughness: 0.55,
            dome: 0.45,
            bevel: 5.0,
        },
        fader_cap: Material {
            base: Hex(0x302e37),
            roughness: 0.6,
            dome: 0.2,
            bevel: 4.0,
        },
        trim: Metal::None,
    },
    space: Space {
        pad_radius: 8.0,
        ..SPACE
    },
    motion: Motion {
        glow: GlowKind::Neon,
        glow_radius: 9.0,
        glow_intensity: 0.75,
        release: 0.26,
        voice_release: 0.40,
        ..MOTION
    },
    surfaces: Surfaces {
        display_grain: 0.0,
        display_bloom: 0.45,
        glass: false,
        lit_face: 0.16,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_round_trips_every_channel_and_mix_stays_in_gamut() {
        for hex in [0x000000, 0xffffff, 0xb6ff3b, 0x010203, 0xfffffe] {
            assert_eq!(Hex::from_slot(Hex(hex).slot()), Hex(hex));
        }
        assert_eq!(Hex(0x000000).mix(Hex(0xffffff), 0.0), Hex(0x000000));
        assert_eq!(Hex(0x000000).mix(Hex(0xffffff), 1.0), Hex(0xffffff));
        assert_eq!(Hex(0x123456).mix(Hex(0xffffff), 7.0), Hex(0xffffff));
    }

    #[test]
    fn every_theme_keeps_text_and_focus_readable() {
        for id in ThemeId::ALL {
            let t = id.theme();
            let p = &t.palette;
            for (name, ink, ground, min) in [
                ("ink on panel", p.ink, p.panel[1], 7.0),
                ("dim ink on panel", p.ink_dim, p.panel[0], 4.5),
                ("pad label", p.on_pad, t.materials.pad.base, 4.5),
                ("pad caption", p.on_pad_dim, t.materials.pad.base, 3.0),
                ("display ink", p.display_ink, p.display[0], 4.5),
                ("display caption", p.display_dim, p.display[1], 3.0),
                ("focus on panel", p.focus, p.panel[1], 3.0),
                ("focus on backdrop", p.focus, p.backdrop[0], 3.0),
                ("danger on panel", p.danger, p.panel[1], 3.0),
            ] {
                let ratio = contrast(ink, ground);
                assert!(ratio >= min, "{} {name}: {ratio:.2} < {min}", t.name);
            }
            assert_eq!(ThemeId::parse(id.slug()), Some(id));
        }
        assert_eq!(FxLevel::parse("LIGHT"), Some(FxLevel::Light));
    }

    #[test]
    fn pad_labels_stay_readable_under_their_own_light() {
        for id in ThemeId::ALL {
            let t = id.theme();
            let base = t.materials.pad.base;
            for light in t.palette.samples {
                let peak = base.mix(light, t.surfaces.lit_face);
                let sustain = base.mix(light, t.surfaces.lit_face * t.motion.sustain);
                let at_peak = contrast(t.palette.on_pad, peak);
                let held = contrast(t.palette.on_pad, sustain);
                assert!(at_peak >= 3.0, "{} {light:?} flash: {at_peak:.2}", t.name);
                assert!(held >= 4.5, "{} {light:?} sustain: {held:.2}", t.name);
            }
        }
        assert_eq!(ThemeId::parse("nope"), None);
    }
}
