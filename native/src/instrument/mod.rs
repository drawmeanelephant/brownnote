//! One sampled instrument (D18, #108): an SF2 preset played by the pinned
//! RustySynth 1.3.8 backend (MIT, pure Rust).
//!
//! Brownnote keeps its own note identities. Every held note is addressed by
//! its `EventId`, never by MIDI channel and pitch: the backend's note-off
//! releases *every* voice of a channel/key, which would break D14. The
//! [`Player`] therefore gives each sounding copy of a pitch its own melodic
//! backend channel, so same-pitch overlaps end oldest-first by ID.
//!
//! Preparation (bounded parse, preset validation, voice allocation) runs off
//! the audio callback. The callback-side [`Player`] never allocates or frees.
//! Retained renders are per note, at [`RENDER_RATE`], with the backend's
//! reverb and chorus disabled so the same notes always produce the same
//! samples.
mod document;
mod sf2;

pub use document::{DocumentError, FORMAT_VERSION, InstrumentDocument, RenderDocument, describe};
pub use sf2::{BLOCK, CHANNELS, HELD_CAPACITY, POLYPHONY, Player};

use crate::performance::{EffectiveNote, EventId, InstrumentId, PerformanceError, TakeId, Tempo};
use rustysynth::SoundFont;
use std::{
    fmt,
    io::Cursor,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

/// The backend this build sounds instruments with. Saved for provenance only;
/// reopening never trusts it to mean the same samples on another build.
pub const BACKEND: &str = "rustysynth 1.3.8";
/// Source files above this size are refused before parsing.
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
/// Retained per-note renders: stereo float32 at this rate.
pub const RENDER_RATE: u32 = 48_000;
/// A held note longer than this is not frozen (about 2.5 minutes).
pub const MAX_GATE_FRAMES: u64 = RENDER_RATE as u64 * 150;
/// Release tails are cut here even if the preset never decays to silence.
pub const MAX_TAIL_FRAMES: u64 = RENDER_RATE as u64 * 20;
/// The single instrument this build mounts. MIDI mappings target it by ID.
pub const INSTRUMENT: InstrumentId = InstrumentId(1);
pub const DEFAULT_GAIN: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Preset {
    /// Melodic banks only (0..=127); percussion banks are not offered.
    pub bank: u8,
    pub program: u8,
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:03}:{:03}", self.bank, self.program)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstrumentError {
    TooLarge(u64),
    NotSoundFont,
    Truncated,
    Parse(String),
    NoMelodicPreset,
    PresetMissing(Preset),
    NoSampleData,
    SampleRate(u32),
    InvalidGain,
    GateTooLong(u64),
    TailTooLong,
    Encode(String),
}

impl fmt::Display for InstrumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge(bytes) => write!(
                f,
                "instrument file is {bytes} bytes; the limit is {MAX_SOURCE_BYTES}"
            ),
            Self::NotSoundFont => write!(f, "not an SF2 SoundFont (RIFF sfbk)"),
            Self::Truncated => write!(f, "SF2 chunk sizes exceed the file"),
            Self::Parse(reason) => write!(f, "SF2 could not be read: {reason}"),
            Self::NoMelodicPreset => write!(f, "SF2 has no melodic preset (bank 0-127)"),
            Self::PresetMissing(preset) => write!(f, "SF2 has no preset {preset}"),
            Self::NoSampleData => write!(f, "SF2 has no sample data"),
            Self::SampleRate(rate) => write!(f, "{rate} Hz is outside 16-192 kHz"),
            Self::InvalidGain => write!(f, "instrument gain must be 0..=1"),
            Self::GateTooLong(frames) => write!(f, "note of {frames} frames is too long to freeze"),
            Self::TailTooLong => write!(f, "release did not decay within the tail limit"),
            Self::Encode(reason) => write!(f, "render could not be encoded: {reason}"),
        }
    }
}
impl std::error::Error for InstrumentError {}

/// A parsed, validated SF2 plus the one preset it plays. Cheap to clone.
#[derive(Clone)]
pub struct Font {
    sound: Arc<SoundFont>,
    preset: Preset,
    name: String,
    presets: Arc<[(Preset, String)]>,
}

impl fmt::Debug for Font {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Font")
            .field("preset", &self.preset)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Font {
    /// Parse exact source bytes. `None` selects the lowest melodic preset.
    /// A requested preset that the file lacks is an error: the backend's own
    /// silent fallback to a default preset is never accepted.
    pub fn prepare(bytes: &[u8], preset: Option<Preset>) -> Result<Self, InstrumentError> {
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            return Err(InstrumentError::TooLarge(bytes.len() as u64));
        }
        check_chunks(bytes)?;
        let sound = catch_unwind(AssertUnwindSafe(|| SoundFont::new(&mut Cursor::new(bytes))))
            .map_err(|_| InstrumentError::Parse("backend panicked".into()))?
            .map_err(|error| InstrumentError::Parse(error.to_string()))?;
        if sound.get_wave_data().is_empty() {
            return Err(InstrumentError::NoSampleData);
        }
        let mut presets: Vec<(Preset, String)> = sound
            .get_presets()
            .iter()
            .filter_map(|p| {
                let bank = u8::try_from(p.get_bank_number())
                    .ok()
                    .filter(|b| *b < 128)?;
                let program = u8::try_from(p.get_patch_number())
                    .ok()
                    .filter(|p| *p < 128)?;
                Some((Preset { bank, program }, p.get_name().trim().to_string()))
            })
            .collect();
        presets.sort();
        presets.dedup_by_key(|(preset, _)| *preset);
        let chosen = match preset {
            Some(preset) => presets
                .iter()
                .find(|(p, _)| *p == preset)
                .ok_or(InstrumentError::PresetMissing(preset))?,
            None => presets.first().ok_or(InstrumentError::NoMelodicPreset)?,
        };
        Ok(Self {
            preset: chosen.0,
            name: chosen.1.clone(),
            sound: Arc::new(sound),
            presets: presets.into(),
        })
    }

