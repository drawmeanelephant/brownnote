//! Semantic project DTOs and immutable resolution, not a persistence format.
//! The registry owns source/event/take IDs; a later producer owns held state.
use super::parser::{Data7, Message};
use crate::{
    performance::{Channel, InstrumentId, NoteKey, Pitch, SourceId, Velocity},
    project::PadId,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MappingKey {
    Note { channel: Channel, pitch: Pitch },
    Controller { channel: Channel, number: Data7 },
}

impl MappingKey {
    /// Learn candidates do not include release (including velocity-zero on).
    pub fn learn(message: Message) -> Option<Self> {
        match message {
            Message::NoteOn { channel, pitch, .. } => Some(Self::Note { channel, pitch }),
            Message::ControlChange {
                channel,
                controller,
                ..
            } => Some(Self::Controller {
                channel,
                number: controller,
            }),
            Message::NoteOff { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteTarget {
    OneShot(PadId),
    Held(InstrumentId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlAction {
    Sustain,
    AllNotesOff,
    Panic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapping {
    Note {
        channel: Channel,
        pitch: Pitch,
        target: NoteTarget,
    },
    Controller {
        channel: Channel,
        number: Data7,
        action: ControlAction,
    },
}

impl Mapping {
    pub fn key(self) -> MappingKey {
        match self {
            Self::Note { channel, pitch, .. } => MappingKey::Note { channel, pitch },
            Self::Controller {
                channel, number, ..
            } => MappingKey::Controller { channel, number },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceChannel {
    pub source: SourceId,
    pub channel: Channel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlIntent {
    Sustain(bool),
    AllNotesOff,
    Panic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Start {
        key: NoteKey,
        target: NoteTarget,
        velocity: Velocity,
    },
    /// Resolve against already-held identities, never the current mapping.
    /// One-shot starts are not held and therefore ignore this release.
    Release(NoteKey),
    Control {
        scope: SourceChannel,
        action: ControlIntent,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplicateMapping(pub MappingKey);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mappings(Vec<Mapping>);

impl Mappings {
    pub fn new(mappings: Vec<Mapping>) -> Result<Self, DuplicateMapping> {
        let mut keys = BTreeSet::new();
        for mapping in &mappings {
            if !keys.insert(mapping.key()) {
                return Err(DuplicateMapping(mapping.key()));
            }
        }
        Ok(Self(mappings))
    }

    pub fn entries(&self) -> &[Mapping] {
        &self.0
    }

    pub fn resolve(&self, source: SourceId, message: Message) -> Option<Intent> {
        match message {
            Message::NoteOff { channel, pitch, .. } => Some(Intent::Release(NoteKey {
                source,
                channel,
                pitch,
            })),
            Message::NoteOn {
                channel,
                pitch,
                velocity,
            } => self.0.iter().find_map(|mapping| match *mapping {
                Mapping::Note {
                    channel: mapped_channel,
                    pitch: mapped_pitch,
                    target,
                } if channel == mapped_channel && pitch == mapped_pitch => Some(Intent::Start {
                    key: NoteKey {
                        source,
                        channel,
                        pitch,
                    },
                    target,
                    velocity,
                }),
                _ => None,
            }),
            Message::ControlChange {
                channel,
                controller,
                value,
            } => self.0.iter().find_map(|mapping| match *mapping {
                Mapping::Controller {
                    channel: mapped_channel,
                    number,
                    action,
                } if channel == mapped_channel && controller == number => Some(Intent::Control {
                    scope: SourceChannel { source, channel },
                    action: match action {
                        ControlAction::Sustain => ControlIntent::Sustain(value.value() >= 64),
                        ControlAction::AllNotesOff => ControlIntent::AllNotesOff,
                        ControlAction::Panic => ControlIntent::Panic,
                    },
                }),
                _ => None,
            }),
        }
    }
}
