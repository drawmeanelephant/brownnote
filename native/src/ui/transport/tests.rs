#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::ui;
use brownnote_native::{kit::PADS, performance::scheduler::ScheduleCounters, takes::Removal};
use rune::{Env, Key, Runtime};

fn runtime() -> Runtime<State> {
    let kit = crate::ui_test_support::kit(0.5, 44_100, 44_100);
    let mut state = State::new(kit, true, true);
    state.pedals.auto_load = false;
    let mut rt = Runtime::new(state, ui::view, (1120.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.0);
    rt
}

/// Advance the app clock in 10 ms frames (the virtual mixer renders each).
fn run(rt: &mut Runtime<State>, seconds: f64) {
    for _ in 0..(seconds / 0.01).round() as usize {
        rt.frame(0.01);
    }
}

fn run_until(rt: &mut Runtime<State>, done: impl Fn(&State) -> bool) {
    for _ in 0..1_000 {
        if done(&rt.state) {
            return;
        }
        rt.frame(0.01);
    }
    panic!("condition never reached: {}", status(&rt.state));
}

fn tap(rt: &mut Runtime<State>, target: &str) {
    let (x, y) = rt
        .find(target)
        .unwrap_or_else(|| panic!("missing {target}"));
    rt.pointer_down(x, y);
    rt.frame(0.0);
    rt.pointer_up(x, y);
    rt.frame(0.0);
}

fn hit(rt: &mut Runtime<State>, pad: usize) {
    rt.key(Key::Text(PADS[pad].key.to_string()).into());
    rt.frame(0.0);
}

fn label(rt: &Runtime<State>, target: &str) -> String {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(target))
        .unwrap_or_else(|| panic!("missing {target}"))
        .label
        .unwrap()
}

/// Nanoseconds of the current app time relative to the open pass start.
fn offset(rt: &Runtime<State>) -> u64 {
    app_nanos(&rt.state) - rt.state.transport.pass.unwrap().start
}

fn engine(rt: &Runtime<State>) -> ScheduleCounters {
    rt.state.output.schedule_counters()
}

fn live(s: &State) -> Vec<TakeId> {
    s.takes
        .takes()
        .filter(|take| take.removal().is_none())
        .map(|take| take.id())
        .collect()
}

fn one_bar(rt: &mut Runtime<State>) {
    tap(rt, "transport-loop");
    assert_eq!(rt.state.takes.loop_bars().bars(), 1);
    assert_eq!(label(rt, "transport-loop"), "LOOP 1");
    let milli_bpm = u64::from(rt.state.takes.tempo().milli_bpm());
    assert_eq!(
        loop_nanos(&rt.state.takes),
        4 * 60_000_000_000_000 / milli_bpm
    );
}

/// One keyboard take on pad 4: a 20 ms flam at 250 ms, committed after one
/// loop cycle. Returns the two take-relative times.
fn keyboard_take(rt: &mut Runtime<State>) -> [u64; 2] {
    one_bar(rt);
    tap(rt, "transport-record");
    let status = label(rt, "transport-status");
    assert!(status.starts_with("REC T1 · 0 NOTES"), "{status}");
    assert_eq!(label(rt, "transport-record"), "■ REC");
    run(rt, 0.25);
    let first = offset(rt);
    hit(rt, 4);
    run(rt, 0.02);
    let second = offset(rt);
    hit(rt, 4);
    assert!(label(rt, "transport-status").starts_with("REC T1 · 2 NOTES"));
    run_until(rt, |s| s.transport.pass.is_none());
    [first, second]
}

