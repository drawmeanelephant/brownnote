//! Event/time contract and additive bounded engine scheduling.
//! Capture/lifecycle work runs off the callback; originals are never quantized.
use crate::project::{AssetId, PadId};
pub mod scheduler;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    num::NonZeroU32,
};

const NANOS_PER_SECOND: u128 = 1_000_000_000;
// Original nanoseconds * integer milli-BPM gives this many units per quarter.
const UNITS_PER_QUARTER: u128 = 60_000_000_000_000;

macro_rules! identity {
    ($($name:ident),+ $(,)?) => {$(
        /// Project-stable identity, assigned by the owning registry, not a slot.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u128);
    )+};
}
identity!(TakeId, SourceId, InstrumentId);

/// Sequence is unique within its take. Reopen continues above every saved ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventId {
    pub take: TakeId,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Pad(PadId),
    Sample(AssetId),
    Instrument(InstrumentId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerformanceError {
    ZeroTempo,
    ZeroSampleRate,
    Overflow,
    InvalidChannel,
    InvalidPitch,
    InvalidVelocity,
    InvalidLoopBars,
    WrongTake,
    DuplicateEvent(EventId),
    UnmatchedNoteOff(NoteKey),
    EndBeforeStart,
    TempoLocked,
    AlreadyArmed,
    NotArmed,
    UnknownTake,
    StaleEpoch,
    BeforeTake,
    BeforeStream,
}
impl fmt::Display for PerformanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "performance: {self:?}")
    }
}
impl std::error::Error for PerformanceError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tempo(NonZeroU32);
impl TryFrom<u32> for Tempo {
    type Error = PerformanceError;
    fn try_from(milli_bpm: u32) -> Result<Self, Self::Error> {
        NonZeroU32::new(milli_bpm)
            .map(Self)
            .ok_or(PerformanceError::ZeroTempo)
    }
}
impl Tempo {
    pub fn milli_bpm(self) -> u32 {
        self.0.get()
    }
}
impl Default for Tempo {
    fn default() -> Self {
        Self(NonZeroU32::new(116_000).unwrap_or(NonZeroU32::MIN))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRate(NonZeroU32);
impl TryFrom<u32> for SampleRate {
    type Error = PerformanceError;
    fn try_from(rate: u32) -> Result<Self, Self::Error> {
        NonZeroU32::new(rate)
            .map(Self)
            .ok_or(PerformanceError::ZeroSampleRate)
    }
}
impl SampleRate {
    pub fn hz(self) -> u32 {
        self.0.get()
    }
}

/// Immutable integer nanoseconds relative to take start, not device frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct OriginalTime(u64);
impl OriginalTime {
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }
    pub fn nanos(self) -> u64 {
        self.0
    }
    pub fn from_frames(frames: u64, rate: SampleRate) -> Result<Self, PerformanceError> {
        rounded_u64(u128::from(frames) * NANOS_PER_SECOND, u128::from(rate.hz())).map(Self)
    }
}

macro_rules! midi_value {
    ($name:ident, $min:literal, $max:literal, $error:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(u8);
        impl TryFrom<u8> for $name {
            type Error = PerformanceError;
            fn try_from(value: u8) -> Result<Self, Self::Error> {
                if ($min..=$max).contains(&value) {
                    Ok(Self(value))
                } else {
                    Err(PerformanceError::$error)
                }
            }
        }
        impl $name {
            pub fn value(self) -> u8 {
                self.0
            }
        }
    };
}
// MIDI wire channels are zero-based. Normalize note-on velocity zero to off.
midi_value!(Channel, 0, 15, InvalidChannel);
midi_value!(Pitch, 0, 127, InvalidPitch);
midi_value!(Velocity, 1, 127, InvalidVelocity);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NoteKey {
    /// Logical source identity, never the current MIDI port enumeration index.
    pub source: SourceId,
    pub channel: Channel,
    pub pitch: Pitch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteOn {
    pub id: EventId,
    pub target: Target,
    pub key: NoteKey,
    pub velocity: Velocity,
    pub original_time: OriginalTime,
}

/// A completed original note. There are deliberately no timing setters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    on: NoteOn,
    original_end: OriginalTime,
}
impl Note {
    pub fn on(&self) -> &NoteOn {
        &self.on
    }
    pub fn original_end(&self) -> OriginalTime {
        self.original_end
    }
    pub fn duration_nanos(&self) -> u64 {
        self.original_end.0 - self.on.original_time.0
    }
}

/// Producer-side lifecycle matching, not callback code. Input order is FIFO;
/// source/channel/pitch isolation prevents one device ending another's note.
#[derive(Default)]
pub struct NoteLifecycle {
    active: BTreeMap<NoteKey, VecDeque<NoteOn>>,
    used: BTreeSet<EventId>,
}
impl NoteLifecycle {
    pub fn note_on(&mut self, on: NoteOn) -> Result<(), PerformanceError> {
        if !self.used.insert(on.id) {
            return Err(PerformanceError::DuplicateEvent(on.id));
        }
        self.active.entry(on.key).or_default().push_back(on);
        Ok(())
    }

