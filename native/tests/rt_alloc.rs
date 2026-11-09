#![allow(unsafe_code)]
use brownnote_native::engine::{
    Command, Meters, Mixer, QUEUE_CAPACITY, Returned, SLOTS, STEAL_TAILS, Sample, Slots, VOICES,
};
use brownnote_native::performance::{
    EventId, OriginalTime, TakeId,
    scheduler::{ScheduleOutcome, ScheduledAction, ScheduledEvent, Trigger},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::VecDeque,
    sync::Arc,
};

struct Counting;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
}

fn count(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    let _ = TRACK.try_with(|track| {
        if track.get() {
            let _ = counter.try_with(|n| n.set(n.get() + 1));
        }
    });
}

// SAFETY: every allocation is forwarded unchanged to System; TLS only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(&FREES);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn trigger(pad: usize, slot: usize, group: usize) -> Command {
    Command::Trigger {
        pad,
        slot,
        group,
        velocity: 0.5,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    }
}

#[test]
fn scheduled_callback_saturation_releases_and_retirement_allocate_and_free_nothing() {
    for rate in [44_100, 48_000] {
        for block in [31, 64, 256] {
            let meters = Arc::new(Meters::default());
            let (retired, mut returns) = rtrb::RingBuffer::new(1);
            let mut mixer = Mixer::new(
                Slots::from_samples([Sample {
                    data: Arc::from(vec![0.001; rate as usize * 3]),
                    rate,
                }]),
                rate,
                meters.clone(),
                retired,
            );
            let mut sender = mixer.take_scheduler().unwrap_or_else(|| unreachable!());
            let epoch = sender.epoch();
            let id = |sequence| EventId {
                take: TakeId(1),
                sequence,
            };
            let on = |frame, sequence| ScheduledEvent {
                epoch,
                frame,
                original_time: OriginalTime::from_nanos(123_456_789),
                action: ScheduledAction::NoteOn {
                    id: id(sequence),
                    trigger: Trigger {
                        pad: 0,
                        slot: 0,
                        group: 0,
                        velocity: 1.0,
                        gain: 1.0,
                        pan: -1.0,
                        semitones: 0.0,
                    },
                },
            };
            let mut output = [0.0; 512];
            ALLOCS.set(0);
            FREES.set(0);
            TRACK.set(true);
            mixer.command(Command::SetChoke(false));
            for i in (0..256).rev() {
                let result = sender.try_schedule(on(i, i));
                assert_eq!(result.outcome, ScheduleOutcome::Accepted);
            }
            let mut frames = 0;
            while frames < 512 {
                mixer.render(&mut output[..block * 2], 2);
                frames += block;
            }
            let off = ScheduledEvent {
                action: ScheduledAction::NoteOff { id: id(255) },
                ..on(frames as u64, 255)
            };
            assert_eq!(sender.try_schedule(off).outcome, ScheduleOutcome::Accepted);
            mixer.render(&mut output[..block * 2], 2);
            for i in 0..256 {
                let result = sender.try_schedule(on(sender.frame() + u64::from(rate) / 2, 256 + i));
                assert_eq!(result.outcome, ScheduleOutcome::Accepted);
            }
            let result = sender.try_schedule(off);
            assert_eq!(result.outcome, ScheduleOutcome::Full);
            assert!(result.release_recovery_requested);
            mixer.render(&mut output[..block * 2], 2);
            mixer.command(Command::Unload { slot: 0 });
            for _ in 0..(rate as usize * 2).div_ceil(block) {
                mixer.render(&mut output[..block * 2], 2);
            }
            TRACK.set(false);
            assert_eq!(
                ALLOCS.get(),
                0,
                "scheduled callback allocated at {rate}/{block}"
            );
            assert_eq!(FREES.get(), 0, "scheduled callback freed at {rate}/{block}");
            assert_eq!(meters.counts(), [0; 16]);
            let c = sender.counters();
            assert_eq!((c.accepted, c.dispatched, c.cancelled), (513, 257, 256));
            assert_eq!((c.full, c.recoveries, c.outstanding), (1, 1, 0));
            assert!(matches!(
                returns.pop(),
                Ok(Returned::Retired { slot: 0, .. })
            ));
        }
    }
}

