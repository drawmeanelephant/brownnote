#![allow(clippy::unwrap_used, clippy::expect_used)]
use brownnote_native::{performance, project};

use performance::*;
use project::{AssetId, AssetRef, PadId};
use std::num::NonZeroU32;

fn tempo(bpm: u32) -> Tempo {
    Tempo::try_from(bpm * 1000).unwrap()
}
fn time(nanos: u64) -> OriginalTime {
    OriginalTime::from_nanos(nanos)
}
fn rate(hz: u32) -> SampleRate {
    SampleRate::try_from(hz).unwrap()
}
fn key(source: u128, channel: u8, pitch: u8) -> NoteKey {
    NoteKey {
        source: SourceId(source),
        channel: Channel::try_from(channel).unwrap(),
        pitch: Pitch::try_from(pitch).unwrap(),
    }
}
fn on(sequence: u64, nanos: u64, key: NoteKey) -> NoteOn {
    NoteOn {
        id: EventId {
            take: TakeId(1),
            sequence,
        },
        target: Target::Pad(PadId::try_from(15).unwrap()),
        key,
        velocity: Velocity::try_from(100).unwrap(),
        original_time: time(nanos),
    }
}
fn completed(sequence: u64, start: u64, end: u64) -> Note {
    let mut lifecycle = NoteLifecycle::default();
    let key = key(1, 0, 60);
    lifecycle.note_on(on(sequence, start, key)).unwrap();
    lifecycle.note_off(key, time(end)).unwrap()
}

#[test]
fn performance_fifo_same_pitch_distinct_notes_source_channel_and_pitch_isolation() {
    let mut lifecycle = NoteLifecycle::default();
    let a = key(1, 0, 60);
    let b = key(2, 0, 60);
    let c = key(1, 1, 60);
    let d = key(1, 0, 61);
    for (sequence, key) in [(1, a), (2, a), (3, b), (4, c), (5, d)] {
        lifecycle
            .note_on(on(sequence, sequence * 1_000_000, key))
            .unwrap();
    }
    assert_eq!(lifecycle.active_count(), 5);
    for (key, sequence, end) in [(b, 3, 6), (a, 1, 7), (c, 4, 8), (d, 5, 9), (a, 2, 10)] {
        let note = lifecycle.note_off(key, time(end * 1_000_000)).unwrap();
        assert_eq!(note.on().id.sequence, sequence);
        assert_eq!(note.on().original_time.nanos(), sequence * 1_000_000);
        assert_eq!(note.original_end().nanos(), end * 1_000_000);
        assert_eq!(note.duration_nanos(), (end - sequence) * 1_000_000);
    }
    assert_eq!(lifecycle.active_count(), 0);
    assert_eq!(
        lifecycle.note_off(a, time(11_000_000)),
        Err(PerformanceError::UnmatchedNoteOff(a))
    );
}

#[test]
fn performance_lifecycle_failures_do_not_consume_held_notes_or_reuse_identity() {
    let mut lifecycle = NoteLifecycle::default();
    let key = key(1, 0, 60);
    let first = on(1, 20, key);
    lifecycle.note_on(first.clone()).unwrap();
    assert_eq!(
        lifecycle.note_on(first.clone()),
        Err(PerformanceError::DuplicateEvent(first.id))
    );
    assert_eq!(
        lifecycle.note_off(key, time(19)),
        Err(PerformanceError::EndBeforeStart)
    );
    assert_eq!(lifecycle.active_count(), 1);
    assert_eq!(lifecycle.note_off(key, time(21)).unwrap().on().id, first.id);
    assert_eq!(
        lifecycle.note_on(first),
        Err(PerformanceError::DuplicateEvent(EventId {
            take: TakeId(1),
            sequence: 1
        }))
    );
    let mut another_take = on(1, 22, key);
    another_take.id.take = TakeId(2);
    lifecycle.note_on(another_take).unwrap();
    assert_eq!(
        lifecycle.note_off(key, time(23)).unwrap().on().id.take,
        TakeId(2)
    );
}

