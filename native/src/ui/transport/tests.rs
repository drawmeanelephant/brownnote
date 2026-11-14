#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::ui;
use brownnote_native::{
    instrument::{DEFAULT_GAIN, Font, INSTRUMENT, describe},
    kit::PADS,
    performance::scheduler::ScheduleCounters,
    project::AssetId,
    takes::Removal,
};
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
        status.contains("2 HELD NOT SOUNDED (NO INSTRUMENT LOADED)"),
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

/// The pinned mechanics fixture, used only so a held note schedules a real
/// `NoteOff`. No sound quality is claimed here.
const SF2: &[u8] = include_bytes!("../../../docs/spikes/sampled-instrument/fixtures/mechanics.sf2");

/// One held-instrument note plus one one-shot per pad 1–4. One event per pad
/// lets the mixer's own meter stand in for that `EventId`'s dispatch count.
const MAPPINGS: &str = r#"{"version":1,"mappings":[
    {"type":"note","channel":0,"pitch":60,"target":{"type":"held","instrument":"1"}},
    {"type":"note","channel":0,"pitch":72,"target":{"type":"held","instrument":"1"}},
    {"type":"note","channel":1,"pitch":62,"target":{"type":"one_shot","pad":1}},
    {"type":"note","channel":1,"pitch":64,"target":{"type":"one_shot","pad":2}},
    {"type":"note","channel":1,"pitch":65,"target":{"type":"one_shot","pad":3}},
    {"type":"note","channel":1,"pitch":67,"target":{"type":"one_shot","pad":4}}
]}"#;

fn mount_instrument(rt: &mut Runtime<State>) {
    let font = Font::prepare(SF2, None).unwrap();
    let document = describe(
        INSTRUMENT,
        "mechanics",
        AssetId::of(SF2),
        font.preset(),
        DEFAULT_GAIN,
    );
    rt.state.instrument.restore(Some((document, Ok(font))));
    rt.frame(0.0);
    assert!(rt.state.instrument.plays(Target::Instrument(INSTRUMENT)));
}

/// Record one pass from the MIDI fixture. The fixture anchors its first packet
/// on the running pass, so packet times are take-relative.
fn record_fixture(rt: &mut Runtime<State>, packets: &[(u64, &[u8])]) -> TakeId {
    let midi = rt.state.midi.clone();
    ui::midi::fixture(&midi, SourceId(9), MAPPINGS, packets);
    record(&mut rt.state);
    rt.frame(0.0);
    assert!(
        rt.state.transport.pass.is_some(),
        "the fixture fills a pass"
    );
    run_until(rt, |s| s.transport.pass.is_none());
    assert_eq!(live(&rt.state), [TakeId(1)]);
    TakeId(1)
}

/// The pad each one-shot event owns. One event per pad lets the mixer's own
/// live voice meter stand in for that `EventId`'s dispatches.
fn event_pads(rt: &Runtime<State>, take: TakeId) -> BTreeMap<EventId, usize> {
    rt.state
        .takes
        .take(take)
        .expect("a committed take")
        .notes()
        .iter()
        .filter_map(|note| match note.on().target {
            Target::Pad(pad) => Some((note.on().id, pad.index())),
            _ => None,
        })
        .collect()
}

/// Frame until `done`, counting each pad's silent → sounding rise. A pad has
/// at most one voice per hit and a hit outlasts the gap to the next cycle, so
/// each rise is one dispatch of that pad's `EventId` reaching the mixer.
fn watch(rt: &mut Runtime<State>, done: impl Fn(&State) -> bool) -> [u32; 16] {
    let mut rises = [0; 16];
    let mut counts = rt.state.output.meters.counts();
    for _ in 0..1_000 {
        if done(&rt.state) {
            return rises;
        }
        rt.frame(0.01);
        let next = rt.state.output.meters.counts();
        for (pad, rise) in rises.iter_mut().enumerate() {
            if counts[pad] == 0 && next[pad] > 0 {
                *rise += 1;
            }
        }
        counts = next;
    }
    panic!("condition never reached: {}", status(&rt.state));
}

