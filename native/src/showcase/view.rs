//! The play view: header, sample display, pads over their light, the selected pad's knobs, the
//! master fader beside the output meter, and the look switches. Every colour, size and motion
//! comes from the theme through `look::controls`.

use crate::params::Param;
use crate::state::{State, View};
use brownnote_native::{
    engine::Command,
    kit::{FILES, PADS},
    look::{
        controls::{
            FaderView, KnobView, PadView, Skin, Tone, Wave, bloom, button, caption, display, fader,
            knob, led, pad, pad_glow, panel, playhead, styled, waveform,
        },
        motion::RippleShot,
        theme::{FxLevel, Hex, ThemeId},
    },
};
use rune::{Cx, Drag, El, Phase, Tx, col, el, row};
use std::time::Duration;

/// Knob travel: points of vertical drag for the whole range.
const KNOB_TRAVEL: f32 = 160.0;
const SIDE: f32 = 236.0;

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    if s.reduced_motion {
        cx.reduced_motion(true);
    }
    let skin = s.skin(cx.is_reduced_motion());
    let size = cx.layout_size("window");
    let wave_w = cx.layout_size("sample-wave").map(|[w, _]| w);
    let tall = size.is_some_and(|[_, h]| h > 720.0);
    let wide = size.is_none_or(|[w, _]| w > 1000.0);
    let t = skin.theme;
    let p = &t.palette;
    let body = match s.view {
        View::Play => row()
            .key("body")
            .grow(1.0)
            .min_h(0.0)
            .gap(t.space.gap)
            .child(pads(s, &skin))
            .child(side(s, &skin)),
        View::Lab => crate::lab::view(s, &skin),
    };
    let mut root = col()
        .key("window")
        .role("window")
        .gradient(p.backdrop[0].color(), p.backdrop[1].color())
        .padding(t.space.outer)
        .gap(t.space.gap)
        .child(header(s, &skin))
        .child(screen(s, &skin, tall, wave_w))
        .child(body)
        .child(footer(s, &skin, wide));
    if !s.kit.banner.is_empty() {
        root = root.child(
            styled(&s.kit.banner, t.type_.caption, p.danger)
                .key("asset-errors")
                .label(&s.kit.banner)
                .truncate(),
        );
    }
    if s.output.dropped > 0 {
        root = root.child(caption(
            &skin,
            format!("Command queue full: {} dropped", s.output.dropped),
        ));
    }
    root
}

fn header(s: &State, skin: &Skin) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let mut controls = row()
        .gap(8.0)
        .center_y()
        .child(
            button(skin, "lab", "EFFECT LAB", Tone::Toggle(s.view == View::Lab))
                .role("switch")
                .on_press(|s: &mut State| {
                    s.view = match s.view {
                        View::Play => View::Lab,
                        View::Lab => View::Play,
                    }
                }),
        )
        .child(
            button(skin, "choke", "CHOKE", Tone::Toggle(s.choke))
                .role("switch")
                .on_press(|s: &mut State| {
                    s.choke = !s.choke;
                    s.output.send(Command::SetChoke(s.choke));
                }),
        )
        .child(
            button(skin, "panic", "PANIC  /  ESC", Tone::Danger).on_press_tx(
                |s: &mut State, tx| {
                    s.panic();
                    tx.wake_after(Duration::from_millis(1));
                },
            ),
        );
    if s.output.failed {
        controls = controls.child(
            button(skin, "retry-audio", "RETRY AUDIO", Tone::Plain)
                .on_press(|s: &mut State| s.retry()),
        );
    }
    row()
        .h(44.0)
        .gap(12.0)
        .justify_between()
        .center_y()
        .child(
            col()
                .gap(3.0)
                .min_w(0.0)
                .child(styled("brownnote", t.type_.title, p.ink))
                .child(
                    styled(
                        format!("BN–01   /   {}", t.tagline),
                        t.type_.caption,
                        p.ink_dim,
                    )
                    .truncate(),
                ),
        )
        .child(controls)
}

