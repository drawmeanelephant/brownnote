#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::performance::{
    Channel, LoopBars, MusicalPosition, NoteKey, Pitch, SourceId, Target, Tempo,
};
use crate::project::PadId;
use crate::takes::{PassOptions, Removal};

fn bars(value: u8) -> LoopBars {
    LoopBars::try_from(value).unwrap()
}
fn tempo(value: u32) -> Tempo {
    Tempo::try_from(value).unwrap()
}
fn time(value: u64) -> OriginalTime {
    OriginalTime::from_nanos(value)
}
fn key(source: u128, channel: u8, pitch: u8) -> NoteKey {
    NoteKey {
        source: SourceId(source),
        channel: Channel::try_from(channel).unwrap(),
        pitch: Pitch::try_from(pitch).unwrap(),
    }
}
fn pad() -> Target {
    Target::Pad(PadId::try_from(3).unwrap())
}
fn velocity(value: u8) -> Velocity {
    Velocity::try_from(value).unwrap()
}
fn registry() -> Takes {
    Takes::new(bars(8), TakeId(700))
}
fn recording(model: &mut Takes) -> TakeId {
    let id = model.arm(PassOptions::default()).unwrap();
    model.start(id).unwrap();
    id
}
fn hit(model: &mut Takes, take: TakeId, pitch: u8, start: u64, end: u64, vel: u8) -> EventId {
    let key = key(7, 0, pitch);
    let id = model
        .note_on(take, pad(), key, velocity(vel), time(start))
        .unwrap();
    assert_eq!(model.note_off(take, key, time(end)).unwrap(), id);
    id
}
/// One committed take: a 15 ms same-pitch flam (e0/e1), an overlapping
/// different-pitch hit (e2), and one far, unaffected note (e3).
fn fixture_take(model: &mut Takes) -> (TakeId, [EventId; 4]) {
    let take = recording(model);
    let e0 = hit(model, take, 60, 10_000_000, 50_000_000, 90);
    let e1 = hit(model, take, 60, 25_000_000, 60_000_000, 100);
    let e2 = hit(model, take, 64, 30_000_000, 200_000_000, 80);
    let e3 = hit(model, take, 67, 1_000_000_000, 1_100_000_000, 70);
    model.finish(take).unwrap();
    (take, [e0, e1, e2, e3])
}
fn starts(model: &EditModel, takes: &Takes, take: TakeId) -> Vec<MusicalPosition> {
    model
        .effective_take_notes(takes, take)
        .unwrap()
        .iter()
        .map(|note| note.start)
        .collect()
}

#[test]
fn quantize_snaps_flam_and_overlaps_to_grid_without_merging() {
    // 250 BPM makes one 1/16 exactly 60 ms in nanoseconds, so expected grid
    // positions are exact: 29 ms -> 0, 61 ms and 76 ms -> 60 ms, 119 ms -> 120 ms.
    let mut takes = registry();
    takes.set_tempo(tempo(250_000)).unwrap();
    let take = recording(&mut takes);
    let flam_a = hit(&mut takes, take, 60, 61_000_000, 161_000_000, 90);
    let flam_b = hit(&mut takes, take, 60, 76_000_000, 176_000_000, 100);
    let overlap = hit(&mut takes, take, 64, 119_000_000, 219_000_000, 80);
    let early = hit(&mut takes, take, 67, 29_000_000, 229_000_000, 70);
    takes.finish(take).unwrap();

    let mut model = EditModel::new();
    assert_eq!(model.quantization(take), Quantization::Off);
    assert_eq!(
        model.toggle_quantization(&takes, take).unwrap(),
        Quantization::Sixteenth
    );
    let notes = model.effective_take_notes(&takes, take).unwrap();
    assert_eq!(notes.len(), 4);
    let expected = [
        (early, 0),
        (flam_a, 60_000_000),
        (flam_b, 60_000_000),
        (overlap, 120_000_000),
    ];
    for (note, (id, nanos)) in notes.iter().zip(expected) {
        assert_eq!(note.id, id);
        assert_eq!(
            note.start,
            MusicalPosition::from_original(time(nanos), tempo(250_000))
        );
    }
    // The flam shares a derived position but keeps two distinct events.
    let flam: Vec<_> = notes.iter().filter(|note| note.id.take == take).collect();
    assert_eq!(flam.len(), 4);
    assert_ne!(flam_a, flam_b);
    // Durations survive snapping: each end moved with its onset.
    for note in &notes {
        let original = takes
            .take(take)
            .unwrap()
            .notes()
            .iter()
            .find(|n| n.on().id == note.id)
            .unwrap();
        let duration = note.end.at_tempo_nanos(tempo(250_000)).unwrap()
            - note.start.at_tempo_nanos(tempo(250_000)).unwrap();
        assert_eq!(duration, original.duration_nanos());
    }
}