/// Fill the bounded scheduler to capacity, so the next replay pass is refused.
fn saturate(rt: &mut Runtime<State>) {
    let slot = crate::ui_test_support::slot(&rt.state, 0);
    let mut sequence = 0;
    loop {
        let epoch = rt.state.output.scheduler().epoch();
        let frame = rt.state.output.scheduler().frame() + 44_100;
        let event = ScheduledEvent {
            epoch,
            frame,
            original_time: OriginalTime::default(),
            // Never a release: a refused filler must not request the panic
            // the transport is meant to observe from its own replay.
            action: ScheduledAction::NoteOn {
                id: EventId {
                    take: TakeId(u128::MAX - 3),
                    sequence,
                },
                trigger: Trigger {
                    pad: 0,
                    slot,
                    group: 0,
                    velocity: 0.0,
                    gain: 1.0,
                    pan: 0.0,
                    semitones: 0.0,
                },
            },
        };
        if rt.state.output.schedule(event).outcome != ScheduleOutcome::Accepted {
            break;
        }
        sequence += 1;
    }
    assert_eq!(rt.state.output.schedule_counters().outstanding, 256);
}

#[test]
fn a_scheduler_recovery_rebuilds_the_cycle_without_playing_anything_twice() {
    let mut rt = runtime();
    one_bar(&mut rt);
    mount_instrument(&mut rt);
    let take = record_fixture(
        &mut rt,
        &[
            (0, &[0x90, 60, 100][..]),
            (0, &[0x90, 72, 100][..]),
            (400_000, &[0x91, 62, 100][..]),
            (550_000, &[0x91, 64, 100][..]),
            (560_000, &[0x91, 65, 100][..]),
            (570_000, &[0x91, 67, 100][..]),
            (600_000, &[0x80, 60, 0][..]),
            (600_000, &[0x80, 72, 0][..]),
        ],
    );
    let owned = event_pads(&rt, take);
    assert_eq!(
        owned.values().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 2, 3, 4]),
        "one pad per event"
    );
    let origin = rt.state.transport.playing.unwrap();
    let cycle = loop_nanos(&rt.state.takes);
    // Two cycles of steady replay, then fill the queue in the frame before the
    // held notes' releases enter the 500 ms window.
    run_until(&mut rt, |s| app_nanos(s) - origin >= 2 * cycle + 90_000_000);
    let played = engine(&rt).dispatched;
    let sent = rt.state.transport.counters.scheduled;
    saturate(&mut rt);
    rt.frame(0.01);
    assert_eq!(
        rt.state.transport.counters.schedule_lost, 2,
        "both replay releases were refused"
    );
    assert_eq!(
        rt.state.transport.counters.recovery_requested, 2,
        "both refused releases asked for the bounded panic"
    );
    assert_eq!(rt.state.output.schedule_counters().recovery_requests, 2);
    rt.frame(0.01);
    let recovered = engine(&rt);
    assert_eq!(
        (recovered.recoveries, recovered.cancelled),
        (1, 256),
        "requests coalesce into one panic that cancels the accepted work"
    );
    assert_eq!(rt.state.transport.counters.recovered, 1);
    // The rest of the cycle is rebuilt and each event reaches the mixer once.
    let rises = watch(&mut rt, |s| {
        app_nanos(s) - origin >= 2 * cycle + 650_000_000
    });
    for (id, pad) in &owned {
        assert_eq!(rises[*pad], 1, "event {id:?} dispatched exactly once");
    }
    assert_eq!(rises[0], 0, "refused filler work never sounded");
    assert_eq!(engine(&rt).late, 0, "no dispatched event was retriggered");
    assert_eq!(
        engine(&rt).dispatched - played,
        6,
        "the two rebuilt releases and four pad hits"
    );
    assert_eq!(
        rt.state.transport.counters.scheduled - sent,
        6,
        "rebuilt once, never replayed"
    );
    let status = label(&rt, "transport-status");
    assert!(status.contains("LOSS 2"), "{status}");
    assert!(status.contains("RECOVERED 1 · CANCELLED 256"), "{status}");
    let sent_now = rt.state.transport.counters.scheduled;
    assert!(
        status.contains(&format!("REPLAY {sent_now} SENT · ")),
        "{status}"
    );
    assert!(status.contains(" PLAYED"), "{status}");
    // Nothing is left in the window: one report, no retry loop.
    let sent = rt.state.transport.counters.scheduled;
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= 2 * cycle + 1_500_000_000
    });
    assert_eq!(
        rt.state.transport.counters.scheduled, sent,
        "a quiet window is not re-sent"
    );
    assert_eq!(engine(&rt).recoveries, 1);
    // STOP still panics the engine and clears the window bookkeeping.
    tap(&mut rt, "transport-play");
    assert!(!rt.state.transport.playing());
    assert!(rt.state.transport.anchor.is_none());
    let stopped = engine(&rt).dispatched;
    run(&mut rt, 2.5);
    assert_eq!(engine(&rt).dispatched, stopped, "STOP cancelled replay");
    // A later PLAY re-arms from a fresh anchor.
    tap(&mut rt, "transport-play");
    run(&mut rt, 0.05);
    assert!(rt.state.transport.playing());
    assert!(rt.state.transport.anchor.is_some());
    assert!(
        rt.state.transport.counters.scheduled >= sent,
        "PLAY re-arms the next window"
    );
}