    pub fn note_off(&mut self, key: NoteKey, time: OriginalTime) -> Result<Note, PerformanceError> {
        let unmatched = PerformanceError::UnmatchedNoteOff(key);
        let queue = self.active.get_mut(&key).ok_or(unmatched)?;
        if time < queue.front().ok_or(unmatched)?.original_time {
            return Err(PerformanceError::EndBeforeStart);
        }
        let on = queue.pop_front().ok_or(unmatched)?;
        if queue.is_empty() {
            self.active.remove(&key);
        }
        Ok(Note {
            on,
            original_end: time,
        })
    }

    pub fn active_count(&self) -> usize {
        self.active.values().map(VecDeque::len).sum()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quantization {
    #[default]
    Off,
    Sixteenth,
}

/// Exact derived musical position. Not MIDI PPQ and not a stored original.
/// One quarter is 60_000_000_000_000 units, allowing exact ns * milli-BPM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MusicalPosition(u128);
impl MusicalPosition {
    pub fn from_original(time: OriginalTime, recording_tempo: Tempo) -> Self {
        Self(u128::from(time.0) * u128::from(recording_tempo.milli_bpm()))
    }
    pub fn at_tempo_nanos(self, tempo: Tempo) -> Result<u64, PerformanceError> {
        rounded_u64(self.0, u128::from(tempo.milli_bpm()))
    }
    pub fn at_tempo_frames(self, tempo: Tempo, rate: SampleRate) -> Result<u64, PerformanceError> {
        let numerator = self
            .0
            .checked_mul(u128::from(rate.hz()))
            .ok_or(PerformanceError::Overflow)?;
        rounded_u64(numerator, u128::from(tempo.milli_bpm()) * NANOS_PER_SECOND)
    }
    /// Convert against an observed stream/take anchor with only one rounding.
    pub fn at_tempo_stream_frame(
        self,
        tempo: Tempo,
        rate: SampleRate,
        take_time: OriginalTime,
        stream_frame: u64,
    ) -> Result<u64, PerformanceError> {
        let units = i128::try_from(self.0).map_err(|_| PerformanceError::Overflow)?;
        let tempo = i128::from(tempo.milli_bpm());
        let denominator = tempo * NANOS_PER_SECOND as i128;
        let delta = units - i128::from(take_time.0) * tempo;
        let numerator = delta
            .checked_mul(i128::from(rate.hz()))
            .and_then(|n| n.checked_add(i128::from(stream_frame) * denominator))
            .ok_or(PerformanceError::Overflow)?;
        rounded_stream_frame(numerator, denominator)
    }
    pub fn split_loop(self, bars: LoopBars) -> Result<LoopPosition, PerformanceError> {
        let length = bars.units();
        Ok(LoopPosition {
            iteration: u64::try_from(self.0 / length).map_err(|_| PerformanceError::Overflow)?,
            within: Self(self.0 % length),
        })
    }
    /// Shortening hides originals past the new end; it never wraps/deletes them.
    pub fn visible_in(self, bars: LoopBars) -> bool {
        self.0 < bars.units()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopBars(u8);
impl TryFrom<u8> for LoopBars {
    type Error = PerformanceError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if matches!(value, 1 | 2 | 4 | 8) {
            Ok(Self(value))
        } else {
            Err(PerformanceError::InvalidLoopBars)
        }
    }
}
impl LoopBars {
    fn units(self) -> u128 {
        u128::from(self.0) * 4 * UNITS_PER_QUARTER
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopPosition {
    pub iteration: u64,
    pub within: MusicalPosition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveNote {
    pub id: EventId,
    pub target: Target,
    pub key: NoteKey,
    pub velocity: Velocity,
    pub start: MusicalPosition,
    pub end: MusicalPosition,
}

pub struct TakeTiming {
    id: TakeId,
    recording_tempo: Tempo,
    quantization: Quantization,
}
impl TakeTiming {
    pub fn new(id: TakeId, recording_tempo: Tempo) -> Self {
        Self {
            id,
            recording_tempo,
            quantization: Quantization::Off,
        }
    }
    pub fn recording_tempo(&self) -> Tempo {
        self.recording_tempo
    }
    pub fn quantization(&self) -> Quantization {
        self.quantization
    }
    pub fn set_quantization(&mut self, quantization: Quantization) {
        self.quantization = quantization;
    }
    pub fn view(&self, note: &Note) -> Result<EffectiveNote, PerformanceError> {
        if note.on.id.take != self.id {
            return Err(PerformanceError::WrongTake);
        }
        let original = MusicalPosition::from_original(note.on.original_time, self.recording_tempo);
        let end = MusicalPosition::from_original(note.original_end, self.recording_tempo);
        let start = match self.quantization {
            Quantization::Off => original,
            Quantization::Sixteenth => {
                let grid = UNITS_PER_QUARTER / 4;
                MusicalPosition(round_ratio(original.0, grid) * grid)
            }
        };
        // Move the entire note. Preserve its musical duration, even for flams
        // or very short gates, rather than separately snapping its note-off.
        Ok(EffectiveNote {
            id: note.on.id,
            target: note.on.target,
            key: note.on.key,
            velocity: note.on.velocity,
            start,
            end: MusicalPosition(start.0 + (end.0 - original.0)),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakePhase {
    Armed,
    Recording,
}

/// Locks tempo while *any* take is armed or recording, including overlapping
/// arms. This is a pure ownership guard, not a recorder/transport implementation.
#[derive(Default)]
pub struct TempoControl {
    tempo: Tempo,
    active: BTreeMap<TakeId, TakePhase>,
}
impl TempoControl {
    pub fn tempo(&self) -> Tempo {
        self.tempo
    }
    pub fn set_tempo(&mut self, tempo: Tempo) -> Result<(), PerformanceError> {
        if !self.active.is_empty() {
            return Err(PerformanceError::TempoLocked);
        }
        self.tempo = tempo;
        Ok(())
    }
    pub fn arm(&mut self, take: TakeId) -> Result<Tempo, PerformanceError> {
        if self.active.contains_key(&take) {
            return Err(PerformanceError::AlreadyArmed);
        }
        self.active.insert(take, TakePhase::Armed);
        Ok(self.tempo)
    }
    pub fn record(&mut self, take: TakeId) -> Result<(), PerformanceError> {
        let phase = self
            .active
            .get_mut(&take)
            .ok_or(PerformanceError::NotArmed)?;
        if *phase != TakePhase::Armed {
            return Err(PerformanceError::NotArmed);
        }
        *phase = TakePhase::Recording;
        Ok(())
    }
    pub fn finish(&mut self, take: TakeId) -> Result<(), PerformanceError> {
        self.active
            .remove(&take)
            .map(|_| ())
            .ok_or(PerformanceError::UnknownTake)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamEpoch(pub u64);

/// A monotonic host-clock/take-clock anchor, obtained off callback using #101's
/// timestamp seam. A reopened stream must use a new epoch and fresh anchor.
pub struct ClockAnchor {
    pub epoch: StreamEpoch,
    pub host_nanos: u64,
    pub take_time: OriginalTime,
}
impl ClockAnchor {
    pub fn original_time(
        &self,
        epoch: StreamEpoch,
        host_nanos: u64,
    ) -> Result<OriginalTime, PerformanceError> {
        if epoch != self.epoch {
            return Err(PerformanceError::StaleEpoch);
        }
        let time =
            i128::from(self.take_time.0) + i128::from(host_nanos) - i128::from(self.host_nanos);
        if time < 0 {
            return Err(PerformanceError::BeforeTake);
        }
        u64::try_from(time)
            .map(OriginalTime)
            .map_err(|_| PerformanceError::Overflow)
    }
}

// These are admission/placement laws only. No speculative scheduler framework.
pub const SCHEDULE_CAPACITY: usize = 256;
pub const LOOKAHEAD_SECONDS: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    Late { frames: u64 },
    TooFuture,
    Full,
}

pub fn admission(event: u64, now: u64, queued: usize, rate: SampleRate) -> Admission {
    if queued >= SCHEDULE_CAPACITY {
        Admission::Full
    } else if event < now {
        Admission::Late {
            frames: now - event,
        }
    } else if event - now > u64::from(rate.hz()) * LOOKAHEAD_SECONDS {
        Admission::TooFuture
    } else {
        Admission::Accepted
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockPlacement {
    Late { frames: u64 },
    Offset(u32),
    Future,
}

/// Blocks are half-open. An event at the end belongs to the next block.
pub fn block_placement(event: u64, start: u64, frames: NonZeroU32) -> BlockPlacement {
    if event < start {
        BlockPlacement::Late {
            frames: start - event,
        }
    } else if event - start < u64::from(frames.get()) {
        BlockPlacement::Offset((event - start) as u32)
    } else {
        BlockPlacement::Future
    }
}

// Nearest integer; exact ties go later. Division avoids overflow from n + d/2.
fn round_ratio(n: u128, d: u128) -> u128 {
    n / d + u128::from(n % d >= d - d / 2)
}
fn rounded_u64(n: u128, d: u128) -> Result<u64, PerformanceError> {
    u64::try_from(round_ratio(n, d)).map_err(|_| PerformanceError::Overflow)
}

pub(crate) fn rounded_stream_frame(n: i128, d: i128) -> Result<u64, PerformanceError> {
    if n < 0 {
        return Err(PerformanceError::BeforeStream);
    }
    rounded_u64(n as u128, d as u128)
}