#[test]
fn callback_path_allocates_and_frees_nothing_for_two_seconds_at_both_rates() {
    for rate in [44_100, 48_000] {
        let meters = Arc::new(Meters::default());
        // Enough samples for the initial slots, a full set of loads, and
        // rejected loads that go to the overflow.
        let samples: Vec<Sample> = (0..SLOTS + 16)
            .map(|i| Sample {
                data: Arc::from(vec![0.1; 64 + i]),
                rate,
            })
            .collect();
        let (mut send, mut recv) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let (retired, mut returns) = rtrb::RingBuffer::new(SLOTS);
        let mut mixer = Mixer::new(
            Slots::from_samples(samples[..8].iter().cloned()),
            rate,
            meters,
            retired,
        );
        let mut output = [0.0; 512];

        // Round one: load free slots, trigger them, unload while they play,
        // reject loads into occupied slots, and unload enough to fill the
        // return queue exactly.
        let mut first: VecDeque<Command> = VecDeque::new();
        first.push_back(Command::SetMaster(0.9));
        first.push_back(Command::SetChoke(true));
        for (slot, sample) in samples.iter().enumerate().take(SLOTS).skip(8) {
            first.push_back(Command::Load {
                slot,
                sample: sample.clone(),
            });
        }
        for slot in 8..16 {
            first.push_back(trigger(slot % 16, slot, slot % 5));
        }
        for slot in 8..16 {
            first.push_back(Command::Unload { slot });
        }
        for slot in 0..8 {
            first.push_back(Command::Load {
                slot,
                sample: samples[SLOTS + slot].clone(),
            });
        }
        for slot in 16..SLOTS {
            first.push_back(Command::Unload { slot });
        }
        for slot in 8..12 {
            first.push_back(trigger(slot % 16, slot, slot % 5));
        }
        // Round two: the return queue is full, so a rejected load has to wait
        // in the overflow, and an unload has to stay retiring.
        let mut second: VecDeque<Command> = VecDeque::new();
        second.push_back(Command::Panic);
        second.push_back(Command::Load {
            slot: 8,
            sample: samples[SLOTS + 8].clone(),
        });
        second.push_back(Command::Load {
            slot: 8,
            sample: samples[SLOTS + 9].clone(),
        });
        second.push_back(trigger(0, 8, 1));
        second.push_back(Command::Unload { slot: 0 });
        second.push_back(trigger(1, 0, 2));

        ALLOCS.set(0);
        FREES.set(0);
        TRACK.set(true);
        while let Some(command) = first.pop_front() {
            let _ = send.push(command);
        }
        mixer.drain(&mut recv);
        mixer.render(&mut output, 2);
        while let Some(command) = second.pop_front() {
            let _ = send.push(command);
        }
        mixer.drain(&mut recv);
        mixer.render(&mut output, 2);
        let mut left = rate as usize * 2 - 1024;
        while left > 0 {
            let frames = left.min(256);
            mixer.render(&mut output[..frames * 2], 2);
            left -= frames;
        }
        TRACK.set(false);
        assert_eq!(ALLOCS.get(), 0, "callback allocated at {rate}");
        assert_eq!(FREES.get(), 0, "callback freed memory at {rate}");

        // The full queue held every sample it was given; the overflow and the
        // delayed retirement come back once there is room.
        let mut returned = Vec::new();
        while let Ok(item) = returns.pop() {
            returned.push(item);
        }
        assert_eq!(returned.len(), SLOTS, "queue at {rate}");
        mixer.render(&mut output[..2], 2);
        while let Ok(sample) = returns.pop() {
            returned.push(sample);
        }
        assert_eq!(returned.len(), SLOTS + 2, "overflow or retirement lost");
    }
}