#[test]
fn keyboard_pass_keeps_take_relative_times_and_replays_through_the_scheduler() {
    let mut rt = runtime();
    let [first, second] = keyboard_take(&mut rt);
    assert_eq!(second - first, 20_000_000);
    let take = rt.state.takes.take(TakeId(1)).unwrap();
    let times: Vec<u64> = take
        .notes()
        .iter()
        .map(|note| note.on().original_time.nanos())
        .collect();
    assert_eq!(times, [first, second], "D9: exact, unquantized offsets");
    for note in take.notes() {
        assert_eq!(note.on().target, Target::Pad(PadId::try_from(4).unwrap()));
        assert_eq!(note.on().velocity.value(), 127);
        assert_eq!(note.duration_nanos(), 0, "one-shots ignore release");
    }
    let status = label(&rt, "transport-status");
    assert!(status.contains("1 TAKES · 2 NOTES"), "{status}");
    assert!(status.contains("take 1 recorded: 2 note(s)"), "{status}");
    assert!(rt.state.transport.playing(), "the loop keeps playing");

    // Replay: nothing reaches the mixer before the first note's position in
    // the next cycle; both notes play once it passes; every cycle repeats.
    let cycle = loop_nanos(&rt.state.takes);
    let origin = rt.state.transport.playing.unwrap();
    let at = |rt: &Runtime<State>| app_nanos(&rt.state) - origin;
    assert_eq!(engine(&rt).dispatched, 0);
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= cycle + first - 15_000_000
    });
    assert_eq!(engine(&rt).dispatched, 0, "at {}", at(&rt));
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= cycle + first + 10_000_000
    });
    assert_eq!(engine(&rt).dispatched, 1, "at {}", at(&rt));
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= cycle + second + 10_000_000
    });
    assert_eq!(engine(&rt).dispatched, 2);
    assert!(
        rt.state.output.meters.counts()[4] > 0,
        "the pad voices sound"
    );
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= 2 * cycle + second + 10_000_000
    });
    assert_eq!(engine(&rt).dispatched, 4);
    assert_eq!(rt.state.transport.counters.scheduled, 4);
    assert_eq!(rt.state.transport.counters.schedule_lost, 0);
    assert_eq!(engine(&rt).late, 0);
    let status = label(&rt, "transport-status");
    assert!(status.contains("REPLAY 4 SENT · 4 PLAYED"), "{status}");

    // Stop cancels replay; nothing more is dispatched.
    tap(&mut rt, "transport-play");
    assert!(!rt.state.transport.playing());
    run(&mut rt, 2.5);
    assert_eq!(engine(&rt).dispatched, 4);
    assert!(label(&rt, "transport-status").starts_with("STOPPED"));
    // PLAY restarts the loop from now.
    tap(&mut rt, "transport-play");
    run(&mut rt, 0.3);
    assert_eq!(engine(&rt).dispatched, 6);
}