#[test]
fn quantize_off_restores_exact_original_times_with_overlaps() {
    let mut takes = registry(); // default 116 BPM: rational grid, exact units
    let (take, [e0, e1, e2, e3]) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    assert_eq!(
        model.toggle_quantization(&takes, take).unwrap(),
        Quantization::Sixteenth
    );
    // At 116 BPM the 1/16 grid is ~129.3 ms, so the early flam/overlap all
    // snap to zero and differ from their originals.
    let snapped = starts(&model, &takes, take);
    assert!(
        snapped
            .iter()
            .all(|start| *start != MusicalPosition::from_original(time(10_000_000), takes.tempo()))
    );
    assert_eq!(
        model.toggle_quantization(&takes, take).unwrap(),
        Quantization::Off
    );

    // D13: off restores every original onset/end exactly, flams and
    // same-pitch overlaps included.
    let notes = model.effective_take_notes(&takes, take).unwrap();
    assert_eq!(notes.len(), 4);
    for note in &notes {
        let original = takes
            .take(take)
            .unwrap()
            .notes()
            .iter()
            .find(|n| n.on().id == note.id)
            .unwrap();
        assert_eq!(
            note.start,
            MusicalPosition::from_original(original.on().original_time, takes.tempo())
        );
        assert_eq!(
            note.end,
            MusicalPosition::from_original(original.original_end(), takes.tempo())
        );
    }
    // D9: the stored originals were never rewritten by any toggle.
    let stored: Vec<_> = takes.take(take).unwrap().notes().to_vec();
    assert_eq!(
        stored.iter().map(|n| n.on().id).collect::<Vec<_>>(),
        vec![e0, e1, e2, e3]
    );
    assert_eq!(stored[0].on().original_time, time(10_000_000));
    assert_eq!(stored[1].on().original_time, time(25_000_000));

    // The toggle itself is undoable: undo returns to Sixteenth.
    assert!(model.can_undo());
    model.undo(&mut takes).unwrap();
    assert_eq!(model.quantization(take), Quantization::Sixteenth);
    model.redo(&mut takes).unwrap();
    assert_eq!(model.quantization(take), Quantization::Off);
}

#[test]
fn quantization_is_per_take() {
    let mut takes = registry();
    let (take_a, _) = fixture_take(&mut takes);
    let take_b = recording(&mut takes);
    hit(&mut takes, take_b, 60, 10_000_000, 50_000_000, 90);
    takes.finish(take_b).unwrap();

    let mut model = EditModel::new();
    model.toggle_quantization(&takes, take_a).unwrap();
    assert_eq!(model.quantization(take_a), Quantization::Sixteenth);
    assert_eq!(model.quantization(take_b), Quantization::Off);
    let b_notes = model.effective_take_notes(&takes, take_b).unwrap();
    assert_eq!(
        b_notes[0].start,
        MusicalPosition::from_original(time(10_000_000), takes.tempo())
    );
}