fn screen(s: &State, skin: &Skin, tall: bool, wave_w: Option<f32>) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let spec = &PADS[s.selected];
    let sample = &s.kit.samples[spec.sample];
    let seconds = sample.data.len() as f32 / sample.rate.max(1) as f32;
    let dim = |v: String| styled(v, t.type_.caption, p.display_dim);
    let line = s.playhead().zip(wave_w).map(|(at, w)| {
        playhead(
            at,
            w,
            p.display_ink.mix(p.ink, 0.5).color(),
            skin.fx == FxLevel::Full,
        )
    });
    let wave = |ink: rune::Color, stroke: f32| {
        waveform(&Wave {
            bins: &s.waves[spec.sample],
            ink,
            grid: Some(p.display_dim.alpha(0.18)),
            stroke,
        })
    };
    let missing = s.kit.errors[spec.sample].is_some();
    let ink = if missing {
        p.display_dim
    } else {
        p.display_ink
    };
    let voices: u32 = s.counts.iter().sum();
    let (light, message) = if s.output.failed {
        (p.danger, s.output.status.as_str())
    } else if s.output.status.starts_with("Headless") || s.output.status.contains("off") {
        (p.warn, s.output.status.as_str())
    } else {
        (p.ok, s.output.status.as_str())
    };
    let content = row()
        .grow(1.0)
        .min_w(0.0)
        .gap(18.0)
        .child(
            col()
                .w(150.0)
                .gap(4.0)
                .center_y()
                .child(
                    dim(format!("SAMPLE {:02}  /  SELECTED", s.selected + 1))
                        .w_full()
                        .truncate(),
                )
                .child(
                    styled(spec.label, t.type_.readout, ink)
                        .key("selected-name")
                        .w_full()
                        .truncate(),
                )
                .children(tall.then(|| {
                    dim(format!(
                        "{seconds:.2} S   /   {:.1} KHZ",
                        sample.rate as f32 / 1000.0
                    ))
                    .w_full()
                    .truncate()
                })),
        )
        .child(
            col()
                .key("sample-waveform")
                .grow(1.0)
                .min_w(0.0)
                .gap(3.0)
                .role("img")
                .label(format!(
                    "Normalized sample waveform: {}",
                    FILES[spec.sample]
                ))
                .child(
                    col()
                        .key("sample-wave")
                        .grow(1.0)
                        .w_full()
                        .min_h(4.0)
                        .child(bloom(skin, wave(ink.color(), 1.6), wave(ink.color(), 3.0)))
                        .children(line),
                )
                .child(
                    row()
                        .justify_between()
                        .child(dim("0.00".into()))
                        .child(dim("SAMPLE WAVEFORM".into()))
                        .child(dim(format!("{seconds:.2} S"))),
                ),
        )
        .child(
            col()
                .w(170.0)
                .gap(6.0)
                .center_y()
                .child(dim("AUDIO".into()).w_full())
                .child({
                    let message_text = styled(message, t.type_.caption, p.display_dim)
                        .grow(1.0)
                        .min_w(0.0)
                        .help_when_truncated(message);
                    row()
                        .key("audio-status")
                        .label(message)
                        .w_full()
                        .gap(6.0)
                        .center_y()
                        .child(led(skin, light, 1.0))
                        .child(if tall {
                            message_text.max_lines(2)
                        } else {
                            message_text.truncate()
                        })
                })
                .child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .center_y()
                        .child(led(skin, p.display_ink, if voices > 0 { 1.0 } else { 0.0 }))
                        .child(
                            dim(format!("{voices:02} / 32 VOICES"))
                                .key("voice-count")
                                .grow(1.0)
                                .min_w(0.0)
                                .truncate(),
                        ),
                ),
        );
    display(skin, "sample-display", content).h(if tall { 112.0 } else { 84.0 })
}

/// One pad's view of the state: light, ripples, hover and press.
pub fn pad_view<'a>(s: &'a State, skin: &Skin, i: usize, ripples: &'a [RippleShot]) -> PadView<'a> {
    let spec = &PADS[i];
    let m = &skin.theme.motion;
    PadView {
        index: i,
        label: spec.label,
        key: spec.key,
        light: skin.theme.palette.samples[spec.sample],
        level: s.lights[i].level(s.now, m),
        selected: s.selected == i,
        hovered: s.hover == Some(i),
        pressed: s.held == Some(i),
        missing: s.kit.errors[spec.sample].as_deref(),
        voices: s.counts[i],
        pitch: s.settings[i].pitch,
        bins: &s.waves[spec.sample],
        ripples,
    }
}

pub fn ripples(s: &State, skin: &Skin, i: usize) -> Vec<RippleShot> {
    s.ripples
        .live(s.now, skin.theme.motion.ripple_life, i)
        .collect()
}

