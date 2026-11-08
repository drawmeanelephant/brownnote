//! Fixed voice pool. Samples and meters are owned before the callback starts.
//!
//! Samples live in 64 slots. The UI loads a sample into a free slot, triggers
//! voices from it, and unloads it; a voice keeps reading its slot until it
//! ends, and the sample returns to the UI on a queue once no voice reads it.
//! The callback never allocates, frees, locks, or changes an `Arc` count.
use crate::performance::{
    EventId, StreamEpoch,
    scheduler::{self, ScheduleReceiver, ScheduleSender, ScheduledAction, Trigger},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed},
};

pub const VOICES: usize = 32;
/// Extra fading voices, not counted by pad meters. At most 40 voices render.
/// A ninth overlapping steal cuts the oldest tail, never delays the new hit.
pub const STEAL_TAILS: usize = 8;
pub const QUEUE_CAPACITY: usize = 256;
/// Sample slots. 0..5 hold the kit's files; renders take free slots above.
pub const SLOTS: usize = 64;

#[derive(Clone, Debug)]
pub struct Sample {
    pub data: Arc<[f32]>,
    pub rate: u32,
}

#[derive(Clone)]
pub enum Command {
    Trigger {
        pad: usize,
        slot: usize,
        /// The pad's base file index. Choke works across pads that share it.
        group: usize,
        velocity: f32,
        gain: f32,
        pan: f32,
        semitones: f32,
    },
    SetMaster(f32),
    SetChoke(bool),
    Panic,
    Load {
        slot: usize,
        sample: Sample,
    },
    Unload {
        slot: usize,
    },
}

/// The UI's view of what each slot holds, kept so a new `Output` can be
/// rebuilt after a device failure. Cloning it clones `Arc`s on the UI thread.
#[derive(Clone)]
pub struct Slots(Box<[Option<Sample>]>);

impl Slots {
    pub fn empty() -> Self {
        Self(vec![None; SLOTS].into_boxed_slice())
    }

    /// Put `samples` in slots 0..n and leave the rest empty.
    pub fn from_samples(samples: impl IntoIterator<Item = Sample>) -> Self {
        let mut slots = Self::empty();
        for (index, sample) in samples.into_iter().take(SLOTS).enumerate() {
            slots.0[index] = Some(sample);
        }
        slots
    }

    pub fn get(&self, slot: usize) -> Option<&Sample> {
        self.0.get(slot).and_then(Option::as_ref)
    }

    pub fn set(&mut self, slot: usize, sample: Option<Sample>) {
        if let Some(entry) = self.0.get_mut(slot) {
            *entry = sample;
        }
    }

    pub fn is_empty(&self, slot: usize) -> bool {
        self.get(slot).is_none()
    }
}

impl From<Box<[Sample]>> for Slots {
    fn from(samples: Box<[Sample]>) -> Self {
        Self::from_samples(samples.into_vec())
    }
}

impl From<Vec<Sample>> for Slots {
    fn from(samples: Vec<Sample>) -> Self {
        Self::from_samples(samples)
    }
}

/// A sample coming back from the mixer to the UI thread.
#[derive(Debug)]
pub enum Returned {
    /// A slot finished retiring, so the UI may reuse it.
    Retired { slot: usize, sample: Sample },
    /// A `Load` into a slot that already held a sample. The slot keeps its
    /// sample; the UI just drops this one.
    Rejected(Sample),
}

pub struct Meters {
    /// Active voices per pad (including choke/panic fades), excluding steal tails.
    pub pads: [AtomicU32; 16],
    /// Mastered stereo peak before clamping, since the UI last exchanged it
    /// for zero (positive f32 bits). Saturates at f32::MAX.
    pub peak: AtomicU32,
    /// Either mastered channel exceeded ±1 before clamping. Stays set until
    /// the UI exchanges it for false; the UI owns the 1.5-second clip latch.
    /// Independent of `peak`, whose f32 rounding can hide a tiny overload.
    pub clipped: AtomicBool,
    pub frames: AtomicU64,
}

