#![allow(unsafe_code, clippy::unwrap_used)]

use brownnote_native::{
    engine::{Meters, Mixer, QUEUE_CAPACITY, SLOTS, Slots},
    output::{Callback, CallbackMeasurements, CallbackProbe, CallbackSummary, PlaybackClock},
};
use cpal::{OutputStreamTimestamp, StreamInstant};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    num::NonZeroU32,
    sync::Arc,
    time::Duration,
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

/// The probe's own bounded queue, one per stream, with no device involved.
fn probe_queue(rate: u32) -> (CallbackProbe, CallbackMeasurements) {
    let rate = NonZeroU32::new(rate).unwrap();
    let (_clock, observations) = PlaybackClock::new(rate);
    CallbackProbe::new(observations.stream_id, rate)
}

/// One summary over synthesized observations of this stream, so every printed
/// number derives from the fixtures rather than from wall time.
fn report(rate: u32, observations: &[(usize, Duration)]) -> String {
    let (mut probe, mut measurements) = probe_queue(rate);
    for (index, (frames, duration)) in observations.iter().enumerate() {
        let callback = index as u64 * 10_000_000;
        probe.publish(
            *duration,
            *frames,
            timestamp(callback, callback + 5_000_000),
        );
    }
    let mut summary = CallbackSummary::default();
    summary.absorb(&mut measurements);
    summary.to_string()
}

/// The stable deadline line, the only reported distribution that can be
/// negative.
fn headroom(report: &str) -> &str {
    report
        .lines()
        .find(|line| line.starts_with("callback_headroom_us="))
        .unwrap_or("<missing callback_headroom_us line>")
}

#[test]
fn callback_cost_and_presentation_lead_summarize_minimum_median_and_p95() {
    let (mut probe, mut measurements) = probe_queue(48_000);
    let mut summary = CallbackSummary::default();
    for i in 1..=20u64 {
        let callback = i * 10_000_000;
        probe.publish(
            Duration::from_millis(i),
            if i < 10 { 512 } else { 256 },
            timestamp(callback, callback + i * 500_000),
        );
    }
    summary.absorb(&mut measurements);
    assert_eq!(measurements.dropped(), 0);
    assert_eq!(
        summary.to_string(),
        "callbacks=20, dropped=0, rejected=0, inverted_lead=0\n\
         callback_duration_us=min=1000, p50=10000, p95=19000, n=20\n\
         callback_frames=min=256, max=512, n=20\n\
         presentation_lead_us=min=500, p50=5000, p95=9500, n=20\n\
         callback_headroom_us=min=-14667, p50=-5667, p95=8667, n=20, \
         deadline_overruns=11, worst_overrun_us=14667\n\
         round_trip_latency=unmeasured (output-only callback and presentation \
         instants cannot derive it)"
    );
    // A second drain cannot double-count: the queue is empty.
    summary.absorb(&mut measurements);
    assert!(summary.to_string().starts_with("callbacks=20, "));
}

#[test]
fn zero_and_one_observation_report_placeholders_not_invented_numbers() {
    assert_eq!(
        CallbackSummary::default().to_string(),
        "callbacks=0, dropped=0, rejected=0, inverted_lead=0\n\
         callback_duration_us=min=none, p50=none, p95=none, n=0\n\
         callback_frames=min=none, max=none, n=0\n\
         presentation_lead_us=min=none, p50=none, p95=none, n=0\n\
         callback_headroom_us=min=none, p50=none, p95=none, n=0, \
         deadline_overruns=0, worst_overrun_us=none\n\
         round_trip_latency=unmeasured (output-only callback and presentation \
         instants cannot derive it)"
    );
    let (mut probe, mut measurements) = probe_queue(44_100);
    // Rounded to whole microseconds, never truncated to a misleading zero.
    probe.publish(
        Duration::from_nanos(2_500_400),
        512,
        timestamp(10_000_000, 11_200_000),
    );
    let mut summary = CallbackSummary::default();
    summary.absorb(&mut measurements);
    assert_eq!(
        summary.to_string(),
        "callbacks=1, dropped=0, rejected=0, inverted_lead=0\n\
         callback_duration_us=min=2500, p50=2500, p95=2500, n=1\n\
         callback_frames=min=512, max=512, n=1\n\
         presentation_lead_us=min=1200, p50=1200, p95=1200, n=1\n\
         callback_headroom_us=min=9110, p50=9110, p95=9110, n=1, \
         deadline_overruns=0, worst_overrun_us=0\n\
         round_trip_latency=unmeasured (output-only callback and presentation \
         instants cannot derive it)"
    );
}

