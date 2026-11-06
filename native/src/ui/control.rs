use super::{State, accent, caption, ink};
use brownnote_native::engine::Command;
use rune::{Drag, El, Phase, col, el, row};

#[derive(Clone, Copy)]
pub(super) enum Control {
    Gain,
    Pan,
    Pitch,
    Master,
}

impl Control {
    fn spec(self) -> (&'static str, f32, f32, f32) {
        match self {
            Self::Gain => ("gain", 0.0, 1.5, 0.05),
            Self::Pan => ("pan", -1.0, 1.0, 0.05),
            Self::Pitch => ("pitch", -12.0, 12.0, 1.0),
            Self::Master => ("master", 0.0, 1.0, 0.01),
        }
    }
    fn get(self, s: &State) -> f32 {
        match self {
            Self::Gain => s.settings[s.selected].gain,
            Self::Pan => s.settings[s.selected].pan,
            Self::Pitch => s.settings[s.selected].pitch,
            Self::Master => s.master,
        }
    }
    fn set(self, s: &mut State, value: f32) {
        let (_, min, max, step) = self.spec();
        let value = ((value / step).round() * step).clamp(min, max);
        match self {
            Self::Gain => s.settings[s.selected].gain = value,
            Self::Pan => s.settings[s.selected].pan = value,
            Self::Pitch => s.settings[s.selected].pitch = value,
            Self::Master => {
                s.master = value;
                s.output.send(Command::SetMaster(value));
            }
        }
    }
}

pub(super) fn slider(s: &State, control: Control) -> El {
    let (key, min, max, step) = control.spec();
    let value = control.get(s);
    let normalized = (value - min) / (max - min);
    let display = match control {
        Control::Gain | Control::Master => format!("{:.0}%", value * 100.0),
        Control::Pan => {
            if value.abs() < 0.001 {
                "C".into()
            } else {
                format!(
                    "{} {:.0}%",
                    if value < 0.0 { "L" } else { "R" },
                    value.abs() * 100.0
                )
            }
        }
        Control::Pitch => format!("{value:+.0} st"),
    };
    let (w, h, k) = (154.0, 22.0, 14.0);
    let travel = w - k;
    let to_value = move |x: f32| min + ((x - k / 2.0) / travel).clamp(0.0, 1.0) * (max - min);
    col()
        .grow(1.0)
        .gap(6.0)
        .child(
            row()
                .justify_between()
                .child(caption(key.to_ascii_uppercase()))
                .child(
                    caption(&display)
                        .key(format!("{key}-value"))
                        .color(ink("#edf4dc")),
                ),
        )
        .child(
            el().key(key)
                .role("slider")
                .label(key)
                .focusable(true)
                .range_value(f64::from(value), f64::from(min), f64::from(max))
                .range_step(f64::from(step))
                .a11y_value(display)
                .on_step(move |s: &mut State, steps, _| {
                    control.set(s, control.get(s) + steps as f32 * step)
                })
                .on_step_limit(move |s: &mut State, end, _| {
                    control.set(s, if end { max } else { min })
                })
                .size(w, h)
                .on_drag(move |s: &mut State, d: &mut Drag, tx| match d.phase {
                    Phase::Down => {
                        d.data = control.get(s);
                        let knob = k / 2.0 + normalized * travel;
                        if (d.x - knob).abs() > k / 2.0 + 2.0 {
                            control.set(s, to_value(d.x));
                        }
                    }
                    Phase::Move => {
                        let knob = k / 2.0 + (d.data - min) / (max - min) * travel;
                        let grab = if (d.start_x - knob).abs() <= k / 2.0 + 2.0 {
                            d.start_x - knob
                        } else {
                            0.0
                        };
                        control.set(s, to_value(d.x - grab));
                        tx.instant();
                    }
                    Phase::Up => {
                        if d.cancelled() {
                            control.set(s, d.data);
                        }
                    }
                })
                .child(
                    el().key("track")
                        .size(w, 5.0)
                        .absolute(0.0, 8.5)
                        .radius(2.0)
                        .fill(ink("#101b14"))
                        .border(1.0, ink("#394a3c"))
                        .a11y_hidden(),
                )
                .child(
                    el().key("fill")
                        .size(k / 2.0 + normalized * travel, 3.0)
                        .absolute(0.0, 9.5)
                        .fill(accent(brownnote_native::kit::PADS[s.selected].sample))
                        .a11y_hidden(),
                )
                .child(
                    el().key("knob")
                        .size(k, k)
                        .absolute(normalized * travel, 4.0)
                        .radius(4.0)
                        .gradient(ink("#eef4d8"), ink("#9eac8b"))
                        .border_gradient(1.0, ink("#fcffe8"), ink("#748069"))
                        .shadow(2.0, 3.0, ink("#080f0a"))
                        .a11y_hidden(),
                ),
        )
}