#[test]
fn a_recovery_with_no_remaining_work_reports_once_and_stops() {
    let mut rt = runtime();
    one_bar(&mut rt);
    mount_instrument(&mut rt);
    let take = record_fixture(
        &mut rt,
        &[(0, &[0x90, 60, 100][..]), (300_000, &[0x80, 60, 0][..])],
    );
    assert_eq!(rt.state.takes.take(take).unwrap().notes().len(), 1);
    let origin = rt.state.transport.playing.unwrap();
    let cycle = loop_nanos(&rt.state.takes);
    // A quiet stretch: this take sounds only at 0 and 300 ms of each cycle.
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= 2 * cycle + 900_000_000
    });
    let sent = rt.state.transport.counters.scheduled;
    let played = engine(&rt).dispatched;
    saturate(&mut rt);
    let epoch = rt.state.output.scheduler().epoch();
    let frame = rt.state.output.scheduler().frame();
    let refused = rt.state.output.schedule(ScheduledEvent {
        epoch,
        frame,
        original_time: OriginalTime::default(),
        action: ScheduledAction::NoteOff {
            id: EventId {
                take: TakeId(u128::MAX - 4),
                sequence: 0,
            },
        },
    });
    assert_eq!(refused.outcome, ScheduleOutcome::Full);
    assert!(refused.release_recovery_requested);
    rt.frame(0.01);
    let engine_now = engine(&rt);
    assert_eq!((engine_now.recoveries, engine_now.cancelled), (1, 256));
    assert_eq!(rt.state.transport.counters.recovered, 1);
    assert_eq!(
        rt.state.transport.counters.recovery_requested, 0,
        "another producer asked for this panic, not the transport"
    );
    assert_eq!(
        rt.state.transport.counters.scheduled, sent,
        "an empty window is not re-sent"
    );
    let status = label(&rt, "transport-status");
    assert!(status.contains("RECOVERED 1 · CANCELLED 256"), "{status}");
    // The rest of the cycle sends nothing and never repeats the report.
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= 2 * cycle + 1_500_000_000
    });
    assert_eq!(
        rt.state.transport.counters.scheduled, sent,
        "a quiet window is not re-sent"
    );
    assert_eq!(engine(&rt).recoveries, 1);
    assert_eq!(engine(&rt).dispatched, played);
    // The loop still replays its next cycle normally.
    run_until(&mut rt, |s| {
        app_nanos(s) - origin >= 3 * cycle + 350_000_000
    });
    assert_eq!(engine(&rt).dispatched, played + 2, "the next start and end");
    assert_eq!(engine(&rt).recoveries, 1);
}

