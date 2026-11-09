#![allow(clippy::unwrap_used)]
use brownnote_native::{
    edit::{EditModel, MAX_UNDO_STEPS},
    performance::{
        Channel, EventId, LoopBars, MusicalPosition, NoteKey, OriginalTime, Pitch, Quantization,
        SourceId, TakeId, Target, Tempo, Velocity,
    },
    project::PadId,
    takes::{PassOptions, Takes},
};

fn committed_flam(model: &mut Takes, take: TakeId) -> [EventId; 2] {
    let key = NoteKey {
        source: SourceId(7),
        channel: Channel::try_from(0).unwrap(),
        pitch: Pitch::try_from(60).unwrap(),
    };
    let target = Target::Pad(PadId::try_from(0).unwrap());
    let mut ids = Vec::new();
    for (start, end, vel) in [(10_000_000, 50_000_000, 90), (25_000_000, 60_000_000, 110)] {
        let id = model
            .note_on(
                take,
                target,
                key,
                Velocity::try_from(vel).unwrap(),
                OriginalTime::from_nanos(start),
            )
            .unwrap();
        assert_eq!(
            model
                .note_off(take, key, OriginalTime::from_nanos(end))
                .unwrap(),
            id
        );
        ids.push(id);
    }
    model.finish(take).unwrap();
    ids.try_into().unwrap()
}

#[test]
fn exported_edit_model_restores_originals_and_undoes_edits() {
    let mut takes = Takes::new(LoopBars::try_from(8).unwrap(), TakeId(1));
    let take = takes.arm(PassOptions::default()).unwrap();
    takes.start(take).unwrap();
    let [e0, e1] = committed_flam(&mut takes, take);
    let tempo = Tempo::default();

    let mut model = EditModel::new();
    assert_eq!(model.quantization(take), Quantization::Off);
    assert_eq!(
        model.toggle_quantization(&takes, take).unwrap(),
        Quantization::Sixteenth
    );
    let snapped: Vec<_> = model
        .effective_take_notes(&takes, take)
        .unwrap()
        .iter()
        .map(|note| note.start)
        .collect();
    assert_eq!(snapped.len(), 2);
    assert_ne!(
        snapped[0],
        MusicalPosition::from_original(OriginalTime::from_nanos(10_000_000), tempo)
    );
    assert_eq!(
        model.toggle_quantization(&takes, take).unwrap(),
        Quantization::Off
    );

    // D13: with quantization off, exact original positions return.
    for (note, id) in model
        .effective_take_notes(&takes, take)
        .unwrap()
        .iter()
        .zip([e0, e1])
    {
        assert_eq!(note.id, id);
        let original = takes
            .take(take)
            .unwrap()
            .notes()
            .iter()
            .find(|n| n.on().id == id)
            .unwrap();
        assert_eq!(
            note.start,
            MusicalPosition::from_original(original.on().original_time, tempo)
        );
        assert_eq!(
            note.end,
            MusicalPosition::from_original(original.original_end(), tempo)
        );
    }

    // Delete + undo, and one drag = one undo step, through the public export.
    model.delete(&takes, e1).unwrap();
    assert_eq!(model.effective_take_notes(&takes, take).unwrap().len(), 1);
    assert!(model.undo(&mut takes).unwrap());
    assert_eq!(model.effective_take_notes(&takes, take).unwrap().len(), 2);

    model.begin_drag().unwrap();
    model.nudge(&takes, e0, 1_000_000).unwrap();
    model.nudge(&takes, e0, 2_000_000).unwrap();
    assert!(model.end_drag().unwrap());
    assert_eq!(model.undo_depth(), 3);
    assert!(model.undo(&mut takes).unwrap());
    assert_eq!(model.event_edit(e0).nudge_nanos, 0);

    for _ in 0..(MAX_UNDO_STEPS + 5) {
        model.nudge(&takes, e0, 1).unwrap();
    }
    assert_eq!(model.undo_depth(), MAX_UNDO_STEPS);
}