#[test]
fn overdub_replace_count_in_and_loop_lock_follow_the_pass_rules() {
    let mut rt = runtime();
    keyboard_take(&mut rt);
    // Overdub while playing: armed until the next loop boundary.
    tap(&mut rt, "transport-record");
    let pass = rt.state.transport.pass.unwrap();
    assert_eq!(pass.id, TakeId(2));
    let cycle = loop_nanos(&rt.state.takes);
    assert_eq!(
        (pass.start - rt.state.transport.playing.unwrap()) % cycle,
        0
    );
    assert!(pass.start > app_nanos(&rt.state));
    assert!(label(&rt, "transport-status").starts_with("ARMED T2"));
    tap(&mut rt, "transport-loop");
    assert_eq!(rt.state.takes.loop_bars().bars(), 1, "D10: loop is locked");
    assert!(label(&rt, "transport-status").contains("locked while recording"));
    hit(&mut rt, 0);
    assert_eq!(
        rt.state.transport.counters.outside_pass, 1,
        "before the boundary"
    );
    run_until(&mut rt, |s| app_nanos(s) >= pass.start + 100_000_000);
    hit(&mut rt, 0);
    run_until(&mut rt, |s| s.transport.pass.is_none());
    assert_eq!(live(&rt.state), [TakeId(1), TakeId(2)], "overdub layers");
    let overdub = rt.state.takes.take(TakeId(2)).unwrap().notes()[0].on();
    assert_eq!(overdub.target, Target::Pad(PadId::try_from(0).unwrap()));
    assert!((100_000_000..120_000_000).contains(&overdub.original_time.nanos()));
    assert_eq!(
        rt.state
            .edit
            .model
            .effective_notes(&rt.state.takes)
            .unwrap()
            .len(),
        3
    );

    // Replace: the new take supersedes every live take when it commits.
    tap(&mut rt, "transport-mode");
    assert_eq!(label(&rt, "transport-mode"), "REPLACE");
    tap(&mut rt, "transport-record");
    let pass = rt.state.transport.pass.unwrap();
    run_until(&mut rt, |s| app_nanos(s) >= pass.start + 50_000_000);
    hit(&mut rt, 2);
    run_until(&mut rt, |s| s.transport.pass.is_none());
    assert_eq!(live(&rt.state), [TakeId(3)]);
    for id in [TakeId(1), TakeId(2)] {
        assert_eq!(
            rt.state.takes.take(id).unwrap().removal(),
            Some(Removal::ReplacedBy(TakeId(3))),
            "originals are kept, only marked replaced"
        );
    }

    // Count-in from stopped: one bar before the pass, hits there are outside.
    tap(&mut rt, "transport-play");
    tap(&mut rt, "transport-mode");
    tap(&mut rt, "transport-count-in");
    assert_eq!(label(&rt, "transport-count-in"), "COUNT-IN 1");
    let outside = rt.state.transport.counters.outside_pass;
    tap(&mut rt, "transport-record");
    let pass = rt.state.transport.pass.unwrap();
    assert_eq!(pass.start - app_nanos(&rt.state), cycle, "one 4/4 bar");
    let status = label(&rt, "transport-status");
    assert!(status.starts_with("COUNT-IN 4 · T4"), "{status}");
    run(&mut rt, 0.6);
    let status = label(&rt, "transport-status");
    assert!(status.starts_with("COUNT-IN 3 · T4"), "{status}");
    hit(&mut rt, 1);
    assert_eq!(rt.state.transport.counters.outside_pass, outside + 1);
    run_until(&mut rt, |s| app_nanos(s) >= pass.start + 40_000_000);
    let expected = offset(&rt);
    hit(&mut rt, 1);
    run_until(&mut rt, |s| s.transport.pass.is_none());
    let notes = rt.state.takes.take(TakeId(4)).unwrap().notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].on().original_time.nanos(), expected);
}

#[test]
fn panic_stop_and_discard_never_leave_a_pass_or_replay_running() {
    let mut rt = runtime();
    one_bar(&mut rt);
    // Escape (app panic) closes a started pass with what was played.
    tap(&mut rt, "transport-record");
    run(&mut rt, 0.1);
    hit(&mut rt, 3);
    rt.key(Key::Escape.into());
    rt.frame(0.0);
    assert!(!rt.state.transport.active());
    assert!(!rt.state.takes.has_pass());
    assert_eq!(live(&rt.state), [TakeId(1)]);
    let status = label(&rt, "transport-status");
    assert!(
        status.contains("Panic; take 1 recorded: 1 note(s)"),
        "{status}"
    );
    run(&mut rt, 2.5);
    assert_eq!(engine(&rt).dispatched, 0, "panic stopped replay");

    // DISCARD drops a started pass; committed takes are untouched.
    tap(&mut rt, "transport-record");
    run(&mut rt, 0.1);
    hit(&mut rt, 3);
    tap(&mut rt, "transport-cancel");
    assert!(rt.state.transport.pass.is_none());
    assert!(!rt.state.takes.has_pass());
    assert_eq!(live(&rt.state), [TakeId(1)]);
    assert!(rt.state.takes.take(TakeId(2)).is_none());
    assert!(
        rt.state.transport.playing(),
        "discard keeps the loop playing"
    );
    assert!(rt.find("transport-cancel").is_none());

    // The panic button cancels a pass that is still in its count-in.
    tap(&mut rt, "transport-play");
    tap(&mut rt, "transport-count-in");
    tap(&mut rt, "transport-record");
    tap(&mut rt, "panic");
    assert!(!rt.state.takes.has_pass());
    assert!(label(&rt, "transport-status").contains("armed pass cancelled"));

    // A pass with nothing played commits nothing.
    tap(&mut rt, "transport-count-in");
    tap(&mut rt, "transport-record");
    run_until(&mut rt, |s| s.transport.pass.is_none());
    assert_eq!(live(&rt.state), [TakeId(1)]);
    assert!(label(&rt, "transport-status").contains("nothing played, empty pass discarded"));
}

