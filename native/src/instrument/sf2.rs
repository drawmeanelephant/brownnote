//! The callback-side player. Constructed off the callback; afterwards no
//! method allocates, frees, locks or changes an `Arc` count.
use super::{InstrumentError, Preset};
use crate::performance::EventId;
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::{fmt, sync::Arc};

/// The backend treats MIDI channel 10 (index 9) as percussion.
const PERCUSSION: usize = 9;
/// Melodic backend channels the player rotates same-pitch notes across.
pub const CHANNELS: usize = 15;
/// Brownnote notes the player can address at once. A further note ends the
/// oldest one first (by ID) rather than losing track of it.
pub const HELD_CAPACITY: usize = 128;
/// Backend voices. An SF2 note may layer several regions, each one voice.
pub const POLYPHONY: usize = 64;
/// Backend render block. A note starts on the next block boundary of the
/// backend's own stream, so onsets can trail their exact frame by < BLOCK.
pub const BLOCK: usize = 32;
/// Note-ons that arrive during a panic fade start right after it.
const PENDING: usize = 32;

#[derive(Clone, Copy)]
struct Held {
    id: EventId,
    channel: u8,
    key: u8,
    order: u64,
}

pub struct Player {
    synth: Synthesizer,
    preset: Preset,
    rate: u32,
    held: [Option<Held>; HELD_CAPACITY],
    /// Last start/end per channel and key. A new note takes the free channel
    /// whose copy of that key was used longest ago, so a release tail still
    /// ringing on one channel is not re-released by a later note-off.
    used: [[u64; 128]; CHANNELS],
    order: u64,
    fade: u32,
    fade_frames: u32,
    pending: [Option<(EventId, u8, u8)>; PENDING],
    /// Notes ended early to make room, and note-ons lost during a full fade.
    pub stolen: u64,
    pub dropped: u64,
}

impl fmt::Debug for Player {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Player")
            .field("preset", &self.preset)
            .field("held", &self.held_count())
            .finish_non_exhaustive()
    }
}

fn wire(channel: u8) -> i32 {
    let channel = usize::from(channel);
    (if channel >= PERCUSSION {
        channel + 1
    } else {
        channel
    }) as i32
}

impl Player {
    pub(super) fn new(
        sound: Arc<SoundFont>,
        preset: Preset,
        rate: u32,
        gain: f32,
    ) -> Result<Self, InstrumentError> {
        if !(16_000..=192_000).contains(&rate) {
            return Err(InstrumentError::SampleRate(rate));
        }
        if !(gain.is_finite() && (0.0..=1.0).contains(&gain)) {
            return Err(InstrumentError::InvalidGain);
        }
        let mut settings = SynthesizerSettings::new(rate as i32);
        settings.block_size = BLOCK;
        settings.maximum_polyphony = POLYPHONY;
        settings.enable_reverb_and_chorus = false;
        let mut synth = Synthesizer::new(&sound, &settings)
            .map_err(|error| InstrumentError::Parse(error.to_string()))?;
        synth.set_master_volume(gain);
        let mut player = Self {
            synth,
            preset,
            rate,
            held: [None; HELD_CAPACITY],
            used: [[0; 128]; CHANNELS],
            order: 0,
            fade: 0,
            fade_frames: 0,
            pending: [None; PENDING],
            stolen: 0,
            dropped: 0,
        };
        player.program();
        Ok(player)
    }

    fn program(&mut self) {
        let (bank, program) = (i32::from(self.preset.bank), i32::from(self.preset.program));
        for channel in 0..CHANNELS as u8 {
            self.synth
                .process_midi_message(wire(channel), 0xB0, 0x00, bank);
            self.synth
                .process_midi_message(wire(channel), 0xC0, program, 0);
        }
    }