impl Default for Meters {
    fn default() -> Self {
        Self {
            pads: std::array::from_fn(|_| AtomicU32::new(0)),
            peak: AtomicU32::new(0),
            clipped: AtomicBool::new(false),
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
    event: Option<EventId>,
    pad: usize,
    slot: usize,
    group: usize,
    position: f64,
    step: f64,
    left: f32,
    right: f32,
    age: u64,
    fade: Option<u32>,
}

pub struct Mixer {
    schedule: ScheduleReceiver,
    schedule_sender: Option<ScheduleSender>,
    frame: u64,
    slots: Box<[Option<Sample>]>,
    /// Set by `Unload`; the sample leaves the slot once no voice reads it.
    retiring: Box<[bool]>,
    voices: [Option<Voice>; VOICES],
    tails: [Option<Voice>; STEAL_TAILS],
    /// Samples ready to go back to the UI. Pushing moves the `Arc`; it never
    /// changes its count.
    retired: rtrb::Producer<Returned>,
    /// Rejected loads that found the return queue full. Preallocated, so the
    /// callback never drops a sample and never allocates.
    overflow: [Option<Returned>; SLOTS],
    rate: u32,
    fade_frames: u32,
    age: u64,
    choke: bool,
    master: f32,
    target_master: f32,
    meters: Arc<Meters>,
}

impl Mixer {
    pub fn new(
        slots: Slots,
        rate: u32,
        meters: Arc<Meters>,
        retired: rtrb::Producer<Returned>,
    ) -> Self {
        Self::new_with_epoch(slots, rate, meters, retired, StreamEpoch::fresh())
    }

    /// Construct off callback. A physical output supplies its timestamp epoch.
    pub fn new_with_epoch(
        slots: Slots,
        rate: u32,
        meters: Arc<Meters>,
        retired: rtrb::Producer<Returned>,
        epoch: StreamEpoch,
    ) -> Self {
        debug_assert_eq!(slots.0.len(), SLOTS);
        let (sender, schedule) = scheduler::channel(epoch, rate);
        Self {
            schedule,
            schedule_sender: Some(sender),
            frame: 0,
            slots: slots.0,
            retiring: vec![false; SLOTS].into_boxed_slice(),
            voices: [None; VOICES],
            tails: [None; STEAL_TAILS],
            retired,
            overflow: std::array::from_fn(|_| None),
            rate,
            fade_frames: (f64::from(rate) * 0.005).ceil() as u32,
            age: 0,
            choke: true,
            master: 0.8,
            target_master: 0.8,
            meters,
        }
    }

    /// Move the unique producer to the input/transport thread before playback.
    pub fn take_scheduler(&mut self) -> Option<ScheduleSender> {
        self.schedule_sender.take()
    }

    pub fn command(&mut self, command: Command) {
        match command {
            Command::SetMaster(v) => self.target_master = v,
            Command::SetChoke(on) => self.choke = on,
            Command::Panic => {
                self.schedule.cancel_queued();
                for v in self.voices.iter_mut().chain(&mut self.tails).flatten() {
                    v.fade = Some(v.fade.unwrap_or(self.fade_frames).min(self.fade_frames));
                }
            }
            Command::Load { slot, sample } => match self.slots.get_mut(slot) {
                Some(entry) if entry.is_none() => *entry = Some(sample),
                // A UI bug: never overwrite a sample voices may be reading.
                _ => self.return_item(Returned::Rejected(sample)),
            },
            Command::Unload { slot } => {
                if self.slots.get(slot).is_some_and(Option::is_some) {
                    self.retiring[slot] = true;
                    self.schedule.cancel_slot(slot);
                }
            }
            Command::Trigger {
                pad,
                slot,
                group,
                velocity,
                gain,
                pan,
                semitones,
            } => {
                self.trigger(
                    Trigger {
                        pad,
                        slot,
                        group,
                        velocity,
                        gain,
                        pan,
                        semitones,
                    },
                    None,
                );
            }
        }
    }

    fn trigger(&mut self, trigger: Trigger, event: Option<EventId>) -> bool {
        let Trigger {
            pad,
            slot,
            group,
            velocity,
            gain,
            pan,
            semitones,
        } = trigger;
        let Some(src) = self.slots.get(slot).and_then(Option::as_ref) else {
            return false;
        };
        if self.retiring[slot] || pad >= 16 || src.data.is_empty() {
            return false;
        }
        if self.choke {
            for v in self
                .voices
                .iter_mut()
                .chain(&mut self.tails)
                .flatten()
                .filter(|v| v.group == group)
            {
                v.fade = Some(v.fade.unwrap_or(self.fade_frames).min(self.fade_frames));
            }
        }
        let voice = self
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
        if let Some(mut old) = self.voices[voice].take() {
            // Preserve a fade already in progress, including its phase.
            old.fade = Some(old.fade.unwrap_or(self.fade_frames).min(self.fade_frames));
            let tail = self
                .tails
                .iter()
                .position(Option::is_none)
                .unwrap_or_else(|| {
                    self.tails
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, v)| v.map_or(u64::MAX, |v| v.age))
                        .map_or(0, |(i, _)| i)
                });
            self.tails[tail] = Some(old);
        }
        let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        self.age += 1;
        self.voices[voice] = Some(Voice {
            event,
            pad,
            slot,
            group,
            position: 0.0,
            step: f64::from(src.rate) / f64::from(self.rate)
                * 2.0_f64.powf(f64::from(semitones) / 12.0),
            left: theta.cos() * velocity * gain,
            right: theta.sin() * velocity * gain,
            age: self.age,
            fade: None,
        });
        true
    }