#[test]
fn nudge_delete_velocity_undo_redo_in_order_preserving_others() {
    let mut takes = registry();
    let (take, [e0, e1, e2, e3]) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    let original_view = model.effective_take_notes(&takes, take).unwrap();

    model.set_velocity(&takes, e0, velocity(33)).unwrap();
    let after_velocity = model.effective_take_notes(&takes, take).unwrap();
    model.delete(&takes, e1).unwrap();
    let after_delete = model.effective_take_notes(&takes, take).unwrap();
    model.nudge(&takes, e2, 5_000_000).unwrap();
    let after_nudge = model.effective_take_notes(&takes, take).unwrap();
    assert_eq!(model.undo_depth(), 3);

    // Edits are visible in the effective view only; originals stay put.
    assert_eq!(after_velocity.len(), 4);
    assert_eq!(
        after_velocity.iter().find(|n| n.id == e0).unwrap().velocity,
        velocity(33)
    );
    assert_eq!(after_delete.len(), 3);
    assert!(after_delete.iter().all(|n| n.id != e1));
    let nudged = after_nudge.iter().find(|n| n.id == e2).unwrap();
    assert_eq!(
        nudged.start,
        MusicalPosition::from_original(time(35_000_000), takes.tempo())
    );
    assert_eq!(
        nudged.end,
        MusicalPosition::from_original(time(205_000_000), takes.tempo())
    );
    assert_eq!(
        takes.take(take).unwrap().notes()[2].on().original_time,
        time(30_000_000)
    );

    // Undo replays in reverse order, restoring each earlier view exactly.
    model.undo(&mut takes).unwrap();
    assert_eq!(
        model.effective_take_notes(&takes, take).unwrap(),
        after_delete
    );
    model.undo(&mut takes).unwrap();
    assert_eq!(
        model.effective_take_notes(&takes, take).unwrap(),
        after_velocity
    );
    model.undo(&mut takes).unwrap();
    assert_eq!(
        model.effective_take_notes(&takes, take).unwrap(),
        original_view
    );
    assert!(!model.can_undo());

    // Redo replays forward in order.
    model.redo(&mut takes).unwrap();
    assert_eq!(
        model.effective_take_notes(&takes, take).unwrap(),
        after_velocity
    );
    model.redo(&mut takes).unwrap();
    assert_eq!(
        model.effective_take_notes(&takes, take).unwrap(),
        after_delete
    );
    model.redo(&mut takes).unwrap();
    assert_eq!(
        model.effective_take_notes(&takes, take).unwrap(),
        after_nudge
    );
    assert!(!model.can_redo());

    // The unaffected far note is byte-identical through every step.
    for view in [&original_view, &after_velocity, &after_delete, &after_nudge] {
        let far = view.iter().find(|n| n.id == e3).unwrap();
        assert_eq!(
            far.start,
            MusicalPosition::from_original(time(1_000_000_000), takes.tempo())
        );
        assert_eq!(far.velocity, velocity(70));
    }
}

#[test]
fn nudge_clamps_at_take_start_and_preserves_duration() {
    let mut takes = registry();
    let (take, [.., e3]) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    model.nudge(&takes, e3, -2_000_000_000).unwrap();
    let note = model
        .effective_take_notes(&takes, take)
        .unwrap()
        .into_iter()
        .find(|n| n.id == e3)
        .unwrap();
    assert_eq!(
        note.start,
        MusicalPosition::from_original(time(0), takes.tempo())
    );
    assert_eq!(
        note.end,
        MusicalPosition::from_original(time(100_000_000), takes.tempo())
    );
}