#[test]
fn performance_flam_quantization_keeps_both_events_and_restores_exact_originals() {
    let first = completed(1, 123_456_789, 133_456_789);
    let second = completed(2, 124_456_790, 134_456_790);
    let originals = [first.clone(), second.clone()];
    let mut timing = TakeTiming::new(TakeId(1), tempo(120));
    assert_eq!(timing.recording_tempo(), tempo(120));
    assert_eq!(timing.quantization(), Quantization::Off);
    for _ in 0..4 {
        timing.set_quantization(Quantization::Sixteenth);
        let views: Vec<_> = originals.iter().map(|n| timing.view(n).unwrap()).collect();
        assert_ne!(views[0].id, views[1].id);
        for view in &views {
            assert_eq!(view.velocity.value(), 100);
            assert_eq!(view.key, originals[0].on().key);
            assert_eq!(view.start.at_tempo_nanos(tempo(120)).unwrap(), 125_000_000);
            assert_eq!(view.end.at_tempo_nanos(tempo(120)).unwrap(), 135_000_000);
        }
        timing.set_quantization(Quantization::Off);
        for note in &originals {
            let view = timing.view(note).unwrap();
            assert_eq!(
                view.start.at_tempo_nanos(tempo(120)).unwrap(),
                note.on().original_time.nanos()
            );
            assert_eq!(
                view.end.at_tempo_nanos(tempo(120)).unwrap(),
                note.original_end().nanos()
            );
        }
    }
    assert_eq!(originals, [first, second]);
    assert_eq!(
        TakeTiming::new(TakeId(2), tempo(120)).view(&originals[0]),
        Err(PerformanceError::WrongTake)
    );
    // A second take's view is independent of the first take's quantization.
    assert_eq!(
        TakeTiming::new(TakeId(1), tempo(120)).quantization(),
        Quantization::Off
    );
}

#[test]
fn performance_tempo_edit_keeps_musical_position_without_rewriting_originals() {
    let note = completed(1, 500_000_000, 750_000_000);
    let timing = TakeTiming::new(TakeId(1), tempo(120));
    let view = timing.view(&note).unwrap();
    assert_eq!(view.start.at_tempo_nanos(tempo(60)).unwrap(), 1_000_000_000);
    assert_eq!(view.end.at_tempo_nanos(tempo(60)).unwrap(), 1_500_000_000);
    assert_eq!(
        view.start.at_tempo_frames(tempo(60), rate(44_100)).unwrap(),
        44_100
    );
    assert_eq!(
        view.start.at_tempo_frames(tempo(60), rate(48_000)).unwrap(),
        48_000
    );
    assert_eq!(note.on().original_time.nanos(), 500_000_000);
    assert_eq!(note.original_end().nanos(), 750_000_000);
    assert_eq!(
        view.start,
        MusicalPosition::from_original(time(1_000_000_000), tempo(60))
    );
}

#[test]
fn performance_tempo_is_locked_until_every_armed_or_recording_take_finishes() {
    let mut control = TempoControl::default();
    assert_eq!(control.tempo().milli_bpm(), 116_000);
    assert_eq!(control.record(TakeId(1)), Err(PerformanceError::NotArmed));
    let captured_tempo = control.arm(TakeId(1)).unwrap();
    assert_eq!(captured_tempo, tempo(116));
    assert_eq!(control.arm(TakeId(1)), Err(PerformanceError::AlreadyArmed));
    control.arm(TakeId(2)).unwrap();
    assert_eq!(
        control.set_tempo(tempo(120)),
        Err(PerformanceError::TempoLocked)
    );
    control.record(TakeId(1)).unwrap();
    assert_eq!(control.record(TakeId(1)), Err(PerformanceError::NotArmed));
    control.finish(TakeId(2)).unwrap();
    assert_eq!(
        control.set_tempo(tempo(120)),
        Err(PerformanceError::TempoLocked)
    );
    control.finish(TakeId(1)).unwrap();
    control.set_tempo(tempo(120)).unwrap();
    assert_eq!(control.tempo(), tempo(120));
    assert_eq!(
        control.finish(TakeId(1)),
        Err(PerformanceError::UnknownTake)
    );
}

