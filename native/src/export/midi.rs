//! SMF format 1, 960 PPQ, one constant-tempo conductor plus instrument tracks.
//!
//! Wire channels are zero-based 0..15, preserved without reserving channel 10
//! or remapping pads to drums. Only InstrumentNotes parts enter the SMF.
//! LiveInstrumentNotes parts stay out on purpose: a plain SMF track cannot
//! carry the plugin identity, so writing one would tell a destination to sound
//! those notes with whatever instrument it has, which is a claim the bundle
//! cannot make. Their notes live in the manifest; their take's retained audio
//! is the audible fallback.
//! Onsets and absolute ends round independently to nearest tick, ties later.
//! Thus onset/end error is <= half a tick and duration error <= one tick.
//! Gates rounding to zero get one tick (also <= one tick duration error).
//! Tick conversion uses the *encoded* integer microseconds/quarter, avoiding
//! accumulating audio/MIDI drift when milli-BPM cannot be encoded exactly.
//!
//! FIFO-compatible same-channel/pitch overlaps remain on the same track (D14).
//! Nested/non-FIFO gates, possible after edits or across sources, get another
//! track with the original channel, not shortened notes. Each note-on has a
//! `bn:<take>:<sequence>` text marker and a manifest track/event join.

use super::manifest::{
    EventEntry, EventId, LossRecord, Manifest, MidiEntry, MidiTrack, PartId, PartKind,
};
use std::{collections::BTreeMap, fmt, path::PathBuf};

pub const PPQ: u16 = 960;
pub const MIDI_PATH: &str = "notes.mid";
const MAX_DELTA: u64 = 0x0fff_ffff;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MidiError {
    Tempo,
    InvalidNote(EventId),
    Overflow,
}
impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "export MIDI: {self:?}")
    }
}
impl std::error::Error for MidiError {}

#[derive(Clone, Copy)]
struct Note<'a> {
    event: &'a EventEntry,
    start: u64,
    end: u64,
    channel: u8,
    pitch: u8,
    velocity: u8,
}

fn tick(nanos: u64, micros: u64) -> Result<u64, MidiError> {
    let n = u128::from(nanos) * u128::from(PPQ);
    let d = u128::from(micros) * 1_000;
    u64::try_from((n + d / 2) / d).map_err(|_| MidiError::Overflow)
}

fn vlq(value: u64, out: &mut Vec<u8>) -> Result<(), MidiError> {
    if value > MAX_DELTA {
        return Err(MidiError::Overflow);
    }
    let mut bytes = [0_u8; 4];
    let mut index = 3;
    bytes[index] = (value & 0x7f) as u8;
    let mut rest = value >> 7;
    while rest != 0 {
        index -= 1;
        bytes[index] = ((rest & 0x7f) as u8) | 0x80;
        rest >>= 7;
    }
    out.extend_from_slice(&bytes[index..]);
    Ok(())
}

fn meta(out: &mut Vec<u8>, kind: u8, data: &[u8]) -> Result<(), MidiError> {
    out.extend_from_slice(&[0xff, kind]);
    vlq(data.len() as u64, out)?;
    out.extend_from_slice(data);
    Ok(())
}

fn chunk(out: &mut Vec<u8>, body: &[u8]) -> Result<(), MidiError> {
    let len = u32::try_from(body.len()).map_err(|_| MidiError::Overflow)?;
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    Ok(())
}

pub struct MidiOutput {
    pub bytes: Vec<u8>,
    pub entry: MidiEntry,
    pub losses: Vec<LossRecord>,
}