#[test]
fn continuous_drag_is_one_undo_step() {
    let mut takes = registry();
    let (take, [e0, e1, ..]) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    let before = model.effective_take_notes(&takes, take).unwrap();

    model.begin_drag().unwrap();
    assert!(model.drag_open());
    model.nudge(&takes, e0, 1_000_000).unwrap();
    model.nudge(&takes, e0, 2_000_000).unwrap();
    model.set_velocity(&takes, e0, velocity(40)).unwrap();
    model.nudge(&takes, e1, -500_000).unwrap();
    assert_eq!(model.undo_depth(), 0, "drag must not commit steps yet");
    assert!(model.end_drag().unwrap());
    assert_eq!(model.undo_depth(), 1, "one continuous drag = one step");
    assert!(!model.can_redo());

    let during = model.effective_take_notes(&takes, take).unwrap();
    assert_eq!(model.event_edit(e0).nudge_nanos, 3_000_000);
    assert_eq!(model.event_edit(e0).velocity, Some(velocity(40)));
    assert_eq!(model.event_edit(e1).nudge_nanos, -500_000);

    // A single undo returns to the pre-drag state.
    assert!(model.undo(&mut takes).unwrap());
    assert_eq!(model.event_edit(e0), EventEdit::default());
    assert_eq!(model.event_edit(e1), EventEdit::default());
    assert_eq!(model.effective_take_notes(&takes, take).unwrap(), before);
    assert!(!model.undo(&mut takes).unwrap());
    // A single redo reapplies the whole drag.
    assert!(model.redo(&mut takes).unwrap());
    assert_eq!(model.effective_take_notes(&takes, take).unwrap(), during);
}

#[test]
fn cancel_drag_rolls_back_without_touching_history_or_redo() {
    let mut takes = registry();
    let (take, [e0, e1, ..]) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    model.set_velocity(&takes, e0, velocity(20)).unwrap();
    assert_eq!(model.undo_depth(), 1);
    let after_step = model.effective_take_notes(&takes, take).unwrap();

    model.begin_drag().unwrap();
    model.nudge(&takes, e1, 7_000_000).unwrap();
    model.set_velocity(&takes, e1, velocity(55)).unwrap();
    model.set_muted(&mut takes, take, true).unwrap();
    assert!(takes.take(take).unwrap().muted());
    assert!(model.cancel_drag(&mut takes).unwrap());
    assert!(!model.drag_open());

    // Everything rolled back, including the registry mute; the earlier step
    // and redo state are untouched.
    assert!(!takes.take(take).unwrap().muted());
    assert_eq!(model.event_edit(e1), EventEdit::default());
    assert_eq!(
        model.effective_take_notes(&takes, take).unwrap(),
        after_step
    );
    assert_eq!(model.undo_depth(), 1);
    assert!(!model.can_redo());
    model.undo(&mut takes).unwrap();
    assert_eq!(model.event_edit(e0), EventEdit::default());

    // Drag misuse is explicit, never a silent no-op.
    assert_eq!(model.end_drag().unwrap_err(), EditError::NoDragActive);
    assert_eq!(
        model.cancel_drag(&mut takes).unwrap_err(),
        EditError::NoDragActive
    );
    model.begin_drag().unwrap();
    assert_eq!(model.begin_drag().unwrap_err(), EditError::DragActive);
    assert_eq!(model.undo(&mut takes).unwrap_err(), EditError::DragActive);
    assert_eq!(model.redo(&mut takes).unwrap_err(), EditError::DragActive);
    model.cancel_drag(&mut takes).unwrap();
    // A no-change drag commits nothing.
    model.begin_drag().unwrap();
    model.nudge(&takes, e0, 0).unwrap();
    assert!(!model.end_drag().unwrap());
    assert_eq!(model.undo_depth(), 0);
}

#[test]
fn new_operation_invalidates_redo() {
    let mut takes = registry();
    let (_take, [e0, ..]) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    model.nudge(&takes, e0, 1_000_000).unwrap();
    model.nudge(&takes, e0, 2_000_000).unwrap();
    assert!(model.undo(&mut takes).unwrap());
    assert!(model.can_redo());
    model.nudge(&takes, e0, 5_000_000).unwrap();
    assert!(!model.can_redo());
    assert!(!model.redo(&mut takes).unwrap());
    // +1 ms and +2 ms were recorded, the +2 ms step was undone, then +5 ms.
    assert_eq!(model.event_edit(e0).nudge_nanos, 1_000_000 + 5_000_000);
    assert_eq!(model.undo_depth(), 2);
}