#[test]
fn performance_loop_boundary_quantized_wrap_and_held_note_keep_unwrapped_originals() {
    let one = LoopBars::try_from(1).unwrap();
    let two = LoopBars::try_from(2).unwrap();
    let before = MusicalPosition::from_original(time(1_999_999_999), tempo(120));
    let boundary = MusicalPosition::from_original(time(2_000_000_000), tempo(120));
    assert_eq!(before.split_loop(one).unwrap().iteration, 0);
    assert_eq!(boundary.split_loop(one).unwrap().iteration, 1);
    assert_eq!(
        boundary
            .split_loop(one)
            .unwrap()
            .within
            .at_tempo_nanos(tempo(120))
            .unwrap(),
        0
    );
    assert!(before.visible_in(one));
    assert!(!boundary.visible_in(one));
    assert!(boundary.visible_in(two));
    assert_eq!(
        LoopBars::try_from(3),
        Err(PerformanceError::InvalidLoopBars)
    );
    for bars in [1, 2, 4, 8] {
        assert!(LoopBars::try_from(bars).is_ok());
    }
    let note = completed(1, 1_990_000_000, 2_050_000_000);
    let mut timing = TakeTiming::new(TakeId(1), tempo(120));
    timing.set_quantization(Quantization::Sixteenth);
    let view = timing.view(&note).unwrap();
    assert_eq!(view.start.split_loop(one).unwrap().iteration, 1);
    assert_eq!(view.end.at_tempo_nanos(tempo(120)).unwrap(), 2_060_000_000);
    timing.set_quantization(Quantization::Off);
    assert_eq!(
        timing
            .view(&note)
            .unwrap()
            .start
            .at_tempo_nanos(tempo(120))
            .unwrap(),
        1_990_000_000
    );
    assert_eq!(note.duration_nanos(), 60_000_000);
}

#[test]
fn performance_clock_fixture_reopens_at_both_rates_without_resampling_stored_times() {
    for line in include_str!("fixtures/performance-clocks.csv")
        .lines()
        .filter(|l| !l.starts_with('#'))
    {
        let values: Vec<u64> = line.split(',').map(|v| v.parse().unwrap()).collect();
        let original = time(values[0]);
        let position = MusicalPosition::from_original(original, tempo(120));
        assert_eq!(
            position.at_tempo_frames(tempo(120), rate(44_100)).unwrap(),
            values[1]
        );
        assert_eq!(
            position.at_tempo_frames(tempo(120), rate(48_000)).unwrap(),
            values[2]
        );
        assert_eq!(position.at_tempo_nanos(tempo(120)).unwrap(), values[0]);
        assert_eq!(original.nanos(), values[0]);
    }
    for hz in [44_100, 48_000] {
        for frame in [0, 1, 31, 64, 256, hz as u64] {
            let captured = OriginalTime::from_frames(frame, rate(hz)).unwrap();
            let position = MusicalPosition::from_original(captured, tempo(120));
            assert_eq!(
                position.at_tempo_frames(tempo(120), rate(hz)).unwrap(),
                frame
            );
        }
    }
}

#[test]
fn performance_host_anchor_handles_before_anchor_stale_stream_and_overflow() {
    let anchor = ClockAnchor {
        epoch: StreamEpoch(7),
        host_nanos: 10_000,
        take_time: time(1_000),
    };
    assert_eq!(
        anchor.original_time(StreamEpoch(7), 9_500).unwrap().nanos(),
        500
    );
    assert_eq!(
        anchor
            .original_time(StreamEpoch(7), 10_500)
            .unwrap()
            .nanos(),
        1_500
    );
    assert_eq!(
        anchor.original_time(StreamEpoch(7), 8_999),
        Err(PerformanceError::BeforeTake)
    );
    assert_eq!(
        anchor.original_time(StreamEpoch(8), 10_500),
        Err(PerformanceError::StaleEpoch)
    );
    let overflow = ClockAnchor {
        epoch: StreamEpoch(8),
        host_nanos: 0,
        take_time: time(u64::MAX),
    };
    assert_eq!(
        overflow.original_time(StreamEpoch(8), 1),
        Err(PerformanceError::Overflow)
    );
}

#[test]
fn performance_pure_block_offsets_do_not_depend_on_callback_size() {
    let events = [0, 1, 30, 31, 32, 63, 64, 255, 256, 511, 512, 2048];
    for block_size in [31u32, 64, 256] {
        let mut placed = Vec::new();
        for start in (0..2304).step_by(block_size as usize) {
            for event in events {
                match block_placement(event, start, NonZeroU32::new(block_size).unwrap()) {
                    BlockPlacement::Offset(offset) => placed.push(start + u64::from(offset)),
                    BlockPlacement::Late { .. } | BlockPlacement::Future => {}
                }
            }
        }
        assert_eq!(placed, events);
    }
    assert_eq!(
        block_placement(99, 100, NonZeroU32::new(31).unwrap()),
        BlockPlacement::Late { frames: 1 }
    );
    assert_eq!(
        block_placement(131, 100, NonZeroU32::new(31).unwrap()),
        BlockPlacement::Future
    );
    assert_eq!(
        block_placement(u64::MAX, u64::MAX - 1, NonZeroU32::new(31).unwrap()),
        BlockPlacement::Offset(1)
    );
}