    pub fn preset(&self) -> Preset {
        self.preset
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn presets(&self) -> &[(Preset, String)] {
        &self.presets
    }

    /// Build a callback player off the callback. All voices are allocated here.
    pub fn player(&self, rate: u32, gain: f32) -> Result<Box<Player>, InstrumentError> {
        Player::new(self.sound.clone(), self.preset, rate, gain).map(Box::new)
    }

    /// Freeze one note: onset at frame zero, `gate_frames` held, then the
    /// release tail until the backend is silent. Stereo float32 WAV bytes.
    pub fn render(&self, key: RenderKey, gain: f32) -> Result<Vec<u8>, InstrumentError> {
        if key.gate_frames > MAX_GATE_FRAMES {
            return Err(InstrumentError::GateTooLong(key.gate_frames));
        }
        const CHUNK: usize = 1024;
        let mut player = self.player(RENDER_RATE, gain)?;
        let id = EventId {
            take: TakeId(0),
            sequence: 0,
        };
        let (mut left, mut right) = (Vec::new(), Vec::new());
        let (mut l, mut r) = ([0.0_f32; CHUNK], [0.0_f32; CHUNK]);
        player.note_on(id, key.pitch, key.velocity);
        let mut remaining = key.gate_frames;
        while remaining > 0 {
            let n = remaining.min(CHUNK as u64) as usize;
            player.render(&mut l[..n], &mut r[..n]);
            left.extend_from_slice(&l[..n]);
            right.extend_from_slice(&r[..n]);
            remaining -= n as u64;
        }
        player.note_off(id);
        // Two exactly-silent chunks after release: the backend has no voices left.
        let (mut silent, mut tail) = (0, 0_u64);
        while silent < 2 {
            if tail >= MAX_TAIL_FRAMES {
                return Err(InstrumentError::TailTooLong);
            }
            player.render(&mut l, &mut r);
            left.extend_from_slice(&l);
            right.extend_from_slice(&r);
            let quiet = l.iter().chain(&r).all(|v| *v == 0.0);
            silent = if quiet { silent + 1 } else { 0 };
            tail += CHUNK as u64;
        }
        let last = left
            .iter()
            .zip(&right)
            .rposition(|(l, r)| *l != 0.0 || *r != 0.0)
            .map_or(1, |i| i + 1);
        left.truncate(last);
        right.truncate(last);
        encode(&left, &right)
    }
}

/// The frozen sound of one effective note. Same key, same samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RenderKey {
    pub pitch: u8,
    pub velocity: u8,
    /// Note-on to note-off at [`RENDER_RATE`], from the effective (edited) view.
    pub gate_frames: u64,
}

impl RenderKey {
    pub fn of(note: &EffectiveNote, tempo: Tempo) -> Result<Self, PerformanceError> {
        let start = note.start.at_tempo_nanos(tempo)?;
        let end = note.end.at_tempo_nanos(tempo)?;
        let nanos = u128::from(end.saturating_sub(start));
        let frames = (nanos * u128::from(RENDER_RATE) + 500_000_000) / 1_000_000_000;
        Ok(Self {
            pitch: note.key.pitch.value(),
            velocity: note.velocity.value(),
            gate_frames: u64::try_from(frames).map_err(|_| PerformanceError::Overflow)?,
        })
    }
}

fn encode(left: &[f32], right: &[f32]) -> Result<Vec<u8>, InstrumentError> {
    let error = |e: hound::Error| InstrumentError::Encode(e.to_string());
    if left.iter().chain(right).any(|v| !v.is_finite()) {
        return Err(InstrumentError::Encode("non-finite sample".into()));
    }
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
        hound::WavSpec {
            channels: 2,
            sample_rate: RENDER_RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .map_err(error)?;
    for (l, r) in left.iter().zip(right) {
        writer.write_sample(*l).map_err(error)?;
        writer.write_sample(*r).map_err(error)?;
    }
    writer.finalize().map_err(error)?;
    Ok(bytes.into_inner())
}

fn le32(bytes: &[u8], at: usize) -> Result<usize, InstrumentError> {
    let word = bytes
        .get(at..at + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .ok_or(InstrumentError::Truncated)?;
    usize::try_from(u32::from_le_bytes(word)).map_err(|_| InstrumentError::Truncated)
}

/// Bound every chunk the backend will size an allocation from by the actual
/// file length, so a lying header cannot request more memory than it brings.
fn check_chunks(bytes: &[u8]) -> Result<(), InstrumentError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"sfbk" {
        return Err(InstrumentError::NotSoundFont);
    }
    let end = le32(bytes, 4)?
        .checked_add(8)
        .filter(|end| *end <= bytes.len())
        .ok_or(InstrumentError::Truncated)?;
    let mut at = 12;
    while at + 8 <= end {
        let size = le32(bytes, at + 4)?;
        let body = at + 8;
        let stop = body
            .checked_add(size)
            .filter(|stop| *stop <= end)
            .ok_or(InstrumentError::Truncated)?;
        if &bytes[at..at + 4] == b"LIST" {
            if size < 4 {
                return Err(InstrumentError::Truncated);
            }
            let mut sub = body + 4;
            while sub + 8 <= stop {
                let size = le32(bytes, sub + 4)?;
                sub = (sub + 8)
                    .checked_add(size)
                    .filter(|next| *next <= stop)
                    .ok_or(InstrumentError::Truncated)?;
            }
        }
        at = stop;
    }
    Ok(())
}