#[test]
fn midi_fixture_records_velocities_flams_and_fifo_held_durations() {
    let mut rt = runtime();
    scene_midi(&mut rt.state);
    rt.frame(0.0);
    let status = label(&rt, "transport-status");
    assert!(status.starts_with("REC T1 · 4 NOTES"), "{status}");
    run_until(&mut rt, |s| s.transport.pass.is_none());
    let take = rt.state.takes.take(TakeId(1)).unwrap();
    let notes: Vec<_> = take
        .notes()
        .iter()
        .map(|note| {
            let on = note.on();
            (
                on.target,
                on.key.channel.value(),
                on.velocity.value(),
                on.original_time.nanos(),
                note.original_end().nanos(),
            )
        })
        .collect();
    let held = Target::Instrument(brownnote_native::performance::InstrumentId(1));
    let pad = Target::Pad(PadId::try_from(4).unwrap());
    // Same-key overlap ends FIFO (D14): 0→500 ms, then 100→900 ms.
    assert_eq!(
        notes,
        [
            (held, 0, 100, 0, 500_000_000),
            (held, 0, 90, 100_000_000, 900_000_000),
            (pad, 0, 1, 200_000_000, 200_000_000),
            (pad, 0, 127, 215_000_000, 215_000_000),
        ]
    );
    assert_eq!(rt.state.transport.counters.held_recorded, 2);
    let status = label(&rt, "transport-status");
    assert!(
        status.contains("SEL T1: 4 NOTES · HELD 500ms/800ms"),
        "{status}"
    );
    run(&mut rt, 0.1);
    let status = label(&rt, "transport-status");
    assert!(
        status.contains("2 HELD NOT SOUNDED (NO INSTRUMENT BACKEND)"),
        "{status}"
    );
    // Pad one-shots replay through the scheduler at their recorded offsets.
    let origin = rt.state.transport.playing.unwrap();
    let cycle = loop_nanos(&rt.state.takes);
    run_until(&mut rt, |s| app_nanos(s) - origin >= cycle + 225_000_000);
    assert_eq!(engine(&rt).dispatched, 2);
}

#[test]
fn disconnect_ends_held_notes_at_reconcile_and_never_invents_ends() {
    let mut rt = runtime();
    let mappings = r#"{"version":1,"mappings":[
        {"type":"note","channel":0,"pitch":60,"target":{"type":"held","instrument":"1"}}
    ]}"#;
    let midi = rt.state.midi.clone();
    tap(&mut rt, "transport-loop");
    tap(&mut rt, "transport-record");
    ui::midi::fixture(&midi, SourceId(9), mappings, &[(0, &[0x90, 60, 64])]);
    rt.frame(0.0);
    assert_eq!(rt.state.transport.counters.held_recorded, 1);
    // The loop cycle ends with the key still down: the pass waits.
    run(&mut rt, 2.2);
    let pass = rt
        .state
        .transport
        .pass
        .expect("held note keeps the pass open");
    assert!(pass.closing);
    assert!(label(&rt, "transport-status").starts_with("CLOSING T1 · 1 HELD"));
    assert!(rt.state.takes.take(TakeId(1)).is_none());
    let expected = app_nanos(&rt.state) - pass.start;
    ui::midi::request_disconnect(&midi);
    rt.frame(0.0);
    assert!(rt.state.transport.pass.is_none());
    let note = &rt.state.takes.take(TakeId(1)).unwrap().notes()[0];
    assert_eq!(note.on().original_time.nanos(), 0);
    assert_eq!(note.original_end().nanos(), expected);
}
