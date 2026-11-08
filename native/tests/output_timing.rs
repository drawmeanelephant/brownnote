#![allow(unsafe_code, clippy::unwrap_used)]

use brownnote_native::output::PlaybackClock;
use cpal::{OutputStreamTimestamp, StreamInstant};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    num::NonZeroU32,
};

struct Counting;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: all allocation operations are forwarded unchanged to System.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK.with(|track| {
            if track.get() {
                ALLOCS.set(ALLOCS.get() + 1);
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        TRACK.with(|track| {
            if track.get() {
                FREES.set(FREES.get() + 1);
            }
        });
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn timestamp(callback: u64, playback: u64) -> OutputStreamTimestamp {
    OutputStreamTimestamp {
        callback: StreamInstant::from_nanos(callback),
        playback: StreamInstant::from_nanos(playback),
    }
}

#[test]
fn callback_frames_keep_cpal_instants_and_restart_with_a_new_identity_at_each_rate() {
    let mut previous_id = None;
    for rate in [44_100, 48_000, 48_000] {
        let (mut clock, mut observations) = PlaybackClock::new(NonZeroU32::new(rate).unwrap());
        assert!(observations.take_latest().is_none());
        clock.publish(128, timestamp(1000, 3000));
        let first = observations.take_latest().unwrap();
        assert_eq!(first.stream_id, observations.stream_id);
        assert_ne!(Some(first.stream_id), previous_id);
        assert_eq!(first.first_frame, 0);
        assert_eq!(first.frames, 128);
        assert_eq!(first.sample_rate.get(), rate);
        assert_eq!(first.timestamp, timestamp(1000, 3000));
        clock.publish(257, timestamp(4000, 8000));
        let next = observations.take_latest().unwrap();
        assert_eq!(next.stream_id, first.stream_id);
        assert_eq!(next.first_frame, 128);
        assert_eq!(
            next.frames, 257,
            "observe real length, not a requested fixed buffer"
        );
        assert_eq!(next.timestamp, timestamp(4000, 8000));
        assert_eq!(observations.dropped(), 0);
        previous_id = Some(first.stream_id);
    }
}

#[test]
fn queue_overflow_never_blocks_allocates_or_loses_frame_identity() {
    let (mut clock, mut observations) = PlaybackClock::new(NonZeroU32::new(48_000).unwrap());
    let stamp = timestamp(100, 50); // Preserve even an unavailable/inverted prediction honestly.
    ALLOCS.set(0);
    FREES.set(0);
    TRACK.set(true);
    for _ in 0..1000 {
        clock.publish(256, stamp);
    }
    TRACK.set(false);
    assert_eq!(ALLOCS.get(), 0);
    assert_eq!(FREES.get(), 0);
    let queued = observations.take_latest().unwrap();
    let accepted = queued.first_frame / 256 + 1;
    assert_eq!(accepted + observations.dropped(), 1000);
    assert_eq!(queued.timestamp, stamp);
    assert!(observations.take_latest().is_none());
    clock.publish(64, stamp);
    let resumed = observations.take_latest().unwrap();
    assert_eq!(resumed.first_frame, 256_000);
    assert_eq!(resumed.frames, 64);
}