#[test]
fn stale_and_missing_ids_are_rejected_without_history() {
    let mut takes = registry();
    let (take, [e0, ..]) = fixture_take(&mut takes);
    let other = recording(&mut takes);
    hit(&mut takes, other, 60, 10_000_000, 50_000_000, 90);
    takes.finish(other).unwrap();
    let mut model = EditModel::new();
    let before = model.effective_notes(&takes).unwrap();

    let missing_take_event = EventId {
        take: TakeId(999),
        sequence: 0,
    };
    assert_eq!(
        model.nudge(&takes, missing_take_event, 1).unwrap_err(),
        EditError::UnknownTake(TakeId(999))
    );
    let missing_sequence = EventId { take, sequence: 42 };
    assert_eq!(
        model.nudge(&takes, missing_sequence, 1).unwrap_err(),
        EditError::UnknownEvent(missing_sequence)
    );
    assert_eq!(
        model.delete(&takes, missing_sequence).unwrap_err(),
        EditError::UnknownEvent(missing_sequence)
    );
    assert_eq!(
        model
            .set_velocity(&takes, missing_take_event, velocity(10))
            .unwrap_err(),
        EditError::UnknownTake(TakeId(999))
    );
    assert_eq!(
        model.toggle_quantization(&takes, TakeId(999)).unwrap_err(),
        EditError::UnknownTake(TakeId(999))
    );
    assert_eq!(
        model.set_muted(&mut takes, TakeId(999), true).unwrap_err(),
        EditError::UnknownTake(TakeId(999))
    );
    assert_eq!(
        model.remove_take(&mut takes, TakeId(999)).unwrap_err(),
        EditError::UnknownTake(TakeId(999))
    );
    assert_eq!(
        model.effective_take_notes(&takes, TakeId(999)).unwrap_err(),
        EditError::UnknownTake(TakeId(999))
    );

    // Removed takes reject take-level ops but keep their events editable.
    model.remove_take(&mut takes, other).unwrap();
    assert_eq!(
        model.set_muted(&mut takes, other, true).unwrap_err(),
        EditError::RemovedTake(other)
    );
    assert_eq!(
        model.toggle_quantization(&takes, other).unwrap_err(),
        EditError::RemovedTake(other)
    );
    assert_eq!(
        model.remove_take(&mut takes, other).unwrap_err(),
        EditError::RemovedTake(other)
    );
    let other_event = EventId {
        take: other,
        sequence: 0,
    };
    model.nudge(&takes, other_event, 1_000).unwrap();
    assert_eq!(model.event_edit(other_event).nudge_nanos, 1_000);

    // Nudge accumulation overflow is a defined error, not a wrap.
    model.nudge(&takes, e0, i64::MAX).unwrap();
    assert_eq!(
        model.nudge(&takes, e0, 1).unwrap_err(),
        EditError::NudgeOverflow
    );
    assert_eq!(model.event_edit(e0).nudge_nanos, i64::MAX);

    // No failed operation touched the audible view or the history shape.
    assert_eq!(
        model.effective_notes(&takes).unwrap().len(),
        before.len() - 1
    );
    assert_eq!(
        model.undo_depth(),
        3,
        "remove_take + removed-take nudge + e0 overflow nudge"
    );
    model.undo(&mut takes).unwrap();
    assert_eq!(model.event_edit(e0), EventEdit::default());
    model.undo(&mut takes).unwrap();
    assert_eq!(model.event_edit(other_event), EventEdit::default());
}

