//! One complete MIDI 1.0 message per call, as delivered by the selected backend.
//! Raw streams, running status, concatenated messages and SysEx are not accepted.
use crate::performance::{Channel, Pitch, Velocity};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Data7(u8);

impl TryFrom<u8> for Data7 {
    type Error = ParseError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < 128 {
            Ok(Self(value))
        } else {
            Err(ParseError::InvalidData)
        }
    }
}
impl Data7 {
    pub fn value(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    NoteOn {
        channel: Channel,
        pitch: Pitch,
        velocity: Velocity,
    },
    NoteOff {
        channel: Channel,
        pitch: Pitch,
        release_velocity: Data7,
    },
    ControlChange {
        channel: Channel,
        controller: Data7,
        value: Data7,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    MissingStatus,
    UnsupportedStatus(u8),
    WrongLength,
    InvalidData,
}

pub fn parse_message(bytes: &[u8]) -> Result<Message, ParseError> {
    let status = *bytes.first().ok_or(ParseError::MissingStatus)?;
    if status < 128 {
        return Err(ParseError::MissingStatus);
    }
    let kind = status & 0xf0;
    if !matches!(kind, 0x80 | 0x90 | 0xb0) {
        return Err(ParseError::UnsupportedStatus(status));
    }
    let [_, first, second] = bytes else {
        return Err(ParseError::WrongLength);
    };
    let first = Data7::try_from(*first)?;
    let second = Data7::try_from(*second)?;
    let channel = Channel::try_from(status & 0x0f).map_err(|_| ParseError::InvalidData)?;
    if kind == 0xb0 {
        return Ok(Message::ControlChange {
            channel,
            controller: first,
            value: second,
        });
    }
    let pitch = Pitch::try_from(first.value()).map_err(|_| ParseError::InvalidData)?;
    if kind == 0x80 || second.value() == 0 {
        Ok(Message::NoteOff {
            channel,
            pitch,
            release_velocity: second,
        })
    } else {
        Ok(Message::NoteOn {
            channel,
            pitch,
            velocity: Velocity::try_from(second.value()).map_err(|_| ParseError::InvalidData)?,
        })
    }
}
