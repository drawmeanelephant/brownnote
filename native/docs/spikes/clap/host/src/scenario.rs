//! Scenario fixtures: tab-separated, one directive per line.
//!
//! ```text
//! sample_rate  48000
//! frames       48000
//! block        256
//! input        silence | sine <hz> <amp> <start> <end> | impulse <frame>
//! param        <frame> <param name> <plain value>
//! on           <frame> <note id> <key> <velocity 0..1>
//! off          <frame> <note id> <key>
//! ```

use std::path::Path;

use crate::clap::{HostError, InEvent, Instance};

#[derive(Clone, Debug)]
pub enum Input {
    Silence,
    Sine {
        hz: f64,
        amp: f32,
        start: u32,
        end: u32,
    },
    Impulse {
        frame: u32,
    },
}

#[derive(Clone, Debug)]
enum Directive {
    Param {
        frame: u32,
        name: String,
        value: f64,
    },
    On {
        frame: u32,
        id: i32,
        key: i16,
        velocity: f64,
    },
    Off {
        frame: u32,
        id: i32,
        key: i16,
    },
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub sample_rate: u32,
    pub frames: u32,
    pub block: u32,
    pub input: Input,
    directives: Vec<Directive>,
}

impl Scenario {
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("read scenario");
        let mut sc = Self {
            sample_rate: 48_000,
            frames: 48_000,
            block: 256,
            input: Input::Silence,
            directives: Vec::new(),
        };
        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            let msg = format!("{}:{}: bad directive {line:?}", path.display(), n + 1);
            let bad = || -> ! { panic!("{msg}") };
            let num = |i: usize| {
                f.get(i)
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or_else(|| bad())
            };
            match f[0] {
                "sample_rate" => sc.sample_rate = num(1) as u32,
                "frames" => sc.frames = num(1) as u32,
                "block" => sc.block = num(1) as u32,
                "input" => {
                    sc.input = match f.get(1).copied() {
                        Some("silence") => Input::Silence,
                        Some("sine") => Input::Sine {
                            hz: num(2),
                            amp: num(3) as f32,
                            start: num(4) as u32,
                            end: num(5) as u32,
                        },
                        Some("impulse") => Input::Impulse {
                            frame: num(2) as u32,
                        },
                        _ => bad(),
                    }
                }
                "param" => sc.directives.push(Directive::Param {
                    frame: num(1) as u32,
                    name: f.get(2).copied().unwrap_or_else(|| bad()).to_owned(),
                    value: num(3),
                }),
                "on" => sc.directives.push(Directive::On {
                    frame: num(1) as u32,
                    id: num(2) as i32,
                    key: num(3) as i16,
                    velocity: num(4),
                }),
                "off" => sc.directives.push(Directive::Off {
                    frame: num(1) as u32,
                    id: num(2) as i32,
                    key: num(3) as i16,
                }),
                _ => bad(),
            }
        }
        sc
    }

    /// Absolute-frame events with parameter names resolved for this plugin.
    /// Order within one frame follows the fixture, which CLAP requires to be
    /// non-decreasing in time.
    pub fn resolve(&self, inst: &Instance) -> Result<Vec<InEvent>, HostError> {
        let mut out = Vec::new();
        for d in &self.directives {
            out.push(match d {
                Directive::Param { frame, name, value } => InEvent::Param {
                    time: *frame,
                    param_id: inst.param_id(name)?,
                    value: *value,
                },
                Directive::On {
                    frame,
                    id,
                    key,
                    velocity,
                } => InEvent::NoteOn {
                    time: *frame,
                    note_id: *id,
                    key: *key,
                    velocity: *velocity,
                },
                Directive::Off { frame, id, key } => InEvent::NoteOff {
                    time: *frame,
                    note_id: *id,
                    key: *key,
                },
            });
        }
        out.sort_by_key(InEvent::time);
        Ok(out)
    }

    pub fn input_sample(&self, frame: u32, _channel: usize) -> f32 {
        match self.input {
            Input::Silence => 0.0,
            Input::Impulse { frame: at } => f32::from(u8::from(frame == at)),
            Input::Sine {
                hz,
                amp,
                start,
                end,
            } => {
                if frame < start || frame >= end {
                    0.0
                } else {
                    let t = f64::from(frame - start) / f64::from(self.sample_rate);
                    amp * (2.0 * std::f64::consts::PI * hz * t).sin() as f32
                }
            }
        }
    }
}