#[test]
fn a_replaced_stream_during_a_recovery_reanchors_instead_of_replaying() {
    let mut rt = runtime();
    one_bar(&mut rt);
    mount_instrument(&mut rt);
    let take = record_fixture(
        &mut rt,
        &[
            (0, &[0x90, 60, 100][..]),
            (550_000, &[0x91, 62, 100][..]),
            (600_000, &[0x80, 60, 0][..]),
        ],
    );
    let origin = rt.state.transport.playing.unwrap();
    let cycle = loop_nanos(&rt.state.takes);
    run_until(&mut rt, |s| app_nanos(s) - origin >= 2 * cycle + 90_000_000);
    let old = rt.state.output.scheduler().epoch();
    saturate(&mut rt);
    rt.frame(0.01);
    assert_eq!(rt.state.transport.counters.schedule_lost, 1);
    // Reopen the stream before the callback reaches its panic boundary.
    let slots = rt.state.assets.slots().clone();
    rt.state.install_output(Output::virtual_driver(
        slots,
        "reopened stream".into(),
        false,
    ));
    let epoch = rt.state.output.scheduler().epoch();
    assert_ne!(epoch, old, "a reopen is a fresh stream");
    rt.frame(0.01);
    assert_eq!(
        rt.state.transport.anchor.map(|anchor| anchor.epoch),
        Some(epoch),
        "re-anchored on the new stream"
    );
    assert_eq!(engine(&rt).stale_epoch, 0, "no old window replayed");
    assert_eq!(
        engine(&rt).recoveries,
        0,
        "the replaced stream never ran the panic"
    );
    assert_eq!(rt.state.transport.counters.recovered, 0);
    let status = label(&rt, "transport-status");
    assert!(!status.contains("RECOVERED"), "{status}");
    // The rest of the cycle is rebuilt against the new clock and plays once.
    let owned = event_pads(&rt, take);
    let rises = watch(&mut rt, |s| {
        app_nanos(s) - origin >= 2 * cycle + 700_000_000
    });
    for (id, pad) in &owned {
        assert_eq!(rises[*pad], 1, "event {id:?} played once after reopen");
    }
    assert_eq!(engine(&rt).dispatched, 2, "the rebuilt hit and its release");
}

fn beat_label(rt: &Runtime<State>) -> String {
    label(rt, "transport-beat")
}

#[test]
fn transport_default_tempo_reads_116_bpm_and_steps_through_takes() {
    let mut rt = runtime();
    assert_eq!(rt.state.takes.tempo(), Tempo::default());
    assert_eq!(label(&rt, "transport-bpm"), "Tempo 116 BPM");
    assert!(
        rt.elements()
            .into_iter()
            .any(|e| e.text.as_deref() == Some("116 BPM")),
        "the strip shows 116 BPM"
    );
    assert_eq!(beat_label(&rt), "Transport stopped");

    tap(&mut rt, "transport-tempo-up");
    assert_eq!(rt.state.takes.tempo().milli_bpm(), 117_000);
    assert_eq!(label(&rt, "transport-bpm"), "Tempo 117 BPM");

    tap(&mut rt, "transport-tempo-down");
    tap(&mut rt, "transport-tempo-down");
    assert_eq!(rt.state.takes.tempo().milli_bpm(), 115_000);
    assert_eq!(label(&rt, "transport-bpm"), "Tempo 115 BPM");

    // The steppers are buttons, never a text field: pad keys still play pads.
    let element = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("transport-tempo-up"))
        .expect("tempo stepper");
    assert_eq!(element.role, "button");
    assert_eq!(element.label.as_deref(), Some("Faster tempo"));
    hit(&mut rt, 4);
    rt.frame(0.01);
    assert!(
        rt.state.output.meters.counts()[4] > 0,
        "pad key still triggers"
    );
}

#[test]
fn transport_bpm_formats_integral_and_fractional_milli_bpm() {
    assert_eq!(format_bpm(Tempo::try_from(116_000).unwrap()), "116 BPM");
    assert_eq!(format_bpm(Tempo::try_from(116_500).unwrap()), "116.5 BPM");
    assert_eq!(format_bpm(Tempo::try_from(116_250).unwrap()), "116.25 BPM");
    assert_eq!(format_bpm(Tempo::try_from(120_001).unwrap()), "120.001 BPM");
}