#[test]
fn callback_repeated_steals_saturated_tails_and_retirement_allocate_and_free_nothing() {
    for rate in [44_100, 48_000] {
        const WAVES: usize = 6;
        let meters = Arc::new(Meters::default());
        let (mut send, mut recv) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        // Deliberately tiny: retirements must wait while rejected loads occupy
        // this queue and the preallocated overflow.
        let (retired, mut returns) = rtrb::RingBuffer::new(1);
        let owned_sample = || Sample {
            data: Arc::from(vec![0.01; rate as usize * 3]),
            rate,
        };
        // No spare strong references: dropping a sample in the callback really
        // would free it, so the allocator can detect an ownership regression.
        let mut mixer = Mixer::new(
            Slots::from_samples([owned_sample()]),
            rate,
            meters.clone(),
            retired,
        );
        let mut waves: Vec<VecDeque<Command>> = (0..WAVES)
            .map(|wave| {
                let mut commands = VecDeque::new();
                commands.push_back(Command::Load {
                    slot: wave + 1,
                    sample: owned_sample(),
                });
                for i in 0..VOICES {
                    commands.push_back(trigger(i % 16, wave + 1, wave % 2));
                }
                commands.push_back(Command::Unload { slot: wave });
                commands.push_back(Command::Load {
                    slot: wave,
                    sample: owned_sample(),
                });
                commands
            })
            .collect();
        let mut output = [0.0; 512];
        let mut returned = Vec::with_capacity(WAVES * 2 + 1);
        let fade = (f64::from(rate) / 200.0).ceil() as usize;

        ALLOCS.set(0);
        FREES.set(0);
        TRACK.set(true);
        let _ = send.push(Command::SetChoke(false));
        for i in 0..VOICES {
            let _ = send.push(trigger(i % 16, 0, 0));
        }
        mixer.drain(&mut recv);
        mixer.render(&mut output, 2);
        let mut saturated_count = 0;
        let mut tail_signal_count = 0;
        for (wave, commands) in waves.iter_mut().enumerate() {
            // Render between onsets so tails are nonzero and partly faded,
            // not just unused voices that were replaced before ever sounding.
            while let Some(command) = commands.pop_front() {
                let is_trigger = matches!(command, Command::Trigger { .. });
                let _ = send.push(command);
                mixer.drain(&mut recv);
                if is_trigger {
                    mixer.render(&mut output[..2], 2);
                }
            }
            // All active voices now reference the new slot, but the most
            // recent eight tails still read the retiring previous slot.
            saturated_count += usize::from(meters.counts().iter().sum::<u32>() == VOICES as u32);
            mixer.render(&mut output[..2], 2);
            // The trigger helper uses half velocity, master starts at 0.8,
            // and center pan contributes 1/sqrt(2) to each channel.
            let active_plus_one =
                0.01 * 0.5 * (VOICES + 1) as f32 * std::f32::consts::FRAC_1_SQRT_2 * 0.8;
            tail_signal_count += usize::from(output[0] > active_plus_one);
            if wave % 2 == 0 {
                let _ = send.push(Command::SetChoke(true));
                let _ = send.push(trigger(15, wave + 1, wave % 2));
                let _ = send.push(Command::SetChoke(false));
            } else {
                let _ = send.push(Command::Panic);
            }
            mixer.drain(&mut recv);
            mixer.render(&mut output[..fade * 2], 2);
            // Refill after panic/choke, then saturate tails again before the
            // next wave. All work remains inside the counted callback path.
            for i in 0..VOICES + STEAL_TAILS + 3 {
                let _ = send.push(trigger(i % 16, wave + 1, wave % 2));
            }
            mixer.drain(&mut recv);
            mixer.render(&mut output[..2], 2);
        }
        let _ = send.push(Command::Panic);
        let _ = send.push(Command::Unload { slot: WAVES });
        mixer.drain(&mut recv);
        for _ in 0..rate.div_ceil(256) {
            mixer.render(&mut output, 2);
        }
        TRACK.set(false);
        assert_eq!(
            saturated_count, WAVES,
            "did not exercise every full active pool"
        );
        assert_eq!(
            tail_signal_count, WAVES,
            "did not render every wave's old tails"
        );
        assert_eq!(meters.counts(), [0; 16]);
        assert!(output.iter().all(|v| *v == 0.0));

        // UI receives/drops samples off the counted thread path. Flush the
        // overflow and delayed retirements with allocation counting still on.
        for _ in 0..WAVES * 2 + 2 {
            while let Ok(item) = returns.pop() {
                returned.push(item);
            }
            TRACK.set(true);
            mixer.render(&mut output[..2], 2);
            TRACK.set(false);
        }
        assert_eq!(ALLOCS.get(), 0, "stealing callback allocated at {rate}");
        assert_eq!(FREES.get(), 0, "stealing callback freed memory at {rate}");
        assert_eq!(returned.len(), WAVES * 2 + 1);
        for slot in 0..=WAVES {
            assert_eq!(
                returned
                    .iter()
                    .filter(
                        |item| matches!(item, Returned::Retired { slot: got, .. } if *got == slot)
                    )
                    .count(),
                1,
                "slot {slot} did not retire exactly once at {rate}"
            );
        }
        assert_eq!(
            returned
                .iter()
                .filter(|item| matches!(item, Returned::Rejected(_)))
                .count(),
            WAVES
        );
    }
}

