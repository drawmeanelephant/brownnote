#![allow(clippy::unwrap_used)]
use brownnote_native::{
    engine::{Command, Meters, Mixer, Returned, SLOTS, Sample, Slots},
    output::{Output, PlaybackClock, PlaybackScheduleClock},
    performance::{
        Channel, EventId, LoopBars, MusicalPosition, NoteKey, NoteLifecycle, NoteOn, OriginalTime,
        PerformanceError, Pitch, Quantization, SCHEDULE_CAPACITY, SourceId, StreamEpoch, TakeId,
        TakeTiming, Target, Tempo, Velocity,
        scheduler::{ScheduleOutcome, ScheduleSender, ScheduledAction, ScheduledEvent, Trigger},
    },
    project::PadId,
};
use cpal::{OutputStreamTimestamp, StreamInstant};
use std::{num::NonZeroU32, sync::Arc};

fn time(ns: u64) -> OriginalTime {
    OriginalTime::from_nanos(ns)
}
fn id(sequence: u64) -> EventId {
    EventId {
        take: TakeId(1),
        sequence,
    }
}
fn trigger(slot: usize) -> Trigger {
    Trigger {
        pad: slot,
        slot,
        group: slot,
        velocity: 1.0,
        gain: 1.0,
        pan: -1.0,
        semitones: 0.0,
    }
}
fn event(sender: &ScheduleSender, frame: u64, sequence: u64) -> ScheduledEvent {
    ScheduledEvent {
        epoch: sender.epoch(),
        frame,
        original_time: time(123_456_789),
        action: ScheduledAction::NoteOn {
            id: id(sequence),
            trigger: trigger(0),
        },
    }
}
fn rig(
    rate: u32,
    samples: Vec<Vec<f32>>,
) -> (Mixer, ScheduleSender, Arc<Meters>, rtrb::Consumer<Returned>) {
    let meters = Arc::new(Meters::default());
    let (retired, returns) = rtrb::RingBuffer::new(SLOTS);
    let slots = Slots::from_samples(samples.into_iter().map(|data| Sample {
        data: data.into(),
        rate,
    }));
    let mut mixer = Mixer::new(slots, rate, meters.clone(), retired);
    let sender = mixer.take_scheduler().unwrap();
    mixer.command(Command::SetChoke(false));
    (mixer, sender, meters, returns)
}
fn render(mixer: &mut Mixer, frames: usize, block: usize) -> Vec<f32> {
    let mut result = vec![0.0; frames * 2];
    for chunk in result.chunks_mut(block * 2) {
        mixer.render(chunk, 2);
    }
    result
}
fn hits(audio: &[f32]) -> Vec<u64> {
    audio
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            assert_eq!(f[1], 0.0);
            if f[0] == 0.0 {
                None
            } else {
                assert!((f[0] - 0.2).abs() < 1e-7);
                Some(i as u64)
            }
        })
        .collect()
}
fn observation(rate: u32, first_frame: usize, take_ns: u64) -> PlaybackScheduleClock {
    let (mut clock, mut observations) = PlaybackClock::new(NonZeroU32::new(rate).unwrap());
    let timestamp = OutputStreamTimestamp {
        callback: StreamInstant::from_nanos(4_990_000_000),
        playback: StreamInstant::from_nanos(5_000_000_000),
    };
    if first_frame != 0 {
        clock.publish(first_frame, timestamp);
        observations.take_latest();
    }
    clock.publish(64, timestamp);
    PlaybackScheduleClock {
        observation: observations.take_latest().unwrap(),
        take_time_at_playback: time(take_ns),
    }
}

#[test]
fn performance_scheduled_audio_has_identical_boundary_positions_at_all_buffers_and_rates() {
    let positions = [0, 1, 30, 31, 32, 63, 64, 65, 255, 256, 257, 511, 512, 2048];
    for rate in [44_100, 48_000] {
        for block in [31, 64, 256] {
            let (mut mixer, mut sender, _, _returns) = rig(rate, vec![vec![0.25]]);
            // Deliberately reverse deadlines: ingress must not head-of-line block.
            for (sequence, &frame) in positions.iter().rev().enumerate() {
                let e = event(&sender, frame, sequence as u64);
                assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
            }
            assert_eq!(hits(&render(&mut mixer, 2100, block)), positions);
            let c = sender.counters();
            assert_eq!(c.accepted, positions.len() as u64);
            assert_eq!(c.dispatched, c.accepted);
            assert_eq!((c.late, c.unavailable, c.outstanding), (0, 0, 0));
        }
    }
}