/// Encode without filesystem side effects. All bytes publish with the bundle.
pub fn encode(manifest: &Manifest) -> Result<MidiOutput, MidiError> {
    let bpm = u64::from(manifest.timing.tempo_milli_bpm);
    if bpm == 0 {
        return Err(MidiError::Tempo);
    }
    let micros = (60_000_000_000 + bpm / 2) / bpm;
    if !(1..=0xff_ffff).contains(&micros) {
        return Err(MidiError::Tempo);
    }
    let mut conductor = vec![0];
    let tempo = (micros as u32).to_be_bytes();
    meta(&mut conductor, 0x51, &tempo[1..])?;
    conductor.push(0);
    meta(&mut conductor, 0x58, &[4, 2, 24, 8])?;
    conductor.extend_from_slice(&[0, 0xff, 0x2f, 0]);
    let mut tracks = vec![conductor];
    let mut joins = Vec::new();
    let mut losses = Vec::new();
    for part in &manifest.parts {
        if !matches!(part.kind, PartKind::InstrumentNotes { .. }) {
            continue;
        }
        let mut notes = Vec::new();
        for event in manifest.events.iter().filter(|e| e.part == part.id) {
            if event.pad.is_some() {
                return Err(MidiError::InvalidNote(event.id));
            }
            let (Some(channel @ 0..=15), Some(pitch @ 0..=127), Some(velocity @ 1..=127)) =
                (event.channel, event.pitch, event.velocity)
            else {
                return Err(MidiError::InvalidNote(event.id));
            };
            let end_nanos = event
                .start_nanos
                .checked_add(event.duration_nanos)
                .ok_or(MidiError::Overflow)?;
            let start = tick(event.start_nanos, micros)?;
            let rounded_end = tick(end_nanos, micros)?;
            let end = rounded_end.max(start.checked_add(1).ok_or(MidiError::Overflow)?);
            notes.push(Note {
                event,
                start,
                end,
                channel,
                pitch,
                velocity,
            });
            if u128::from(micros) * u128::from(bpm) != 60_000_000_000
                || u128::from(event.start_nanos) * u128::from(PPQ)
                    != u128::from(start) * u128::from(micros) * 1_000
                || u128::from(end_nanos) * u128::from(PPQ)
                    != u128::from(end) * u128::from(micros) * 1_000
            {
                losses.push(LossRecord::ConversionLoss {
                    event: event.id,
                    detail: format!(
                        "SMF 960 PPQ, tempo {micros} us/quarter; nearest absolute ticks \
                         (ties later), minimum gate 1 tick; onset/end <= 0.5 tick \
                         except minimum gates, duration <= 1 tick"
                    ),
                });
            }
        }
        notes.sort_by_key(|n| (n.start, n.event.id));
        let mut lanes: Vec<Vec<Note<'_>>> = Vec::new();
        let mut ends: Vec<BTreeMap<(u8, u8), u64>> = Vec::new();
        for note in notes {
            let key = (note.channel, note.pitch);
            let lane = ends
                .iter()
                .position(|end| {
                    end.get(&key)
                        .is_none_or(|last| note.start >= *last || note.end >= *last)
                })
                .unwrap_or(lanes.len());
            if lane == lanes.len() {
                lanes.push(Vec::new());
                ends.push(BTreeMap::new());
            }
            if lane > 0 {
                losses.push(LossRecord::ConversionLoss {
                    event: note.event.id,
                    detail: "non-FIFO same-pitch gate isolated on another SMF track; channel and duration preserved".into(),
                });
            }
            ends[lane].insert(key, note.end);
            lanes[lane].push(note);
        }
        for lane in lanes {
            let index = u16::try_from(tracks.len()).map_err(|_| MidiError::Overflow)?;
            tracks.push(note_track(part.id, &lane)?);
            joins.push(MidiTrack {
                index,
                part: part.id,
                events: lane.iter().map(|n| n.event.id).collect(),
            });
        }
    }
    let count = u16::try_from(tracks.len()).map_err(|_| MidiError::Overflow)?;
    let mut bytes = b"MThd".to_vec();
    bytes.extend_from_slice(&6_u32.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&count.to_be_bytes());
    bytes.extend_from_slice(&PPQ.to_be_bytes());
    for track in tracks {
        chunk(&mut bytes, &track)?;
    }
    Ok(MidiOutput {
        bytes,
        entry: MidiEntry {
            path: PathBuf::from(MIDI_PATH),
            ppq: PPQ,
            tracks: joins,
        },
        losses,
    })
}

fn note_track(part: PartId, notes: &[Note<'_>]) -> Result<Vec<u8>, MidiError> {
    let mut out = vec![0];
    meta(&mut out, 3, format!("Brownnote part {}", part.0).as_bytes())?;
    // End before start at shared boundaries; event ID orders simultaneous ons
    // and simultaneous offs identically, preserving FIFO even after rounding.
    let mut actions = Vec::new();
    for note in notes {
        actions.push((note.start, 1_u8, note.event.id, note));
        actions.push((note.end, 0_u8, note.event.id, note));
    }
    actions.sort_by_key(|(tick, kind, id, _)| (*tick, *kind, *id));
    let mut previous = 0;
    for (time, kind, id, note) in actions {
        vlq(time - previous, &mut out)?;
        if kind == 1 {
            meta(
                &mut out,
                1,
                format!("bn:{}:{}", id.take, id.sequence).as_bytes(),
            )?;
            out.extend_from_slice(&[0, 0x90 | note.channel, note.pitch, note.velocity]);
        } else {
            out.extend_from_slice(&[0x80 | note.channel, note.pitch, 0]);
        }
        previous = time;
    }
    out.extend_from_slice(&[0, 0xff, 0x2f, 0]);
    Ok(out)
}