#[test]
fn take_removal_undo_restores_originals_and_audible_notes() {
    let mut takes = registry();
    let (take_a, _) = fixture_take(&mut takes);
    let take_b = recording(&mut takes);
    let b0 = hit(&mut takes, take_b, 60, 500_000_000, 600_000_000, 90);
    takes.finish(take_b).unwrap();
    let originals_b = takes.take(take_b).unwrap().notes().to_vec();

    let mut model = EditModel::new();
    let audible_before = model.effective_notes(&takes).unwrap();
    assert!(audible_before.iter().any(|n| n.id.take == take_b));

    model.remove_take(&mut takes, take_b).unwrap();
    assert_eq!(takes.take(take_b).unwrap().removal(), Some(Removal::User));
    let audible_removed = model.effective_notes(&takes).unwrap();
    assert!(audible_removed.iter().all(|n| n.id.take != take_b));
    assert_eq!(audible_removed.len(), audible_before.len() - 1);
    // History references IDs only: the removed take's originals survive.
    assert_eq!(takes.take(take_b).unwrap().notes(), originals_b.as_slice());
    assert_eq!(originals_b[0].on().id, b0);

    // Undo restores the take; redo removes it again.
    assert!(model.undo(&mut takes).unwrap());
    assert_eq!(takes.take(take_b).unwrap().removal(), None);
    assert_eq!(model.effective_notes(&takes).unwrap(), audible_before);
    assert!(model.redo(&mut takes).unwrap());
    assert_eq!(takes.take(take_b).unwrap().removal(), Some(Removal::User));
    assert_eq!(model.effective_notes(&takes).unwrap(), audible_removed);
    // Other takes are untouched by removal/undo.
    assert!(
        model
            .effective_notes(&takes)
            .unwrap()
            .iter()
            .any(|n| n.id.take == take_a)
    );
}

#[test]
fn mute_is_reversible_through_the_registry() {
    let mut takes = registry();
    let (take, _) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    let audible = model.effective_notes(&takes).unwrap();

    model.set_muted(&mut takes, take, true).unwrap();
    assert!(takes.take(take).unwrap().muted());
    assert!(model.effective_notes(&takes).unwrap().is_empty());
    // A no-change mute records no step.
    let depth = model.undo_depth();
    model.set_muted(&mut takes, take, true).unwrap();
    assert_eq!(model.undo_depth(), depth);

    assert!(model.undo(&mut takes).unwrap());
    assert!(!takes.take(take).unwrap().muted());
    assert_eq!(model.effective_notes(&takes).unwrap(), audible);
    assert!(model.redo(&mut takes).unwrap());
    assert!(takes.take(take).unwrap().muted());
    assert!(model.effective_notes(&takes).unwrap().is_empty());
}

#[test]
fn undo_history_is_bounded_to_100_steps() {
    let mut takes = registry();
    let (_take, [e0, ..]) = fixture_take(&mut takes);
    let mut model = EditModel::new();
    for _ in 0..105 {
        model.nudge(&takes, e0, 1).unwrap();
    }
    assert_eq!(model.undo_depth(), MAX_UNDO_STEPS);
    assert_eq!(model.event_edit(e0).nudge_nanos, 105);

    for _ in 0..MAX_UNDO_STEPS {
        assert!(model.undo(&mut takes).unwrap());
    }
    assert!(
        !model.undo(&mut takes).unwrap(),
        "oldest steps were dropped"
    );
    assert_eq!(model.event_edit(e0).nudge_nanos, 5);
    assert_eq!(model.redo_depth(), MAX_UNDO_STEPS);

    for _ in 0..MAX_UNDO_STEPS {
        assert!(model.redo(&mut takes).unwrap());
    }
    assert!(!model.redo(&mut takes).unwrap());
    assert_eq!(model.event_edit(e0).nudge_nanos, 105);
    assert_eq!(model.undo_depth(), MAX_UNDO_STEPS);
}