#[test]
fn transport_tempo_change_while_armed_or_recording_surfaces_tempo_locked() {
    let mut rt = runtime();
    one_bar(&mut rt);
    tap(&mut rt, "transport-count-in"); // One bar of count-in keeps it armed.
    tap(&mut rt, "transport-record");
    let tempo = rt.state.takes.tempo();
    let start = rt.state.transport.pass.unwrap().start;

    // Armed: the registry refuses and its timing is untouched.
    tap(&mut rt, "transport-tempo-up");
    assert_eq!(rt.state.takes.tempo(), tempo);
    assert_eq!(rt.state.transport.pass.unwrap().start, start);
    assert!(!rt.state.transport.pass.unwrap().started);
    let status = label(&rt, "transport-status");
    assert!(status.contains("Tempo is locked"), "{status}");

    // Recording: still refused, with the same clear notice.
    run_until(&mut rt, |s| s.transport.pass.is_some_and(|p| p.started));
    tap(&mut rt, "transport-tempo-down");
    assert_eq!(rt.state.takes.tempo(), tempo);
    assert_eq!(rt.state.transport.pass.unwrap().start, start);
    let status = label(&rt, "transport-status");
    assert!(status.contains("Tempo is locked"), "{status}");
}

#[test]
fn transport_tempo_change_while_playing_reanchors_replay_without_loss() {
    let mut rt = runtime();
    keyboard_take(&mut rt);
    if !rt.state.transport.playing() {
        tap(&mut rt, "transport-play");
    }
    run(&mut rt, 0.3);
    let origin = rt.state.transport.playing.unwrap();
    let dispatched = engine(&rt).dispatched;

    tap(&mut rt, "transport-tempo-up");
    assert_eq!(rt.state.takes.tempo().milli_bpm(), 117_000);
    assert!(
        rt.state.transport.playing.unwrap() > origin,
        "playback re-anchored at the change"
    );
    assert_eq!(rt.state.transport.counters.schedule_lost, 0);

    // The loop keeps replaying from the new origin rather than going silent.
    run(&mut rt, 2.5);
    assert!(
        engine(&rt).dispatched > dispatched,
        "replay went silent after the tempo change"
    );
    assert_eq!(rt.state.transport.counters.schedule_lost, 0);
}

#[test]
fn transport_beat_indicator_tracks_count_in_then_bar_beat_and_redraws_on_downbeat() {
    let mut rt = runtime();
    one_bar(&mut rt);
    tap(&mut rt, "transport-count-in");
    tap(&mut rt, "transport-record");

    // Count-in counts the remaining beats down from 4 to 1.
    let mut count_in = BTreeSet::new();
    for _ in 0..400 {
        if rt.state.transport.pass.is_some_and(|p| p.started) {
            break;
        }
        rt.frame(0.01);
        count_in.insert(beat_label(&rt));
    }
    assert!(
        count_in.contains("Count-in beat 4 of 4") && count_in.contains("Count-in beat 1 of 4"),
        "{count_in:?}"
    );

    // The downbeat reuses bar.beat (1.1), so the stored triple must keep
    // `is_count_in` for the redraw; the label proves the transition happened.
    let mut beats = BTreeSet::new();
    for _ in 0..600 {
        rt.frame(0.01);
        beats.insert(beat_label(&rt));
        if beats.contains("Bar 1 beat 2") {
            break;
        }
    }
    assert!(beats.contains("Bar 1 beat 1"), "{beats:?}");
    assert!(beats.contains("Bar 1 beat 2"), "{beats:?}");

    let element = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("transport-beat"))
        .expect("beat indicator");
    assert_eq!(element.role, "status");
    assert!(
        element
            .label
            .as_deref()
            .unwrap_or_default()
            .starts_with("Bar 1 beat"),
        "{:?}",
        element.label
    );
}

#[test]
fn transport_stored_position_keeps_count_in_so_the_downbeat_redraws() {
    let mut t = Transport::default();
    let mut changed = |beat| std::mem::replace(&mut t.shown, beat) != beat;
    assert!(changed(Some((1, 1, true))), "first draw");
    assert!(!changed(Some((1, 1, true))), "same beat does not redraw");
    // (1, 1) alone would look unchanged; the boolean forces the redraw.
    assert!(changed(Some((1, 1, false))), "downbeat redraws");
    assert!(!changed(Some((1, 1, false))), "stable after the downbeat");
}