    pub fn preset(&self) -> Preset {
        self.preset
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Brownnote notes currently held (not their release tails).
    pub fn held_count(&self) -> usize {
        self.held.iter().flatten().count()
    }

    fn end(&mut self, index: usize) {
        if let Some(held) = self.held[index].take() {
            self.synth.note_off(wire(held.channel), i32::from(held.key));
            self.order += 1;
            self.used[usize::from(held.channel)][usize::from(held.key)] = self.order;
        }
    }

    fn oldest(&self, matches: impl Fn(&Held) -> bool) -> Option<usize> {
        self.held
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.filter(|h| matches(h)).map(|h| (h.order, i)))
            .min()
            .map(|(_, i)| i)
    }

    /// Start a note under its own identity. Velocity 0 is not a note-off here.
    pub fn note_on(&mut self, id: EventId, key: u8, velocity: u8) {
        let (key, velocity) = (key.min(127), velocity.clamp(1, 127));
        if self.fade > 0 {
            match self.pending.iter_mut().find(|p| p.is_none()) {
                Some(slot) => *slot = Some((id, key, velocity)),
                None => self.dropped += 1,
            }
            return;
        }
        let mut busy = [false; CHANNELS];
        for held in self.held.iter().flatten().filter(|h| h.key == key) {
            busy[usize::from(held.channel)] = true;
        }
        if busy.iter().all(|b| *b)
            && let Some(oldest) = self.oldest(|h| h.key == key)
        {
            busy[usize::from(self.held[oldest].map_or(0, |h| h.channel))] = false;
            self.end(oldest);
            self.stolen += 1;
        }
        let slot = match self.held.iter().position(Option::is_none) {
            Some(slot) => slot,
            None => {
                let oldest = self.oldest(|_| true).unwrap_or(0);
                if let Some(held) = self.held[oldest]
                    && held.key == key
                {
                    busy[usize::from(held.channel)] = false;
                }
                self.end(oldest);
                self.stolen += 1;
                oldest
            }
        };
        let channel = (0..CHANNELS)
            .filter(|c| !busy[*c])
            .min_by_key(|c| self.used[*c][usize::from(key)])
            .unwrap_or(0) as u8;
        self.synth
            .note_on(wire(channel), i32::from(key), i32::from(velocity));
        self.order += 1;
        self.used[usize::from(channel)][usize::from(key)] = self.order;
        self.held[slot] = Some(Held {
            id,
            channel,
            key,
            order: self.order,
        });
    }

    /// Release the oldest held note with this ID (an ID replayed on every loop
    /// cycle can be held more than once). Unknown IDs are ignored.
    pub fn note_off(&mut self, id: EventId) {
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|p| p.is_some_and(|(pending, ..)| pending == id))
        {
            *pending = None;
            return;
        }
        if let Some(index) = self.oldest(|h| h.id == id) {
            self.end(index);
        }
    }

    /// Fade everything out over `frames`, then reset the backend. Held IDs are
    /// forgotten now, so late note-offs for them do nothing.
    pub fn panic(&mut self, frames: u32) {
        self.held = [None; HELD_CAPACITY];
        self.pending = [None; PENDING];
        if frames == 0 {
            self.silence();
        } else if self.fade == 0 {
            self.fade = frames;
            self.fade_frames = frames;
        } else {
            self.fade = self.fade.min(frames);
        }
    }

    fn silence(&mut self) {
        self.fade = 0;
        // reset() discards the cached block too, so nothing old is heard after.
        self.synth.reset();
        self.program();
        self.held = [None; HELD_CAPACITY];
        let pending = std::mem::replace(&mut self.pending, [None; PENDING]);
        for (id, key, velocity) in pending.into_iter().flatten() {
            self.note_on(id, key, velocity);
        }
    }

    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mut at = 0;
        while at < frames {
            if self.fade == 0 {
                self.synth
                    .render(&mut left[at..frames], &mut right[at..frames]);
                return;
            }
            let n = (self.fade as usize).min(frames - at);
            self.synth
                .render(&mut left[at..at + n], &mut right[at..at + n]);
            for i in 0..n {
                let gain = (self.fade - i as u32) as f32 / self.fade_frames as f32;
                left[at + i] *= gain;
                right[at + i] *= gain;
            }
            self.fade -= n as u32;
            at += n;
            if self.fade == 0 {
                self.silence();
            }
        }
    }
}
