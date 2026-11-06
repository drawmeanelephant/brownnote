#![allow(unsafe_code)]
use brownnote_native::engine::{Command, Meters, Mixer, QUEUE_CAPACITY, Sample};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
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

#[test]
fn callback_path_allocates_and_frees_nothing_for_two_seconds_at_both_rates() {
    for rate in [44_100, 48_000] {
        let meters = Arc::new(Meters::default());
        let sample = Sample {
            data: Arc::from(vec![0.2; rate as usize * 3]),
            rate,
        };
        let mut mixer = Mixer::new(vec![sample].into_boxed_slice(), rate, meters);
        let (mut send, mut recv) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let mut output = [0.0; 512];
        let mut left = rate as usize * 2;
        let mut block = 0;
        ALLOCS.set(0);
        FREES.set(0);
        TRACK.set(true);
        while left > 0 {
            let _ = send.push(Command::Trigger {
                pad: block % 16,
                sample: 0,
                velocity: 0.5,
                gain: 1.0,
                pan: 0.0,
                semitones: (block % 25) as f32 - 12.0,
            });
            let _ = send.push(Command::SetChoke(block % 3 == 0));
            let _ = send.push(Command::SetMaster((block % 10) as f32 / 10.0));
            if block % 17 == 0 {
                let _ = send.push(Command::Panic);
            }
            mixer.drain(&mut recv);
            let frames = left.min(256);
            mixer.render(&mut output[..frames * 2], 2);
            left -= frames;
            block += 1;
        }
        TRACK.set(false);
        assert_eq!(ALLOCS.get(), 0, "callback allocated at {rate}");
        assert_eq!(FREES.get(), 0, "callback freed memory at {rate}");
    }
}