#[test]
fn performance_original_clock_fixture_is_measured_in_rendered_samples_not_scheduler_math() {
    for rate in [44_100, 48_000] {
        for block in [31, 64, 256] {
            let mapping = observation(rate, 0, 0);
            let epoch = mapping.observation.stream_id.epoch();
            let meters = Arc::new(Meters::default());
            let (retired, _returns) = rtrb::RingBuffer::new(SLOTS);
            let mut mixer = Mixer::new_with_epoch(
                Slots::from_samples([Sample {
                    data: Arc::from([0.25]),
                    rate,
                }]),
                rate,
                meters,
                retired,
                epoch,
            );
            let mut sender = mixer.take_scheduler().unwrap();
            let mut expected = Vec::new();
            for (i, line) in include_str!("fixtures/performance-clocks.csv")
                .lines()
                .filter(|line| !line.starts_with('#'))
                .enumerate()
            {
                let values: Vec<u64> = line.split(',').map(|v| v.parse().unwrap()).collect();
                let original = time(values[0]);
                let frame = mapping.frame_for_original(epoch, original).unwrap();
                let host = StreamInstant::from_nanos(5_000_000_000 + values[0]);
                assert_eq!(mapping.original_time(epoch, host).unwrap(), original);
                assert_eq!(mapping.frame_for_host(epoch, host).unwrap(), frame);
                let mut e = event(&sender, frame, i as u64);
                e.original_time = original;
                assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
                expected.push(values[if rate == 44_100 { 1 } else { 2 }]);
                assert_eq!(original.nanos(), values[0]);
            }
            assert_eq!(
                hits(&render(&mut mixer, rate as usize * 2 + 1, block)),
                expected
            );
            assert_eq!(sender.counters().late, 0);
        }
    }
}

#[test]
fn performance_late_future_and_total_capacity_have_distinct_results_and_counters() {
    for rate in [44_100, 48_000] {
        let (mut mixer, mut sender, _, _returns) = rig(rate, vec![vec![0.25]]);
        render(&mut mixer, 31, 31);
        let late = event(&sender, 1, 0);
        assert_eq!(sender.try_schedule(late).outcome, ScheduleOutcome::Accepted);
        assert_eq!(hits(&render(&mut mixer, 1, 31)), [0]);
        assert_eq!(
            (sender.counters().late, sender.counters().late_frames),
            (1, 30)
        );
        assert_eq!(late.original_time.nanos(), 123_456_789);
        let horizon = sender.frame() + u64::from(rate) * 2;
        let too_future = event(&sender, horizon + 1, 1);
        assert_eq!(
            sender.try_schedule(too_future).outcome,
            ScheduleOutcome::TooFuture
        );
        // A producer retains/retries future input, without touching originals.
        render(&mut mixer, 1, 31);
        assert_eq!(
            sender.try_schedule(too_future).outcome,
            ScheduleOutcome::Accepted
        );
        for i in 1..SCHEDULE_CAPACITY {
            let e = event(&sender, horizon, i as u64 + 1);
            assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
        }
        // Drain ingress into pending. This must not create a second capacity.
        assert!(hits(&render(&mut mixer, 31, 31)).is_empty());
        let e = event(&sender, horizon, 999);
        assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Full);
        let c = sender.counters();
        assert_eq!((c.accepted, c.full, c.too_future), (257, 1, 1));
        assert_eq!((c.dispatched, c.outstanding), (1, 256));
        render(&mut mixer, rate as usize * 2 + 2, 64);
        let c = sender.counters();
        assert_eq!((c.dispatched, c.outstanding, c.late), (257, 0, 1));
    }
}

#[test]
fn performance_rejected_release_panics_once_cancels_pending_and_releases_sounding_notes() {
    for rate in [44_100, 48_000] {
        for rejected in [
            ScheduleOutcome::Full,
            ScheduleOutcome::TooFuture,
            ScheduleOutcome::StaleEpoch,
        ] {
            let (mut mixer, mut sender, meters, _returns) =
                rig(rate, vec![vec![0.1; rate as usize * 3]]);
            let e = event(&sender, 0, 0);
            sender.try_schedule(e);
            render(&mut mixer, 1, 31);
            assert_eq!(meters.counts()[0], 1);
            let n = if rejected == ScheduleOutcome::Full {
                256
            } else {
                1
            };
            for i in 0..n {
                let e = event(&sender, u64::from(rate), i + 1);
                assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
            }
            render(&mut mixer, 31, 31);
            let off = ScheduledEvent {
                epoch: if rejected == ScheduleOutcome::StaleEpoch {
                    StreamEpoch::fresh()
                } else {
                    sender.epoch()
                },
                frame: if rejected == ScheduleOutcome::TooFuture {
                    sender.frame() + u64::from(rate) * 2 + 1
                } else {
                    64
                },
                original_time: time(345_678),
                action: ScheduledAction::NoteOff { id: id(0) },
            };
            let result = sender.try_schedule(off);
            assert_eq!(result.outcome, rejected);
            assert!(result.release_recovery_requested);
            let audio = render(&mut mixer, rate as usize * 2, 64);
            assert_eq!(meters.counts(), [0; 16]);
            assert!(audio[rate as usize..].iter().all(|v| *v == 0.0));
            let c = sender.counters();
            assert_eq!((c.recovery_requests, c.recoveries, c.cancelled), (1, 1, n));
            assert_eq!((c.dispatched, c.outstanding), (1, 0));
        }
    }
}

