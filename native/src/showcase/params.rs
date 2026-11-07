//! The four parameters the showcase edits, with the main app's ranges, steps and commands.

use crate::state::State;
use brownnote_native::engine::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    Gain,
    Pan,
    Pitch,
    Master,
}

pub struct Spec {
    pub key: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
}

impl Param {
    pub fn spec(self) -> Spec {
        let (key, label, min, max, step) = match self {
            Param::Gain => ("gain", "Gain", 0.0, 1.5, 0.05),
            Param::Pan => ("pan", "Pan", -1.0, 1.0, 0.05),
            Param::Pitch => ("pitch", "Pitch", -12.0, 12.0, 1.0),
            Param::Master => ("master", "Master", 0.0, 1.0, 0.01),
        };
        Spec {
            key,
            label,
            min,
            max,
            step,
        }
    }

    pub fn get(self, s: &State) -> f32 {
        match self {
            Param::Gain => s.settings[s.selected].gain,
            Param::Pan => s.settings[s.selected].pan,
            Param::Pitch => s.settings[s.selected].pitch,
            Param::Master => s.master,
        }
    }

    /// Snap to the step and range, then apply: pad settings take effect on the next hit, master
    /// goes to the engine at once.
    pub fn set(self, s: &mut State, value: f32) {
        let spec = self.spec();
        let value = ((value / spec.step).round() * spec.step).clamp(spec.min, spec.max);
        match self {
            Param::Gain => s.settings[s.selected].gain = value,
            Param::Pan => s.settings[s.selected].pan = value,
            Param::Pitch => s.settings[s.selected].pitch = value,
            Param::Master => {
                s.master = value;
                s.output.send(Command::SetMaster(value));
            }
        }
    }

    pub fn normalized(self, s: &State) -> f32 {
        let spec = self.spec();
        (self.get(s) - spec.min) / (spec.max - spec.min)
    }

    pub fn display(self, value: f32) -> String {
        match self {
            Param::Gain | Param::Master => format!("{:.0}%", value * 100.0),
            Param::Pan if value.abs() < 0.001 => "C".into(),
            Param::Pan => format!(
                "{} {:.0}%",
                if value < 0.0 { "L" } else { "R" },
                value.abs() * 100.0
            ),
            Param::Pitch => format!("{value:+.0} st"),
        }
    }
}
