//! Fixed voice pool. Samples and meters are owned before the callback starts.
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering::Relaxed},
};

pub const VOICES: usize = 32;
pub const QUEUE_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct Sample {
    pub data: Arc<[f32]>,
    pub rate: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Trigger {
        pad: usize,
        sample: usize,
        velocity: f32,
        gain: f32,
        pan: f32,
        semitones: f32,
    },
    SetMaster(f32),
    SetChoke(bool),
    Panic,
}

pub struct Meters {
    pub pads: [AtomicU32; 16],
    /// Maximum since the UI last exchanged it for zero (positive f32 bits).
    pub peak: AtomicU32,
    pub frames: AtomicU64,
}

impl Default for Meters {
    fn default() -> Self {
        Self {
            pads: std::array::from_fn(|_| AtomicU32::new(0)),
            peak: AtomicU32::new(0),
            frames: AtomicU64::new(0),
        }
    }
}

impl Meters {
    pub fn counts(&self) -> [u32; 16] {
        std::array::from_fn(|i| self.pads[i].load(Relaxed))
    }
}

#[derive(Clone, Copy)]
struct Voice {
    pad: usize,
    sample: usize,
    position: f64,
    step: f64,
    left: f32,
    right: f32,
    age: u64,
    fade: Option<u32>,
}

pub struct Mixer {
    samples: Box<[Sample]>,
    voices: [Option<Voice>; VOICES],
    rate: u32,
    fade_frames: u32,
    age: u64,
    choke: bool,
    master: f32,
    target_master: f32,
    meters: Arc<Meters>,
}

impl Mixer {
    pub fn new(samples: Box<[Sample]>, rate: u32, meters: Arc<Meters>) -> Self {
        Self {
            samples,
            voices: [None; VOICES],
            rate,
            fade_frames: (f64::from(rate) * 0.005).ceil() as u32,
            age: 0,
            choke: true,
            master: 0.8,
            target_master: 0.8,
            meters,
        }
    }

    pub fn command(&mut self, command: Command) {
        match command {
            Command::SetMaster(v) => self.target_master = v,
            Command::SetChoke(on) => self.choke = on,
            Command::Panic => {
                for v in self.voices.iter_mut().flatten() {
                    v.fade = Some(v.fade.unwrap_or(self.fade_frames).min(self.fade_frames));
                }
            }
            Command::Trigger {
                pad,
                sample,
                velocity,
                gain,
                pan,
                semitones,
            } => {
                let Some(src) = self.samples.get(sample) else {
                    return;
                };
                if pad >= 16 || src.data.is_empty() {
                    return;
                }
                if self.choke {
                    for v in self
                        .voices
                        .iter_mut()
                        .flatten()
                        .filter(|v| v.sample == sample)
                    {
                        v.fade = Some(v.fade.unwrap_or(self.fade_frames).min(self.fade_frames));
                    }
                }
                let slot = self
                    .voices
                    .iter()
                    .position(Option::is_none)
                    .unwrap_or_else(|| {
                        self.voices
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, v)| v.map_or(u64::MAX, |v| v.age))
                            .map_or(0, |(i, _)| i)
                    });
                let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
                self.age += 1;
                self.voices[slot] = Some(Voice {
                    pad,
                    sample,
                    position: 0.0,
                    step: f64::from(src.rate) / f64::from(self.rate)
                        * 2.0_f64.powf(f64::from(semitones) / 12.0),
                    left: theta.cos() * velocity * gain,
                    right: theta.sin() * velocity * gain,
                    age: self.age,
                    fade: None,
                });
            }
        }
    }

    /// Bounded draining prevents a busy producer from extending a callback indefinitely.
    pub fn drain(&mut self, commands: &mut rtrb::Consumer<Command>) {
        for _ in 0..QUEUE_CAPACITY {
            let Ok(command) = commands.pop() else { break };
            self.command(command);
        }
    }

    pub fn render(&mut self, output: &mut [f32], channels: usize) {
        self.render_into(output, channels, |v| v);
    }

    /// Conversion writes straight into the device buffer, with no scratch allocation.
    pub fn render_into<T: Copy>(
        &mut self,
        output: &mut [T],
        channels: usize,
        convert: impl Fn(f32) -> T,
    ) {
        if channels == 0 {
            return;
        }
        let frames = output.len() / channels;
        let start_master = self.master;
        let mut peak = 0.0_f32;
        for (i, frame) in output.chunks_exact_mut(channels).enumerate() {
            let mut left = 0.0_f64;
            let mut right = 0.0_f64;
            for slot in &mut self.voices {
                let Some(v) = slot else { continue };
                let src = &self.samples[v.sample].data;
                let index = v.position as usize;
                let next = (index + 1).min(src.len() - 1);
                // Float WAVs may contain any finite f32, including values above unity.
                let a = f64::from(src[index]);
                let value = a + (f64::from(src[next]) - a) * v.position.fract();
                let fade = v.fade.map_or(1.0, |n| n as f32 / self.fade_frames as f32);
                left += value * f64::from(v.left) * f64::from(fade);
                right += value * f64::from(v.right) * f64::from(fade);
                v.position += v.step;
                if let Some(n) = &mut v.fade {
                    *n = n.saturating_sub(1);
                }
                if v.position >= src.len() as f64 || v.fade == Some(0) {
                    *slot = None;
                }
            }
            let master =
                start_master + (self.target_master - start_master) * (i + 1) as f32 / frames as f32;
            let left = (left * f64::from(master)).clamp(-1.0, 1.0) as f32;
            let right = (right * f64::from(master)).clamp(-1.0, 1.0) as f32;
            peak = peak.max(left.abs()).max(right.abs());
            frame.fill(convert(0.0));
            if channels == 1 {
                frame[0] = convert((left + right) * 0.5);
            } else {
                frame[0] = convert(left);
                frame[1] = convert(right);
            }
        }
        if frames > 0 {
            self.master = self.target_master;
        }
        output[frames * channels..].fill(convert(0.0));
        let mut counts = [0; 16];
        for v in self.voices.iter().flatten() {
            counts[v.pad] += 1;
        }
        for (meter, count) in self.meters.pads.iter().zip(counts) {
            meter.store(count, Relaxed);
        }
        self.meters.peak.fetch_max(peak.to_bits(), Relaxed);
        self.meters.frames.fetch_add(frames as u64, Relaxed);
    }
}

#[cfg(test)]
mod tests;