#[test]
fn a_prediction_at_or_before_its_callback_is_counted_not_invented() {
    let (mut probe, mut measurements) = probe_queue(48_000);
    probe.publish(
        Duration::from_micros(900),
        128,
        timestamp(5_000_000, 5_000_000),
    );
    probe.publish(
        Duration::from_micros(800),
        128,
        timestamp(6_000_000, 4_000_000),
    );
    let mut summary = CallbackSummary::default();
    summary.absorb(&mut measurements);
    // A zero-length prediction is a lead; an earlier playback instant is
    // unavailable, so it is counted and kept out of the distribution.
    assert_eq!(
        summary.to_string(),
        "callbacks=2, dropped=0, rejected=0, inverted_lead=1\n\
         callback_duration_us=min=800, p50=800, p95=900, n=2\n\
         callback_frames=min=128, max=128, n=2\n\
         presentation_lead_us=min=0, p50=0, p95=0, n=1\n\
         callback_headroom_us=min=1767, p50=1767, p95=1867, n=2, \
         deadline_overruns=0, worst_overrun_us=0\n\
         round_trip_latency=unmeasured (output-only callback and presentation \
         instants cannot derive it)"
    );
}

#[test]
fn observations_inside_their_own_budget_report_no_overruns_and_positive_headroom() {
    // 480 frames at 48 kHz is exactly a 10 ms deadline, so 1..5 ms callbacks
    // leave 9..5 ms of headroom and none of them overruns.
    let inside = report(
        48_000,
        &[
            (480, Duration::from_millis(1)),
            (480, Duration::from_millis(2)),
            (480, Duration::from_millis(3)),
            (480, Duration::from_millis(4)),
            (480, Duration::from_millis(5)),
        ],
    );
    assert_eq!(
        headroom(&inside),
        "callback_headroom_us=min=5000, p50=7000, p95=9000, n=5, \
         deadline_overruns=0, worst_overrun_us=0"
    );
}

#[test]
fn a_duration_equal_to_its_budget_is_not_an_overrun_but_a_longer_one_is_counted() {
    let equal = report(48_000, &[(480, Duration::from_micros(10_000))]);
    assert_eq!(
        headroom(&equal),
        "callback_headroom_us=min=0, p50=0, p95=0, n=1, deadline_overruns=0, \
         worst_overrun_us=0"
    );
    let past = report(48_000, &[(480, Duration::from_micros(10_250))]);
    assert_eq!(
        headroom(&past),
        "callback_headroom_us=min=-250, p50=-250, p95=-250, n=1, \
         deadline_overruns=1, worst_overrun_us=250"
    );
    // The overrun is reported plainly; the block still makes no latency claim.
    assert!(
        past.starts_with("callbacks=1, dropped=0, rejected=0, inverted_lead=0\n"),
        "{past}"
    );
    assert!(past.ends_with("instants cannot derive it)"), "{past}");
    // Equal and one overrun in the same run: the boundary adds no overrun.
    let both = report(
        48_000,
        &[
            (480, Duration::from_micros(10_000)),
            (480, Duration::from_micros(10_250)),
        ],
    );
    assert_eq!(
        headroom(&both),
        "callback_headroom_us=min=-250, p50=-250, p95=0, n=2, \
         deadline_overruns=1, worst_overrun_us=250"
    );
}