#[test]
fn performance_fifo_note_off_targets_overlaps_at_the_exact_sample() {
    for rate in [44_100, 48_000] {
        for block in [31, 64, 256] {
            let (mut mixer, mut sender, _, _returns) =
                rig(rate, vec![vec![0.1; 2000], vec![0.2; 2000]]);
            let key = NoteKey {
                source: SourceId(1),
                channel: Channel::try_from(0).unwrap(),
                pitch: Pitch::try_from(60).unwrap(),
            };
            let mut lifecycle = NoteLifecycle::default();
            for (sequence, frame, slot) in [(0, 0, 0), (1, 64, 1)] {
                lifecycle
                    .note_on(NoteOn {
                        id: id(sequence),
                        target: Target::Pad(PadId::try_from(slot as u8).unwrap()),
                        key,
                        velocity: Velocity::try_from(127).unwrap(),
                        original_time: time(frame * 1_000),
                    })
                    .unwrap();
                let mut e = event(&sender, frame, sequence);
                e.action = ScheduledAction::NoteOn {
                    id: id(sequence),
                    trigger: trigger(slot),
                };
                assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
            }
            for (sequence, frame) in [(0, 128), (1, 512)] {
                let note = lifecycle.note_off(key, time(frame * 1_000)).unwrap();
                assert_eq!(note.on().id, id(sequence));
                let mut e = event(&sender, frame, sequence);
                e.action = ScheduledAction::NoteOff { id: note.on().id };
                assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
            }
            let audio = render(&mut mixer, 1024, block);
            let fade = rate.div_ceil(200) as usize;
            for (frame, pair) in audio.as_chunks::<2>().0.iter().enumerate() {
                let first = if frame < 128 {
                    0.1
                } else {
                    0.1 * (1.0 - (frame - 128) as f32 / fade as f32).max(0.0)
                };
                let second = if frame < 64 {
                    0.0
                } else if frame < 512 {
                    0.2
                } else {
                    0.2 * (1.0 - (frame - 512) as f32 / fade as f32).max(0.0)
                };
                assert!(
                    (pair[0] - (first + second) * 0.8).abs() < 1e-7,
                    "rate={rate} block={block} frame={frame}"
                );
            }
            assert_eq!(sender.counters().dispatched, 4);
        }
    }
}

#[test]
fn performance_equal_deadlines_preserve_submission_order() {
    for on_first in [true, false] {
        let (mut mixer, mut sender, meters, _returns) = rig(48_000, vec![vec![0.1; 4000]]);
        let on = event(&sender, 64, 0);
        let off = ScheduledEvent {
            action: ScheduledAction::NoteOff { id: id(0) },
            ..on
        };
        for e in if on_first { [on, off] } else { [off, on] } {
            assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
        }
        render(&mut mixer, 64 + 241, 31);
        assert_eq!(meters.counts()[0], if on_first { 0 } else { 1 });
        assert_eq!(sender.counters().dispatched, 2);
    }
}