    /// A sample the mixer can't use goes straight back to the UI.
    fn return_item(&mut self, item: Returned) {
        match self.retired.push(item) {
            Ok(()) => {}
            Err(rtrb::PushError::Full(item)) => {
                if let Some(empty) = self.overflow.iter_mut().find(|slot| slot.is_none()) {
                    *empty = Some(item);
                }
            }
        }
    }

    /// Hand back retiring samples no voice reads, and flush the overflow.
    /// With nowhere to put a sample, the slot stays retiring and is retried
    /// on the next block.
    fn collect(&mut self) {
        for entry in &mut self.overflow {
            let Some(item) = entry.take() else { continue };
            if let Err(rtrb::PushError::Full(item)) = self.retired.push(item) {
                *entry = Some(item);
                break;
            }
        }
        for index in 0..SLOTS {
            if !self.retiring[index] {
                continue;
            }
            if self
                .voices
                .iter()
                .chain(&self.tails)
                .flatten()
                .any(|v| v.slot == index)
            {
                continue;
            }
            let Some(sample) = self.slots[index].take() else {
                self.retiring[index] = false;
                continue;
            };
            let item = Returned::Retired {
                slot: index,
                sample,
            };
            match self.retired.push(item) {
                Ok(()) => self.retiring[index] = false,
                // No room yet: the slot stays retiring and is retried next block.
                Err(rtrb::PushError::Full(Returned::Retired { sample, .. })) => {
                    self.slots[index] = Some(sample);
                }
                Err(rtrb::PushError::Full(Returned::Rejected(_))) => {
                    unreachable!("only retirement items are pushed here")
                }
            }
        }
    }

    /// Bounded draining prevents a busy producer from extending a callback
    /// indefinitely. It also stops when a returned sample would have nowhere
    /// to go, so the command stays queued for the next block.
    pub fn drain(&mut self, commands: &mut rtrb::Consumer<Command>) {
        for _ in 0..QUEUE_CAPACITY {
            if self.retired.is_full() && self.overflow.iter().all(Option::is_some) {
                break;
            }
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
        let slots = &self.slots;
        let retiring = &self.retiring;
        if self.schedule.drain(|event| match event.action {
            ScheduledAction::NoteOn { trigger, .. } => {
                trigger.pad < 16
                    && slots
                        .get(trigger.slot)
                        .and_then(Option::as_ref)
                        .is_some_and(|s| !s.data.is_empty())
                    && !retiring[trigger.slot]
            }
            ScheduledAction::NoteOff { .. } => true,
        }) {
            self.command(Command::Panic);
        }
        let frames = output.len() / channels;
        let start_master = self.master;
        let mut peak = 0.0_f64;
        for (i, frame) in output.chunks_exact_mut(channels).enumerate() {
            while let Some(event) = self.schedule.pop_due(self.frame) {
                match event.action {
                    ScheduledAction::NoteOn { id, trigger } => {
                        if !self.trigger(trigger, Some(id)) {
                            self.schedule.unavailable();
                        }
                    }
                    ScheduledAction::NoteOff { id } => {
                        for v in self.voices.iter_mut().chain(&mut self.tails).flatten() {
                            if v.event == Some(id) {
                                v.fade =
                                    Some(v.fade.unwrap_or(self.fade_frames).min(self.fade_frames));
                            }
                        }
                    }
                }
            }
            let mut left = 0.0_f64;
            let mut right = 0.0_f64;
            for slot in self.voices.iter_mut().chain(&mut self.tails) {
                let Some(v) = slot else { continue };
                let src = match &self.slots[v.slot] {
                    Some(sample) => &sample.data,
                    // Unreachable: a slot is emptied only when no voice reads it.
                    None => {
                        *slot = None;
                        continue;
                    }
                };
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
            let left = left * f64::from(master);
            let right = right * f64::from(master);
            peak = peak.max(left.abs()).max(right.abs());
            let left = left.clamp(-1.0, 1.0) as f32;
            let right = right.clamp(-1.0, 1.0) as f32;
            frame.fill(convert(0.0));
            if channels == 1 {
                frame[0] = convert((left + right) * 0.5);
            } else {
                frame[0] = convert(left);
                frame[1] = convert(right);
            }
            self.frame = self.frame.saturating_add(1);
        }
        self.schedule.cursor(self.frame);
        if frames > 0 {
            self.master = self.target_master;
        }
        output[frames * channels..].fill(convert(0.0));
        self.collect();
        let mut counts = [0; 16];
        for v in self.voices.iter().flatten() {
            counts[v.pad] += 1;
        }
        for (meter, count) in self.meters.pads.iter().zip(counts) {
            meter.store(count, Relaxed);
        }
        if peak > 1.0 {
            self.meters.clipped.store(true, Relaxed);
        }
        let peak = peak.min(f64::from(f32::MAX)) as f32;
        self.meters.peak.fetch_max(peak.to_bits(), Relaxed);
        self.meters.frames.fetch_add(frames as u64, Relaxed);
    }
}

#[cfg(test)]
mod tests;
