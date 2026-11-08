#![allow(clippy::unwrap_used, clippy::expect_used)]
use brownnote_native::{midi, performance, project};
use midi::{
    mapping::*,
    parser::{Data7, Message, ParseError, parse_message},
};
use performance::*;
use project::PadId;
use serde_json::json;

fn channel(value: u8) -> Channel {
    Channel::try_from(value).unwrap()
}
fn pitch(value: u8) -> Pitch {
    Pitch::try_from(value).unwrap()
}
fn data(value: u8) -> Data7 {
    Data7::try_from(value).unwrap()
}
fn maps(target: NoteTarget) -> Mappings {
    Mappings::new(vec![
        Mapping::Note {
            channel: channel(1),
            pitch: pitch(40),
            target,
        },
        Mapping::Controller {
            channel: channel(1),
            number: data(64),
            action: ControlAction::Sustain,
        },
        Mapping::Controller {
            channel: channel(1),
            number: data(123),
            action: ControlAction::AllNotesOff,
        },
        Mapping::Controller {
            channel: channel(1),
            number: data(120),
            action: ControlAction::Panic,
        },
    ])
    .unwrap()
}
fn resolve(mappings: &Mappings, source: u128, bytes: &[u8]) -> Option<Intent> {
    mappings.resolve(SourceId(source), parse_message(bytes).unwrap())
}
fn held_on(mappings: &Mappings, source: u128, take: u128, sequence: u64, nanos: u64) -> NoteOn {
    let Some(Intent::Start {
        key,
        target: NoteTarget::Held(instrument),
        velocity,
    }) = resolve(mappings, source, &[0x91, 40, 100])
    else {
        panic!("expected held instrument onset");
    };
    NoteOn {
        id: EventId {
            take: TakeId(take),
            sequence,
        },
        target: Target::Instrument(instrument),
        key,
        velocity,
        original_time: OriginalTime::from_nanos(nanos),
    }
}

#[test]
fn checked_in_message_fixtures() {
    let fixtures: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/midi/messages.json")).unwrap();
    for fixture in fixtures {
        let bytes: Vec<u8> = serde_json::from_value(fixture["bytes"].clone()).unwrap();
        match parse_message(&bytes) {
            Ok(message) => {
                let event = match message {
                    Message::NoteOn {
                        channel,
                        pitch,
                        velocity,
                    } => json!(["on", channel.value(), pitch.value(), velocity.value()]),
                    Message::NoteOff {
                        channel,
                        pitch,
                        release_velocity,
                    } => json!([
                        "off",
                        channel.value(),
                        pitch.value(),
                        release_velocity.value()
                    ]),
                    Message::ControlChange {
                        channel,
                        controller,
                        value,
                    } => json!(["cc", channel.value(), controller.value(), value.value()]),
                };
                assert_eq!(event, fixture["event"], "{}", fixture["name"]);
                assert!(fixture.get("error").is_none(), "{}", fixture["name"]);
            }
            Err(error) => {
                assert_eq!(
                    format!("{error:?}"),
                    fixture["error"],
                    "{}",
                    fixture["name"]
                );
                assert!(fixture.get("event").is_none(), "{}", fixture["name"]);
            }
        }
    }
}

#[test]
fn every_channel_pitch_and_velocity_preserves_wire_values() {
    for wire_channel in 0..16 {
        for wire_pitch in 0..128 {
            for wire_velocity in 0..128 {
                let message =
                    parse_message(&[0x90 | wire_channel, wire_pitch, wire_velocity]).unwrap();
                if wire_velocity == 0 {
                    assert_eq!(
                        message,
                        Message::NoteOff {
                            channel: channel(wire_channel),
                            pitch: pitch(wire_pitch),
                            release_velocity: data(0),
                        }
                    );
                } else {
                    assert_eq!(
                        message,
                        Message::NoteOn {
                            channel: channel(wire_channel),
                            pitch: pitch(wire_pitch),
                            velocity: Velocity::try_from(wire_velocity).unwrap(),
                        }
                    );
                }
            }
        }
    }
}

#[test]
fn unsupported_status_and_invalid_data_never_fabricate_events_or_leak_state() {
    for status in 0..=255 {
        let supported = matches!(status & 0xf0, 0x80 | 0x90 | 0xb0);
        assert_eq!(parse_message(&[status, 40, 1]).is_ok(), supported);
        assert!(parse_message(&[status]).is_err());
        assert!(parse_message(&[status, 40]).is_err());
        for invalid in 128..=255 {
            assert!(parse_message(&[status, invalid, 1]).is_err());
            assert!(parse_message(&[status, 40, invalid]).is_err());
        }
    }
    assert!(parse_message(&[40, 127]).is_err());
    assert!(parse_message(&[0x91, 40, 1]).is_ok());
    assert_eq!(parse_message(&[40, 127]), Err(ParseError::MissingStatus));
    assert_eq!(Data7::try_from(128), Err(ParseError::InvalidData));
}

#[test]
fn channel_two_note_forty_only_and_full_velocity() {
    let target = NoteTarget::OneShot(PadId::try_from(3).unwrap());
    let mappings = maps(target);
    for velocity in [1, 127] {
        let Some(Intent::Start {
            key,
            target: actual,
            velocity: actual_velocity,
        }) = resolve(&mappings, 11, &[0x91, 40, velocity])
        else {
            panic!("expected mapped onset");
        };
        assert_eq!(actual, target);
        assert_eq!(actual_velocity.value(), velocity);
        assert_eq!(key.source, SourceId(11));
        assert_eq!(key.channel, channel(1));
        assert_eq!(key.pitch, pitch(40));
    }
    for other_channel in 0..16 {
        if other_channel != 1 {
            assert_eq!(
                resolve(&mappings, 11, &[0x90 | other_channel, 40, 127]),
                None
            );
        }
    }
    assert_eq!(resolve(&mappings, 11, &[0x91, 41, 127]), None);
}