#[test]
fn performance_loop_end_and_tempo_conversion_render_without_wrapping_held_note_off() {
    for rate in [44_100, 48_000] {
        for block in [31, 64, 256] {
            let (mut mixer, mut sender, _, _returns) = rig(rate, vec![vec![0.1; 4000]]);
            let tempo = Tempo::try_from(120_000).unwrap();
            let key = NoteKey {
                source: SourceId(1),
                channel: Channel::try_from(0).unwrap(),
                pitch: Pitch::try_from(60).unwrap(),
            };
            let mut lifecycle = NoteLifecycle::default();
            lifecycle
                .note_on(NoteOn {
                    id: id(0),
                    target: Target::Pad(PadId::try_from(0).unwrap()),
                    key,
                    velocity: Velocity::try_from(127).unwrap(),
                    original_time: time(1_999_000_000),
                })
                .unwrap();
            let note = lifecycle.note_off(key, time(2_009_000_000)).unwrap();
            let mut timing = TakeTiming::new(TakeId(1), tempo);
            timing.set_quantization(Quantization::Sixteenth);
            let view = timing.view(&note).unwrap();
            let split = view
                .start
                .split_loop(LoopBars::try_from(1).unwrap())
                .unwrap();
            assert_eq!(split.iteration, 1);
            assert_eq!(split.within.at_tempo_nanos(tempo).unwrap(), 0);
            let mapping = observation(rate, rate as usize * 2, 2_000_000_000);
            let epoch = mapping.observation.stream_id.epoch();
            let start = mapping
                .frame_for_position(epoch, view.start, tempo)
                .unwrap();
            let end = mapping.frame_for_position(epoch, view.end, tempo).unwrap();
            assert_eq!(start, u64::from(rate) * 2);
            assert_eq!(end, if rate == 44_100 { 88_641 } else { 96_480 });
            assert_eq!(
                mapping
                    .frame_for_position(epoch, view.start, Tempo::try_from(60_000).unwrap())
                    .unwrap(),
                u64::from(rate) * 4
            );
            assert_eq!(note.on().original_time.nanos(), 1_999_000_000);
            let on = event(&sender, start, 0);
            sender.try_schedule(on);
            let mut audio = render(&mut mixer, rate as usize, block);
            let off = ScheduledEvent {
                frame: end,
                action: ScheduledAction::NoteOff { id: id(0) },
                ..on
            };
            assert_eq!(sender.try_schedule(off).outcome, ScheduleOutcome::Accepted);
            audio.extend(render(&mut mixer, rate as usize + 1000, block));
            let fade = rate.div_ceil(200) as u64;
            for (f, pair) in audio.as_chunks::<2>().0.iter().enumerate() {
                let f = f as u64;
                // Sample is 0.1, unity velocity/pan, master 0.8.
                let expected = if f < start {
                    0.0
                } else if f < end {
                    0.08
                } else {
                    0.08 * (1.0 - (f - end) as f32 / fade as f32).max(0.0)
                };
                assert!((pair[0] - expected).abs() < 1e-7, "frame {f}");
            }
        }
    }
}

#[test]
fn performance_clock_checks_epochs_signed_offsets_rounding_and_reopen() {
    let mut old_epoch = StreamEpoch::fresh();
    for rate in [44_100, 48_000, 48_000] {
        let mapping = observation(rate, rate as usize, 1_000_000_000);
        let epoch = mapping.observation.stream_id.epoch();
        assert_ne!(epoch, old_epoch);
        assert_eq!(
            mapping.frame_for_host(old_epoch, StreamInstant::from_nanos(5_000_000_000)),
            Err(PerformanceError::StaleEpoch)
        );
        assert_eq!(
            mapping
                .original_time(epoch, StreamInstant::from_nanos(4_990_000_000))
                .unwrap()
                .nanos(),
            990_000_000
        );
        assert_eq!(
            mapping
                .frame_for_host(epoch, StreamInstant::from_nanos(4_000_000_000))
                .unwrap(),
            0
        );
        assert_eq!(
            mapping.original_time(epoch, StreamInstant::from_nanos(3_999_999_999)),
            Err(PerformanceError::BeforeTake)
        );
        assert_eq!(
            mapping.frame_for_host(epoch, StreamInstant::from_nanos(3_999_999_999)),
            Err(PerformanceError::BeforeStream)
        );
        let origin = observation(rate, 0, 0);
        let new_epoch = origin.observation.stream_id.epoch();
        let (retired, _returns) = rtrb::RingBuffer::new(SLOTS);
        let mut mixer = Mixer::new_with_epoch(
            Slots::from_samples([Sample {
                data: Arc::from([0.25]),
                rate,
            }]),
            rate,
            Arc::new(Meters::default()),
            retired,
            new_epoch,
        );
        let mut sender = mixer.take_scheduler().unwrap();
        let original = time(125_000_000);
        let frame = origin.frame_for_original(new_epoch, original).unwrap();
        assert_eq!(frame, if rate == 44_100 { 5513 } else { 6000 });
        let e = ScheduledEvent {
            original_time: original,
            ..event(&sender, frame, 0)
        };
        assert_eq!(
            sender
                .try_schedule(ScheduledEvent {
                    epoch: old_epoch,
                    ..e
                })
                .outcome,
            ScheduleOutcome::StaleEpoch
        );
        assert_eq!(sender.try_schedule(e).outcome, ScheduleOutcome::Accepted);
        assert_eq!(hits(&render(&mut mixer, 6100, 31)), [frame]);
        let pending = event(&sender, 6200, 1);
        sender.try_schedule(pending);
        drop(mixer);
        assert_eq!(
            sender.try_schedule(e).outcome,
            ScheduleOutcome::Disconnected
        );
        assert_eq!(
            (sender.counters().cancelled, sender.counters().outstanding),
            (1, 0)
        );
        old_epoch = new_epoch;
    }
    let mapping = observation(48_000, 0, 0);
    let epoch = mapping.observation.stream_id.epoch();
    // 31,250ns is exactly 1.5 frames. Ties go later.
    assert_eq!(mapping.frame_for_original(epoch, time(31_250)).unwrap(), 2);
    // 31,249.6ns is below 1.5 frames, though rounded ns would hit the tie.
    let tempo = Tempo::try_from(312_496).unwrap();
    let position = MusicalPosition::from_original(time(31_249), tempo);
    assert_eq!(
        mapping
            .frame_for_position(epoch, position, Tempo::try_from(312_490).unwrap())
            .unwrap(),
        1
    );
    let mut overflow = mapping;
    overflow.take_time_at_playback = time(u64::MAX);
    assert_eq!(
        overflow.original_time(epoch, StreamInstant::from_nanos(5_000_000_001)),
        Err(PerformanceError::Overflow)
    );
    assert_eq!(
        overflow.frame_for_original(epoch, time(0)),
        Err(PerformanceError::BeforeStream)
    );
    overflow.observation.first_frame = u64::MAX;
    overflow.take_time_at_playback = time(0);
    assert_eq!(
        overflow.frame_for_original(epoch, time(1_000_000_000)),
        Err(PerformanceError::Overflow)
    );
}

