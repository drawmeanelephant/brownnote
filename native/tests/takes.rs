#![allow(clippy::unwrap_used)]
use brownnote_native::{
    performance::{
        Channel, LoopBars, NoteKey, OriginalTime, Pitch, SourceId, TakeId, Target, Velocity,
    },
    project::PadId,
    takes::{PassOptions, Takes},
};

#[test]
fn exported_api_keeps_same_grid_flams_and_fifo_releases() {
    let mut model = Takes::new(LoopBars::try_from(1).unwrap(), TakeId(1));
    let take = model.arm(PassOptions::default()).unwrap();
    model.start(take).unwrap();
    let key = NoteKey {
        source: SourceId(7),
        channel: Channel::try_from(0).unwrap(),
        pitch: Pitch::try_from(60).unwrap(),
    };
    let target = Target::Pad(PadId::try_from(0).unwrap());
    let mut ids = Vec::new();
    for (time, velocity) in [(10_000_000, 20), (25_000_000, 110)] {
        ids.push(
            model
                .note_on(
                    take,
                    target,
                    key,
                    Velocity::try_from(velocity).unwrap(),
                    OriginalTime::from_nanos(time),
                )
                .unwrap(),
        );
    }
    assert_ne!(ids[0], ids[1]);
    for (id, time) in ids.iter().zip([40_000_000, 60_000_000]) {
        assert_eq!(
            model
                .note_off(take, key, OriginalTime::from_nanos(time))
                .unwrap(),
            *id
        );
    }
    model.finish(take).unwrap();
    let notes: Vec<_> = model.audible_notes().map(|note| note.on()).collect();
    assert_eq!(notes.len(), 2);
    for (index, (time, velocity)) in [(10_000_000, 20), (25_000_000, 110)]
        .into_iter()
        .enumerate()
    {
        assert_eq!(notes[index].id, ids[index]);
        assert_eq!(notes[index].original_time, OriginalTime::from_nanos(time));
        assert_eq!(notes[index].velocity, Velocity::try_from(velocity).unwrap());
    }
}