/// Hover, the main app's velocity curve (higher hits are louder) and pointer panic.
pub fn playable(root: El, i: usize, missing: bool) -> El {
    let root = root.on_hover_change(move |s: &mut State, inside: bool, _: &mut Tx| {
        if inside {
            s.hover = Some(i);
        } else if s.hover == Some(i) {
            s.hover = None;
        }
    });
    if missing {
        return root;
    }
    root.on_drag(move |s: &mut State, d: &mut Drag, tx| match d.phase {
        Phase::Down => {
            crate::latency::input(tx);
            let at = [
                (d.x / d.w.max(1.0)).clamp(0.0, 1.0),
                (d.y / d.h.max(1.0)).clamp(0.0, 1.0),
            ];
            s.trigger(i, 1.0 - 0.6 * at[1], at);
            s.held = Some(i);
            tx.wake_after(Duration::from_millis(1));
        }
        Phase::Up => {
            if s.held == Some(i) {
                s.held = None;
            }
            if d.cancelled() {
                // Rune consumes Escape to cancel a held pointer before window keys.
                s.panic();
                tx.wake_after(Duration::from_millis(1));
            }
        }
        _ => {}
    })
}

fn grid(gap: f32, cell: impl Fn(usize) -> El) -> El {
    let mut grid = col().grow(1.0).min_h(0.0).gap(gap);
    for r in 0..4 {
        let mut line = row().grow(1.0).min_h(0.0).gap(gap);
        for c in 0..4 {
            line = line.child(cell(r * 4 + c));
        }
        grid = grid.child(line);
    }
    grid
}

fn pads(s: &State, skin: &Skin) -> El {
    let t = skin.theme;
    let m = &t.motion;
    let gap = t.space.pad_gap;
    let glow = grid(gap, |i| {
        let light = t.palette.samples[PADS[i].sample];
        let level = if s.kit.errors[PADS[i].sample].is_some() {
            0.0
        } else {
            s.lights[i].level(s.now, m)
        };
        pad_glow(skin, light, level)
    })
    .absolute(0.0, 0.0)
    .w_full()
    .h_full()
    .a11y_hidden();
    let pads = grid(gap, |i| {
        let shots = ripples(s, skin, i);
        let view = pad_view(s, skin, i, &shots);
        playable(pad(skin, &view), i, view.missing.is_some())
    })
    .key("pads");
    panel(skin)
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .padding(12.0)
        .gap(8.0)
        .child(
            row()
                .h(14.0)
                .justify_between()
                .center_y()
                .child(caption(skin, "01—16   /   PRESSURE PADS"))
                .child(caption(skin, "HIT HIGHER. GO LOUDER.")),
        )
        .child(col().grow(1.0).min_h(0.0).child(glow).child(pads))
}

/// Range, keyboard steps and accessibility value for a parameter's control.
fn ranged(control: El, param: Param, s: &State) -> El {
    let spec = param.spec();
    let value = param.get(s);
    control
        .range_value(f64::from(value), f64::from(spec.min), f64::from(spec.max))
        .range_step(f64::from(spec.step))
        .a11y_value(param.display(value))
        .on_step(move |s: &mut State, steps, _| {
            param.set(s, param.get(s) + steps as f32 * param.spec().step);
        })
        .on_step_limit(move |s: &mut State, end, _| {
            let spec = param.spec();
            param.set(s, if end { spec.max } else { spec.min });
        })
}

fn knob_for(s: &State, skin: &Skin, param: Param, accent: Hex) -> El {
    let spec = param.spec();
    let display = param.display(param.get(s));
    knob(
        skin,
        &KnobView {
            key: spec.key,
            label: spec.label,
            value: param.normalized(s),
            display: &display,
            accent,
            bipolar: param == Param::Pan,
        },
        |body| {
            ranged(body, param, s).on_drag(move |s: &mut State, d: &mut Drag, tx| match d.phase {
                Phase::Down => d.data = param.get(s),
                Phase::Move => {
                    let spec = param.spec();
                    param.set(s, d.data - d.dy / KNOB_TRAVEL * (spec.max - spec.min));
                    tx.instant();
                }
                Phase::Up => {
                    if d.cancelled() {
                        param.set(s, d.data);
                    }
                }
            })
        },
    )
}

