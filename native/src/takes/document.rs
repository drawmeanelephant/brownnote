//! Versioned project DTO for committed takes (D9, D11, D14). Only committed
//! originals are saved; tentative passes are never persisted. Reopen rebuilds
//! every note through the real `NoteLifecycle`, so no timing setter exists.
use super::{Removal, Take, Takes};
use crate::{
    performance::{
        Channel, EventId, InstrumentId, LoopBars, NoteKey, NoteLifecycle, NoteOn, OriginalTime,
        Pitch, SourceId, TakeId, TakeTiming, Target, Tempo, TempoControl, Velocity,
    },
    project::{AssetId, PadId},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

pub const TAKES_FORMAT_VERSION: u32 = 1;

/// u128 identities are decimal strings: serde_json has no portable u128.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakesDocument {
    pub version: u32,
    pub tempo_milli_bpm: u32,
    pub loop_bars: u8,
    /// First take ID a reopened registry may assign; above every saved ID.
    pub next_take: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    pub takes: Vec<TakeDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakeDocument {
    pub id: String,
    pub recording_tempo_milli_bpm: u32,
    pub muted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal: Option<RemovalDocument>,
    /// Note-on acceptance (sequence) order.
    pub notes: Vec<NoteDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemovalDocument {
    User,
    ReplacedBy { take: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteDocument {
    pub sequence: u64,
    pub target: TargetDocument,
    pub source: String,
    pub channel: u8,
    pub pitch: u8,
    pub velocity: u8,
    /// Original take-relative nanoseconds, never quantized or wrapped.
    pub on_nanos: u64,
    pub end_nanos: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TargetDocument {
    Pad { pad: PadId },
    Sample { asset: AssetId },
    Instrument { instrument: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TakesDocumentError {
    UnsupportedVersion(u32),
    InvalidTempo,
    InvalidLoopBars(u8),
    InvalidIdentity(String),
    DuplicateTake(TakeId),
    UnknownTake(TakeId),
    NextTakeNotAbove(TakeId),
    SequenceOrder(EventId),
    InvalidNote(EventId),
}
impl fmt::Display for TakesDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "takes document: {self:?}")
    }
}
impl std::error::Error for TakesDocumentError {}

fn identity(value: &str) -> Result<u128, TakesDocumentError> {
    // Canonical decimal only, so one ID has exactly one spelling.
    match value.parse::<u128>() {
        Ok(parsed) if parsed.to_string() == value => Ok(parsed),
        _ => Err(TakesDocumentError::InvalidIdentity(value.into())),
    }
}

impl TakesDocument {
    /// Committed takes only. The caller refuses to save while a pass is active.
    pub fn from_takes(takes: &Takes) -> Self {
        Self {
            version: TAKES_FORMAT_VERSION,
            tempo_milli_bpm: takes.tempo().milli_bpm(),
            loop_bars: takes.loop_bars().bars(),
            next_take: takes
                .next_take
                .map_or_else(|| u128::MAX.to_string(), |id| id.0.to_string()),
            selected: takes.selected.map(|id| id.0.to_string()),
            takes: takes.takes().map(TakeDocument::from_take).collect(),
        }
    }

    /// Validates everything before building; the result has no active pass.
    pub fn to_takes(&self) -> Result<Takes, TakesDocumentError> {
        if self.version != TAKES_FORMAT_VERSION {
            return Err(TakesDocumentError::UnsupportedVersion(self.version));
        }
        let tempo =
            Tempo::try_from(self.tempo_milli_bpm).map_err(|_| TakesDocumentError::InvalidTempo)?;
        let loop_bars = LoopBars::try_from(self.loop_bars)
            .map_err(|_| TakesDocumentError::InvalidLoopBars(self.loop_bars))?;
        let mut takes = BTreeMap::new();
        let mut removals = Vec::new();
        for document in &self.takes {
            let (take, removal) = document.to_take()?;
            removals.extend(removal);
            if takes.insert(take.id, take).is_some() {
                return Err(TakesDocumentError::DuplicateTake(TakeId(identity(
                    &document.id,
                )?)));
            }
        }
        for replacement in removals {
            if !takes.contains_key(&replacement) {
                return Err(TakesDocumentError::UnknownTake(replacement));
            }
        }
        let next = TakeId(identity(&self.next_take)?);
        if let Some(last) = takes.keys().next_back()
            && *last >= next
        {
            return Err(TakesDocumentError::NextTakeNotAbove(*last));
        }
        let selected = match &self.selected {
            Some(id) => {
                let id = TakeId(identity(id)?);
                match takes.get(&id) {
                    Some(take) if take.removal.is_none() => Some(id),
                    _ => return Err(TakesDocumentError::UnknownTake(id)),
                }
            }
            None => None,
        };
        let mut control = TempoControl::default();
        control
            .set_tempo(tempo)
            .map_err(|_| TakesDocumentError::InvalidTempo)?;
        Ok(Takes {
            tempo: control,
            loop_bars,
            // u128::MAX is the saved "exhausted" marker.
            next_take: (next.0 != u128::MAX).then_some(next),
            passes: BTreeMap::new(),
            takes,
            selected,
        })
    }
}

impl TakeDocument {
    fn from_take(take: &Take) -> Self {
        Self {
            id: take.id.0.to_string(),
            recording_tempo_milli_bpm: take.timing.recording_tempo().milli_bpm(),
            muted: take.muted,
            removal: take.removal.map(|removal| match removal {
                Removal::User => RemovalDocument::User,
                Removal::ReplacedBy(id) => RemovalDocument::ReplacedBy {
                    take: id.0.to_string(),
                },
            }),
            notes: take
                .notes
                .iter()
                .map(|note| {
                    let on = note.on();
                    NoteDocument {
                        sequence: on.id.sequence,
                        target: match on.target {
                            Target::Pad(pad) => TargetDocument::Pad { pad },
                            Target::Sample(asset) => TargetDocument::Sample { asset },
                            Target::Instrument(id) => TargetDocument::Instrument {
                                instrument: id.0.to_string(),
                            },
                        },
                        source: on.key.source.0.to_string(),
                        channel: on.key.channel.value(),
                        pitch: on.key.pitch.value(),
                        velocity: on.velocity.value(),
                        on_nanos: on.original_time.nanos(),
                        end_nanos: note.original_end().nanos(),
                    }
                })
                .collect(),
        }
    }

    fn to_take(&self) -> Result<(Take, Option<TakeId>), TakesDocumentError> {
        let id = TakeId(identity(&self.id)?);
        let tempo = Tempo::try_from(self.recording_tempo_milli_bpm)
            .map_err(|_| TakesDocumentError::InvalidTempo)?;
        let mut notes = Vec::with_capacity(self.notes.len());
        let mut previous = None;
        for document in &self.notes {
            let event = EventId {
                take: id,
                sequence: document.sequence,
            };
            if previous.is_some_and(|previous| previous >= document.sequence) {
                return Err(TakesDocumentError::SequenceOrder(event));
            }
            previous = Some(document.sequence);
            let invalid = TakesDocumentError::InvalidNote(event);
            let target = match &document.target {
                TargetDocument::Pad { pad } => Target::Pad(*pad),
                TargetDocument::Sample { asset } => Target::Sample(*asset),
                TargetDocument::Instrument { instrument } => {
                    Target::Instrument(InstrumentId(identity(instrument)?))
                }
            };
            let key = NoteKey {
                source: SourceId(identity(&document.source)?),
                channel: Channel::try_from(document.channel).map_err(|_| invalid.clone())?,
                pitch: Pitch::try_from(document.pitch).map_err(|_| invalid.clone())?,
            };
            // One note at a time through the real lifecycle: the end pairs with
            // exactly this note-on, and end-before-start is rejected.
            let mut lifecycle = NoteLifecycle::default();
            lifecycle
                .note_on(NoteOn {
                    id: event,
                    target,
                    key,
                    velocity: Velocity::try_from(document.velocity).map_err(|_| invalid.clone())?,
                    original_time: OriginalTime::from_nanos(document.on_nanos),
                })
                .map_err(|_| invalid.clone())?;
            notes.push(
                lifecycle
                    .note_off(key, OriginalTime::from_nanos(document.end_nanos))
                    .map_err(|_| invalid)?,
            );
        }
        let (removal, replaced_by) = match &self.removal {
            None => (None, None),
            Some(RemovalDocument::User) => (Some(Removal::User), None),
            Some(RemovalDocument::ReplacedBy { take }) => {
                let by = TakeId(identity(take)?);
                (Some(Removal::ReplacedBy(by)), Some(by))
            }
        };
        Ok((
            Take {
                id,
                timing: TakeTiming::new(id, tempo),
                notes,
                muted: self.muted,
                removal,
            },
            replaced_by,
        ))
    }
}