#[test]
fn performance_output_public_path_keeps_immediate_calls_and_silent_reopen_honest() {
    let slots = || {
        Slots::from_samples([Sample {
            data: Arc::from([0.25; 1000]),
            rate: 44_100,
        }])
    };
    let mut output = Output::virtual_driver(slots(), "test".into(), false);
    assert!(output.opened().is_none());
    assert!(output.timestamps().is_none());
    let e = event(output.scheduler(), 31, 0);
    assert_eq!(output.schedule(e).outcome, ScheduleOutcome::Accepted);
    output.advance(0.001);
    assert_eq!(output.scheduler().counters().dispatched, 1);
    assert_eq!(output.meters.counts()[0], 1);
    assert!(
        f32::from_bits(
            output
                .meters
                .peak
                .load(std::sync::atomic::Ordering::Relaxed)
        ) > 0.0
    );
    assert!(output.send(Command::Panic));
    output.advance(0.01);
    assert_eq!(output.meters.counts(), [0; 16]);
    let settings = trigger(0);
    assert!(output.send(Command::Trigger {
        pad: settings.pad,
        slot: settings.slot,
        group: settings.group,
        velocity: settings.velocity,
        gain: settings.gain,
        pan: settings.pan,
        semitones: settings.semitones
    }));
    output.advance(0.001);
    assert_eq!(output.meters.counts()[0], 1);
    let mut reopened = Output::virtual_driver(slots(), "reopen".into(), true);
    assert_ne!(reopened.scheduler().epoch(), output.scheduler().epoch());
    assert_eq!(reopened.schedule(e).outcome, ScheduleOutcome::StaleEpoch);
    assert_eq!(reopened.meters.counts(), [0; 16]);
}

#[test]
fn performance_unload_cancels_pending_and_ingress_without_replaying_reused_slots() {
    for drain_first in [false, true] {
        let (mut mixer, mut sender, _, mut returns) = rig(48_000, vec![vec![0.25]]);
        let e = event(&sender, 128, 0);
        sender.try_schedule(e);
        if drain_first {
            render(&mut mixer, 31, 31);
        }
        mixer.command(Command::Unload { slot: 0 });
        render(&mut mixer, 1, 31);
        assert!(matches!(
            returns.pop().unwrap(),
            Returned::Retired { slot: 0, .. }
        ));
        mixer.command(Command::Load {
            slot: 0,
            sample: Sample {
                data: Arc::from([0.5]),
                rate: 48_000,
            },
        });
        assert!(render(&mut mixer, 256, 31).iter().all(|v| *v == 0.0));
        assert_eq!(
            (sender.counters().cancelled, sender.counters().outstanding),
            (1, 0)
        );
        assert_eq!(sender.counters().dispatched, 0);
    }
}
