#![allow(unsafe_code)]
use brownnote_native::engine::{Command, Meters, Mixer, QUEUE_CAPACITY, SLOTS, Sample, Slots};
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