fn fader_for(s: &State, skin: &Skin, param: Param, accent: Hex) -> El {
    let spec = param.spec();
    let display = param.display(param.get(s));
    let v = param.normalized(s);
    let ch = brownnote_native::look::controls::FADER_CAP[1];
    let to_value = move |y: f32, h: f32| {
        let spec = param.spec();
        let k = 1.0 - ((y - ch / 2.0) / (h - ch).max(1.0)).clamp(0.0, 1.0);
        spec.min + k * (spec.max - spec.min)
    };
    fader(
        skin,
        &FaderView {
            key: spec.key,
            label: spec.label,
            value: v,
            display: &display,
            accent,
        },
        |body| {
            ranged(body, param, s).on_drag(move |s: &mut State, d: &mut Drag, tx| {
                let h = d.h;
                let cap = |s: &State| ch / 2.0 + (1.0 - param.normalized(s)) * (h - ch);
                match d.phase {
                    Phase::Down => {
                        let on_cap = (d.y - cap(s)).abs() <= ch / 2.0 + 2.0;
                        d.data = param.get(s);
                        if !on_cap {
                            param.set(s, to_value(d.y, d.h));
                        }
                    }
                    Phase::Move => {
                        let spec = param.spec();
                        let at = ch / 2.0
                            + (1.0 - (d.data - spec.min) / (spec.max - spec.min)) * (d.h - ch);
                        let grab = if (d.start_y - at).abs() <= ch / 2.0 + 2.0 {
                            d.start_y - at
                        } else {
                            0.0
                        };
                        param.set(s, to_value(d.y - grab, d.h));
                        tx.instant();
                    }
                    Phase::Up => {
                        if d.cancelled() {
                            param.set(s, d.data);
                        }
                    }
                }
            })
        },
    )
}

fn side(s: &State, skin: &Skin) -> El {
    let t = skin.theme;
    let p = &t.palette;
    let spec = &PADS[s.selected];
    let accent = p.samples[spec.sample];
    let meter = brownnote_native::look::controls::meter(
        skin,
        &brownnote_native::look::controls::MeterView {
            level: s.meter.level,
            hold: s.meter.hold,
            hold_light: s.meter.hold_light,
            clip: s.meter.clipping(),
        },
    );
    panel(skin)
        .key("editor")
        .w(SIDE)
        .min_h(0.0)
        .padding(12.0)
        .gap(10.0)
        .child(
            col()
                .gap(2.0)
                .child(
                    styled(
                        format!("SHAPE {:02}  /  {}", s.selected + 1, spec.label),
                        t.type_.label,
                        p.ink,
                    )
                    .truncate(),
                )
                .child(caption(skin, "APPLIES TO THE NEXT HIT")),
        )
        .child(
            row()
                .justify_between()
                .child(knob_for(s, skin, Param::Gain, accent))
                .child(knob_for(s, skin, Param::Pan, accent))
                .child(knob_for(s, skin, Param::Pitch, accent)),
        )
        .child(el().h(1.0).w_full().fill(p.edge[1].alpha(0.6)))
        .child(
            row()
                .grow(1.0)
                .min_h(0.0)
                .gap(26.0)
                .justify_center()
                .child(fader_for(s, skin, Param::Master, p.accent))
                .child(meter),
        )
}

fn footer(s: &State, skin: &Skin, wide: bool) -> El {
    let mut bar = row()
        .h(30.0)
        .gap(6.0)
        .center_y()
        .child(caption(skin, "LOOK"));
    for id in ThemeId::ALL {
        bar = bar.child(
            button(
                skin,
                &format!("look-{}", id.slug()),
                &id.theme().name.to_ascii_uppercase(),
                Tone::Toggle(s.theme == id),
            )
            .on_press(move |s: &mut State| s.theme = id),
        );
    }
    bar = bar.child(el().w(8.0)).child(caption(skin, "FX"));
    for level in FxLevel::ALL {
        bar = bar.child(
            button(
                skin,
                &format!("fx-{}", level.slug()),
                &level.slug().to_ascii_uppercase(),
                Tone::Toggle(s.fx == level),
            )
            .on_press(move |s: &mut State| s.fx = level),
        );
    }
    bar.child(el().w(8.0))
        .child(
            button(
                skin,
                "reduced-motion",
                "REDUCE MOTION",
                Tone::Toggle(s.reduced_motion),
            )
            .role("switch")
            .on_press(|s: &mut State| s.reduced_motion = !s.reduced_motion),
        )
        .child(el().grow(1.0).min_w(0.0))
        .children(wide.then(|| {
            caption(skin, "1–4 / QWER / ASDF / ZXCV  ·  ESC PANIC")
                .key("hint")
                .min_w(0.0)
                .truncate()
        }))
}
