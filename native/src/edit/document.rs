//! Versioned DTO for the current edit layer only (D15): history never saves.
use super::{EditModel, EventEdit};
use crate::{
    performance::{EventId, Quantization, TakeId, Velocity},
    takes::Takes,
};
use serde::{Deserialize, Serialize};
use std::fmt;

pub const EDITS_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditsDocument {
    pub version: u32,
    /// Takes whose D13 quantization is on; absent means off.
    pub quantized: Vec<String>,
    pub events: Vec<EventEditDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEditDocument {
    pub take: String,
    pub sequence: u64,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    pub nudge_nanos: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditsDocumentError {
    UnsupportedVersion(u32),
    InvalidIdentity(String),
    UnknownTake(TakeId),
    UnknownEvent(EventId),
    InvalidVelocity(EventId),
    Duplicate,
}
impl fmt::Display for EditsDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "edits document: {self:?}")
    }
}
impl std::error::Error for EditsDocumentError {}

fn take_id(value: &str) -> Result<TakeId, EditsDocumentError> {
    match value.parse::<u128>() {
        Ok(parsed) if parsed.to_string() == value => Ok(TakeId(parsed)),
        _ => Err(EditsDocumentError::InvalidIdentity(value.into())),
    }
}

impl EditsDocument {
    pub fn from_model(model: &EditModel) -> Self {
        Self {
            version: EDITS_FORMAT_VERSION,
            quantized: model
                .quantization
                .iter()
                .filter(|(_, grid)| **grid != Quantization::Off)
                .map(|(take, _)| take.0.to_string())
                .collect(),
            events: model
                .events
                .iter()
                .map(|(id, edit)| EventEditDocument {
                    take: id.take.0.to_string(),
                    sequence: id.sequence,
                    deleted: edit.deleted,
                    velocity: edit.velocity.map(Velocity::value),
                    nudge_nanos: edit.nudge_nanos,
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.quantized.is_empty() && self.events.is_empty()
    }

    /// Every reference must name a saved take/event; history starts empty.
    pub fn to_model(&self, takes: &Takes) -> Result<EditModel, EditsDocumentError> {
        if self.version != EDITS_FORMAT_VERSION {
            return Err(EditsDocumentError::UnsupportedVersion(self.version));
        }
        let mut model = EditModel::new();
        for take in &self.quantized {
            let id = take_id(take)?;
            takes.take(id).ok_or(EditsDocumentError::UnknownTake(id))?;
            if model
                .quantization
                .insert(id, Quantization::Sixteenth)
                .is_some()
            {
                return Err(EditsDocumentError::Duplicate);
            }
        }
        for event in &self.events {
            let id = EventId {
                take: take_id(&event.take)?,
                sequence: event.sequence,
            };
            let take = takes
                .take(id.take)
                .ok_or(EditsDocumentError::UnknownTake(id.take))?;
            if !take.notes().iter().any(|note| note.on().id == id) {
                return Err(EditsDocumentError::UnknownEvent(id));
            }
            let velocity = event
                .velocity
                .map(Velocity::try_from)
                .transpose()
                .map_err(|_| EditsDocumentError::InvalidVelocity(id))?;
            let edit = EventEdit {
                deleted: event.deleted,
                velocity,
                nudge_nanos: event.nudge_nanos,
            };
            if edit != EventEdit::default() && model.events.insert(id, edit).is_some() {
                return Err(EditsDocumentError::Duplicate);
            }
        }
        Ok(model)
    }
}