/// The mounted sampled instrument (D18) on the callback: mounting, live and
/// scheduled held notes past every capacity, releases, panic, a remount and an
/// unmount neither allocate nor free; replaced players leave by the return queue.
#[test]
#[allow(clippy::unwrap_used)]
fn mounted_instrument_callback_allocates_and_frees_nothing() {
    const SF2: &[u8] = include_bytes!("../docs/spikes/sampled-instrument/fixtures/mechanics.sf2");
    let font = brownnote_native::instrument::Font::prepare(SF2, None).unwrap();
    for rate in [44_100, 48_000] {
        for block in [31, 64, 256] {
            let meters = Arc::new(Meters::default());
            let (retired, mut returns) = rtrb::RingBuffer::new(4);
            let mut mixer = Mixer::new(
                Slots::from_samples([Sample {
                    data: Arc::from(vec![0.0; 16]),
                    rate,
                }]),
                rate,
                meters.clone(),
                retired,
            );
            let mut sender = mixer.take_scheduler().unwrap_or_else(|| unreachable!());
            let epoch = sender.epoch();
            let first = font.player(rate, 0.5).unwrap();
            let second = font.player(rate, 0.5).unwrap();
            let id = |sequence| EventId {
                take: TakeId(3),
                sequence,
            };
            let mut output = [0.0; 512];
            let mut render = |mixer: &mut Mixer, frames: usize| {
                for _ in 0..frames.div_ceil(block) {
                    mixer.render(&mut output[..block * 2], 2);
                }
            };
            ALLOCS.set(0);
            FREES.set(0);
            TRACK.set(true);
            mixer.command(Command::Instrument(first));
            // More notes than voices and held slots, many on one pitch.
            for sequence in 0..200 {
                mixer.command(Command::HeldOn {
                    id: id(sequence),
                    pitch: 40 + (sequence % 3) as u8,
                    velocity: 100,
                });
                render(&mut mixer, block);
            }
            let peak = f32::from_bits(meters.peak.swap(0, std::sync::atomic::Ordering::Relaxed));
            for sequence in (0..200).step_by(2) {
                mixer.command(Command::HeldOff { id: id(sequence) });
            }
            render(&mut mixer, rate as usize / 10);
            for i in 0..64 {
                let at = sender.frame() + 100 + i;
                let on = ScheduledEvent {
                    epoch,
                    frame: at,
                    original_time: OriginalTime::from_nanos(0),
                    action: ScheduledAction::HeldOn {
                        id: id(1_000 + i),
                        pitch: 60,
                        velocity: 90,
                    },
                };
                assert_eq!(sender.try_schedule(on).outcome, ScheduleOutcome::Accepted);
                let off = ScheduledEvent {
                    frame: at + u64::from(rate) / 20,
                    action: ScheduledAction::NoteOff { id: id(1_000 + i) },
                    ..on
                };
                assert_eq!(sender.try_schedule(off).outcome, ScheduleOutcome::Accepted);
            }
            render(&mut mixer, rate as usize / 5);
            mixer.command(Command::Panic);
            render(&mut mixer, rate as usize / 10);
            mixer.command(Command::Instrument(second));
            mixer.command(Command::HeldOn {
                id: id(5_000),
                pitch: 72,
                velocity: 127,
            });
            render(&mut mixer, rate as usize / 10);
            mixer.command(Command::UnmountInstrument);
            render(&mut mixer, rate as usize);
            TRACK.set(false);
            assert_eq!(
                ALLOCS.get(),
                0,
                "instrument callback allocated at {rate}/{block}"
            );
            assert_eq!(
                FREES.get(),
                0,
                "instrument callback freed at {rate}/{block}"
            );
            assert!(peak > 0.0, "held notes sounded at {rate}/{block}");
            let c = sender.counters();
            assert_eq!((c.dispatched, c.outstanding), (128, 0));
            let mut players = 0;
            while let Ok(item) = returns.pop() {
                assert!(matches!(item, Returned::Instrument(_)));
                players += 1;
            }
            assert_eq!(
                players, 2,
                "both players returned, none dropped on the callback"
            );
        }
    }
}