#[test]
fn performance_admission_laws_define_late_horizon_and_full_without_queue_framework() {
    for hz in [44_100, 48_000] {
        let rate = rate(hz);
        let horizon = u64::from(hz) * LOOKAHEAD_SECONDS;
        assert_eq!(admission(100, 100, 0, rate), Admission::Accepted);
        assert_eq!(admission(99, 100, 0, rate), Admission::Late { frames: 1 });
        assert_eq!(
            admission(100 + horizon, 100, 255, rate),
            Admission::Accepted
        );
        assert_eq!(admission(101 + horizon, 100, 0, rate), Admission::TooFuture);
        assert_eq!(
            admission(100, 100, SCHEDULE_CAPACITY, rate),
            Admission::Full
        );
        assert_eq!(admission(99, 100, SCHEDULE_CAPACITY, rate), Admission::Full);
    }
}

#[test]
fn performance_semantic_targets_input_bounds_ties_and_extreme_time_restoration() {
    assert_eq!(AssetRef::of(b"a").id, AssetId::of(b"a"));
    assert_ne!(
        Target::Sample(AssetId::of(b"a")),
        Target::Instrument(InstrumentId(1))
    );
    assert_eq!(Channel::try_from(15).unwrap().value(), 15);
    assert_eq!(Pitch::try_from(127).unwrap().value(), 127);
    assert_eq!(Velocity::try_from(1).unwrap().value(), 1);
    assert_eq!(Channel::try_from(16), Err(PerformanceError::InvalidChannel));
    assert_eq!(Pitch::try_from(128), Err(PerformanceError::InvalidPitch));
    assert_eq!(
        Velocity::try_from(0),
        Err(PerformanceError::InvalidVelocity)
    );
    assert_eq!(Tempo::try_from(0), Err(PerformanceError::ZeroTempo));
    assert_eq!(
        SampleRate::try_from(0),
        Err(PerformanceError::ZeroSampleRate)
    );
    let mut timing = TakeTiming::new(TakeId(1), tempo(120));
    timing.set_quantization(Quantization::Sixteenth);
    assert_eq!(
        timing
            .view(&completed(1, 62_500_000, 62_500_001))
            .unwrap()
            .start
            .at_tempo_nanos(tempo(120))
            .unwrap(),
        125_000_000
    );
    timing.set_quantization(Quantization::Off);
    let extreme = completed(2, u64::MAX - 1, u64::MAX);
    assert_eq!(
        timing
            .view(&extreme)
            .unwrap()
            .start
            .at_tempo_nanos(tempo(120))
            .unwrap(),
        u64::MAX - 1
    );
    assert_eq!(
        timing
            .view(&extreme)
            .unwrap()
            .end
            .at_tempo_nanos(tempo(120))
            .unwrap(),
        u64::MAX
    );
    assert_eq!(
        MusicalPosition::from_original(time(u64::MAX), tempo(120))
            .at_tempo_nanos(Tempo::try_from(1).unwrap()),
        Err(PerformanceError::Overflow)
    );
}

#[test]
fn performance_fractional_grid_and_fractional_tempo_keep_integer_originals() {
    // At 116 BPM a sixteenth is exactly 15e12/116000 ns,
    // or 5702 + 17/29 frames at 44100 and 6206 + 26/29 at 48000.
    let note = completed(1, 120_000_000, 130_000_000);
    let mut timing = TakeTiming::new(TakeId(1), tempo(116));
    timing.set_quantization(Quantization::Sixteenth);
    let view = timing.view(&note).unwrap();
    assert_eq!(view.start.at_tempo_nanos(tempo(116)).unwrap(), 129_310_345);
    assert_eq!(
        view.start
            .at_tempo_frames(tempo(116), rate(44_100))
            .unwrap(),
        5703
    );
    assert_eq!(
        view.start
            .at_tempo_frames(tempo(116), rate(48_000))
            .unwrap(),
        6207
    );
    timing.set_quantization(Quantization::Off);
    assert_eq!(
        timing
            .view(&note)
            .unwrap()
            .start
            .at_tempo_nanos(tempo(116))
            .unwrap(),
        120_000_000
    );
    let fractional = Tempo::try_from(123_456).unwrap();
    let half = Tempo::try_from(61_728).unwrap();
    let position = MusicalPosition::from_original(time(123_456_789), fractional);
    assert_eq!(position.at_tempo_nanos(half).unwrap(), 246_913_578);
    assert_eq!(position.at_tempo_nanos(fractional).unwrap(), 123_456_789);
}