#[test]
fn learn_is_channel_aware_and_never_learns_a_release() {
    assert_eq!(
        MappingKey::learn(parse_message(&[0x91, 40, 1]).unwrap()),
        Some(MappingKey::Note {
            channel: channel(1),
            pitch: pitch(40),
        })
    );
    assert_eq!(
        MappingKey::learn(parse_message(&[0xb1, 64, 127]).unwrap()),
        Some(MappingKey::Controller {
            channel: channel(1),
            number: data(64),
        })
    );
    for bytes in [[0x91, 40, 0], [0x81, 40, 72]] {
        assert_eq!(MappingKey::learn(parse_message(&bytes).unwrap()), None);
    }
}

#[test]
fn ambiguous_mapping_keys_rejected_but_channels_and_message_kinds_are_distinct() {
    let mappings = maps(NoteTarget::Held(InstrumentId(17)));
    let original = mappings.entries().to_vec();
    for duplicate in [original[0], original[1]] {
        let mut entries = original.clone();
        entries.push(duplicate);
        assert_eq!(
            Mappings::new(entries),
            Err(DuplicateMapping(duplicate.key()))
        );
    }
    let mut entries = original;
    entries.push(Mapping::Note {
        channel: channel(0),
        pitch: pitch(40),
        target: NoteTarget::OneShot(PadId::try_from(0).unwrap()),
    });
    entries.push(Mapping::Controller {
        channel: channel(1),
        number: data(40),
        action: ControlAction::Panic,
    });
    assert!(Mappings::new(entries).is_ok());
}

#[test]
fn releases_remain_keyed_to_held_identity_even_after_mapping_changes() {
    let mappings = maps(NoteTarget::Held(InstrumentId(17)));
    let mut lifecycle = NoteLifecycle::default();
    let first = held_on(&mappings, 11, 1, 1, 10);
    lifecycle.note_on(first.clone()).unwrap();
    let changed = Mappings::new(vec![]).unwrap();
    for bytes in [[0x91, 40, 0], [0x81, 40, 72]] {
        assert_eq!(
            resolve(&changed, 11, &bytes),
            Some(Intent::Release(first.key))
        );
    }
    let Some(Intent::Release(key)) = resolve(&changed, 11, &[0x91, 40, 0]) else {
        panic!("expected release independent of current mapping");
    };
    let note = lifecycle
        .note_off(key, OriginalTime::from_nanos(20))
        .unwrap();
    assert_eq!(note.on().id, first.id);
    assert_eq!(note.on().target, Target::Instrument(InstrumentId(17)));
    assert_eq!(note.duration_nanos(), 10);
}

#[test]
fn mapped_overlaps_use_actual_shared_ids_and_fifo_across_takes_and_sources() {
    let mappings = maps(NoteTarget::Held(InstrumentId(17)));
    let mut lifecycle = NoteLifecycle::default();
    let first = held_on(&mappings, 11, 1, 1, 10);
    let second = held_on(&mappings, 11, 2, 1, 11);
    let other_source = held_on(&mappings, 12, 2, 2, 12);
    for on in [&first, &second, &other_source] {
        lifecycle.note_on(on.clone()).unwrap();
    }
    for (source, expected, end) in [(11, first, 20), (12, other_source, 21), (11, second, 22)] {
        let Some(Intent::Release(key)) = resolve(&mappings, source, &[0x91, 40, 0]) else {
            panic!("expected release");
        };
        let note = lifecycle
            .note_off(key, OriginalTime::from_nanos(end))
            .unwrap();
        assert_eq!(note.on(), &expected);
        assert_eq!(note.original_end().nanos(), end);
    }
    assert_eq!(lifecycle.active_count(), 0);
}

#[test]
fn one_shot_release_has_no_pad_target_and_controls_are_source_channel_scoped() {
    let mappings = maps(NoteTarget::OneShot(PadId::try_from(3).unwrap()));
    assert!(matches!(
        resolve(&mappings, 11, &[0x91, 40, 127]),
        Some(Intent::Start {
            target: NoteTarget::OneShot(_),
            ..
        })
    ));
    // No held note is created by a one-shot consumer. Release has no pad target.
    assert!(matches!(
        resolve(&mappings, 11, &[0x91, 40, 0]),
        Some(Intent::Release(_))
    ));
    let before = mappings.clone();
    for source in [11, 12] {
        for (controller, value, action) in [
            (64, 0, ControlIntent::Sustain(false)),
            (64, 63, ControlIntent::Sustain(false)),
            (64, 64, ControlIntent::Sustain(true)),
            (64, 127, ControlIntent::Sustain(true)),
            (123, 0, ControlIntent::AllNotesOff),
            (120, 0, ControlIntent::Panic),
        ] {
            assert_eq!(
                resolve(&mappings, source, &[0xb1, controller, value]),
                Some(Intent::Control {
                    scope: SourceChannel {
                        source: SourceId(source),
                        channel: channel(1)
                    },
                    action,
                })
            );
            assert_eq!(resolve(&mappings, source, &[0xb0, controller, value]), None);
        }
    }
    assert_eq!(resolve(&mappings, 11, &[0xb1, 7, 99]), None);
    assert_eq!(mappings, before);
}