#[test]
fn each_observation_uses_its_own_frames_and_stream_rate() {
    // 441 frames is exactly 10 ms at 44.1 kHz but 9.1875 ms at 48 kHz, so the
    // same 9.9 ms callback fits one stream's budget and overruns the other's.
    let slow = report(44_100, &[(441, Duration::from_micros(9_900))]);
    assert_eq!(
        headroom(&slow),
        "callback_headroom_us=min=100, p50=100, p95=100, n=1, \
         deadline_overruns=0, worst_overrun_us=0"
    );
    let fast = report(48_000, &[(441, Duration::from_micros(9_900))]);
    assert_eq!(
        headroom(&fast),
        "callback_headroom_us=min=-713, p50=-713, p95=-713, n=1, \
         deadline_overruns=1, worst_overrun_us=713"
    );
    // At one rate the delivered length decides: 240 frames is a 5 ms deadline
    // and 480 frames a 10 ms deadline, so 7.5 ms overruns only the short one.
    let varied = report(
        48_000,
        &[
            (240, Duration::from_micros(7_500)),
            (480, Duration::from_micros(7_500)),
        ],
    );
    assert_eq!(
        headroom(&varied),
        "callback_headroom_us=min=-2500, p50=-2500, p95=2500, n=2, \
         deadline_overruns=1, worst_overrun_us=2500"
    );
}

fn probed_callback(rate: u32) -> (Callback, CallbackMeasurements) {
    let rate = NonZeroU32::new(rate).unwrap();
    let (clock, observations) = PlaybackClock::new(rate);
    let (probe, measurements) = CallbackProbe::new(observations.stream_id, rate);
    let (retired, _returns) = rtrb::RingBuffer::new(SLOTS);
    let mixer = Mixer::new(
        Slots::empty(),
        rate.get(),
        Arc::new(Meters::default()),
        retired,
    );
    let (_send, commands) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
    let mut callback = Callback::new(clock, mixer, commands, 2);
    callback.set_probe(probe);
    (callback, measurements)
}

#[test]
fn the_measured_callback_allocates_nothing_and_counts_a_full_queue() {
    let (mut callback, mut measurements) = probed_callback(48_000);
    let mut data = [0.0f32; 512];
    ALLOCS.set(0);
    FREES.set(0);
    TRACK.set(true);
    for i in 0..200u64 {
        let callback_instant = i * 10_000_000;
        callback.process(
            &mut data,
            timestamp(callback_instant, callback_instant + 8_000_000),
        );
    }
    TRACK.set(false);
    assert_eq!(ALLOCS.get(), 0);
    assert_eq!(FREES.get(), 0);
    // The queue holds 64; the remaining publications were counted, not waited
    // for, and the audio callback still produced its block every time.
    assert_eq!(measurements.dropped(), 200 - 64);
    let mut summary = CallbackSummary::default();
    summary.absorb(&mut measurements);
    // Wall time is whatever this machine measured, so only its presence and the
    // synthesised CPAL instants and delivered block sizes are asserted.
    let report = summary.to_string();
    assert!(
        report.starts_with("callbacks=64, dropped=136, rejected=0, inverted_lead=0\n"),
        "{report}"
    );
    assert!(report.contains("\ncallback_duration_us=min="), "{report}");
    assert!(
        report.contains("\ncallback_frames=min=256, max=256, n=64\n"),
        "{report}"
    );
    assert!(
        report.contains("\npresentation_lead_us=min=8000, p50=8000, p95=8000, n=64\n"),
        "{report}"
    );
    // Wall durations are whatever this machine measured, so the headroom
    // values are not pinned; the line, its count and the overrun accounting
    // must still be present.
    assert!(report.contains("\ncallback_headroom_us=min="), "{report}");
    assert!(report.contains(", n=64, deadline_overruns="), "{report}");
    assert!(report.ends_with("instants cannot derive it)"), "{report}");
}
