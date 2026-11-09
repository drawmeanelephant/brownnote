#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::performance::{Channel, InstrumentId, Pitch, Quantization, SourceId};
use crate::project::{AssetId, PadId};

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
fn recording(model: &mut Takes, options: PassOptions) -> TakeId {
    let id = model.arm(options).unwrap();
    model.start(id).unwrap();
    id
}
fn hit(model: &mut Takes, take: TakeId, start: u64, end: u64) -> EventId {
    let key = key(7, 0, 60);
    let id = model
        .note_on(take, pad(), key, velocity(90), time(start))
        .unwrap();
    assert_eq!(model.note_off(take, key, time(end)).unwrap(), id);
    id
}
fn committed(model: &mut Takes, start: u64) -> TakeId {
    let take = recording(model, PassOptions::default());
    hit(model, take, start, start + 10);
    model.finish(take).unwrap();
    take
}
fn audible(model: &Takes) -> Vec<EventId> {
    model.audible_notes().map(|note| note.on().id).collect()
}
fn originals(model: &Takes) -> Vec<(TakeId, Vec<Note>)> {
    model
        .takes()
        .map(|take| (take.id(), take.notes().to_vec()))
        .collect()
}

struct FixtureNote {
    pass: u128,
    key: NoteKey,
    velocity: Velocity,
    target: Target,
    start: u64,
    end: u64,
}
fn overdub_fixture() -> Vec<FixtureNote> {
    include_str!("fixtures/overdubs.csv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let row: Vec<_> = line.split(',').collect();
            FixtureNote {
                pass: row[0].parse().unwrap(),
                key: key(
                    row[1].parse().unwrap(),
                    row[2].parse().unwrap(),
                    row[3].parse().unwrap(),
                ),
                velocity: velocity(row[4].parse().unwrap()),
                target: match row[5] {
                    "pad" => pad(),
                    "instrument" => Target::Instrument(InstrumentId(21)),
                    "sample" => Target::Sample(AssetId::of(b"fixture sample identity")),
                    _ => panic!("invalid contract fixture target"),
                },
                start: row[6].parse().unwrap(),
                end: row[7].parse().unwrap(),
            }
        })
        .collect()
}
fn three_overdubs() -> Takes {
    let mut model = registry();
    let fixture = overdub_fixture();
    for pass in 0..3 {
        let take = recording(&mut model, PassOptions::default());
        assert_eq!(take, TakeId(700 + pass));
        let rows: Vec<_> = fixture.iter().filter(|row| row.pass == pass).collect();
        for (sequence, row) in rows.iter().enumerate() {
            assert_eq!(
                model
                    .note_on(take, row.target, row.key, row.velocity, time(row.start))
                    .unwrap(),
                EventId {
                    take,
                    sequence: sequence as u64
                }
            );
        }
        // Different sources finish first; storage must still retain onset order.
        let mut releases = rows.clone();
        releases.sort_by_key(|row| row.end);
        for row in releases {
            model.note_off(take, row.key, time(row.end)).unwrap();
        }
        model.finish(take).unwrap();
        for (note, row) in model.take(take).unwrap().notes().iter().zip(rows) {
            assert_eq!(note.on().key, row.key);
            assert_eq!(note.on().velocity, row.velocity);
            assert_eq!(note.on().target, row.target);
            assert_eq!(note.on().original_time, time(row.start));
            assert_eq!(note.original_end(), time(row.end));
        }
    }
    model
}

#[test]
fn takes_defaults_and_rejected_armed_capture_do_not_consume_event_ids() {
    let mut model = registry();
    assert_eq!(model.tempo().milli_bpm(), 116_000);
    assert_eq!(model.loop_bars(), bars(8));
    assert_eq!(model.selected(), None);
    let take = model.arm(PassOptions::default()).unwrap();
    let pass = model.pass(take).unwrap();
    assert_eq!(pass.phase(), TakePhase::Armed);
    assert_eq!(
        pass.options(),
        PassOptions {
            mode: PassMode::Overdub,
            count_in: CountIn::Off,
            metronome: false,
        }
    );
    assert_eq!(pass.recording_tempo(), tempo(116_000));
    assert_eq!(
        model.note_on(take, pad(), key(7, 0, 60), velocity(90), time(17)),
        Err(TakesError::NotRecording(take))
    );
    assert_eq!(model.finish(take), Err(TakesError::NotRecording(take)));
    assert!(model.take(take).is_none());
    model.start(take).unwrap();
    assert_eq!(
        model.start(take),
        Err(TakesError::Performance(PerformanceError::NotArmed))
    );
    assert_eq!(hit(&mut model, take, 17, 23).sequence, 0);
    model.finish(take).unwrap();
    assert_eq!(model.selected(), Some(take));
    assert_eq!(model.take(take).unwrap().notes().len(), 1);
    assert_eq!(model.pass(take).map(Pass::phase), None);
}

#[test]
fn takes_three_overdubs_keep_same_grid_flams_overlaps_velocities_and_sources() {
    let model = three_overdubs();
    assert_eq!(model.takes().len(), 3);
    assert_eq!(audible(&model).len(), 12);
    assert_eq!(model.selected(), Some(TakeId(702)));
    for take in model.takes() {
        assert_eq!(take.notes().len(), 4);
        assert_eq!(take.timing().quantization(), Quantization::Off);
        let mut quantized = TakeTiming::new(take.id(), tempo(116_000));
        quantized.set_quantization(Quantization::Sixteenth);
        for note in take.notes() {
            // Independent fixture expectation: every onset shares a grid cell.
            assert_eq!(
                quantized
                    .view(note)
                    .unwrap()
                    .start
                    .at_tempo_nanos(tempo(116_000))
                    .unwrap(),
                129_310_345
            );
        }
        let notes = take.notes();
        assert_eq!(
            notes[1].on().original_time.nanos() - notes[0].on().original_time.nanos(),
            15_000_008
        );
        assert!(notes[1].on().original_time < notes[0].original_end());
        assert_ne!(notes[0].on().id, notes[1].on().id);
    }
}

#[test]
fn takes_selection_mute_remove_and_restore_never_change_originals_or_solo() {
    let mut model = three_overdubs();
    let before = originals(&model);
    let all = audible(&model);
    model.select(TakeId(700)).unwrap();
    assert_eq!(audible(&model), all);
    model.set_muted(TakeId(701), true).unwrap();
    assert_eq!(audible(&model).len(), 8);
    model.select(TakeId(701)).unwrap();
    assert_eq!(model.selected(), Some(TakeId(701)));
    model.remove(TakeId(701)).unwrap();
    assert_eq!(model.selected(), None);
    assert_eq!(
        model.take(TakeId(701)).unwrap().removal(),
        Some(Removal::User)
    );
    assert_eq!(
        model.remove(TakeId(701)),
        Err(TakesError::Removed(TakeId(701)))
    );
    assert_eq!(
        model.select(TakeId(701)),
        Err(TakesError::Removed(TakeId(701)))
    );
    assert_eq!(
        model.set_muted(TakeId(701), false),
        Err(TakesError::Removed(TakeId(701)))
    );
    model.restore(TakeId(701)).unwrap();
    assert!(model.take(TakeId(701)).unwrap().muted());
    assert_eq!(model.selected(), None);
    assert_eq!(audible(&model).len(), 8);
    assert_eq!(
        model.restore(TakeId(701)),
        Err(TakesError::NotRemoved(TakeId(701)))
    );
    model.set_muted(TakeId(701), false).unwrap();
    assert_eq!(audible(&model), all);
    model.remove(TakeId(700)).unwrap();
    assert_eq!(audible(&model).len(), 8);
    model.restore(TakeId(700)).unwrap();
    assert_eq!(audible(&model), all);
    assert_eq!(originals(&model), before);
}

#[test]
fn takes_shortened_loop_visibility_matches_fractional_and_exact_boundary_fixture() {
    let rows: Vec<Vec<u64>> = include_str!("fixtures/loop-visibility.csv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split(',').map(|cell| cell.parse().unwrap()).collect())
        .collect();
    let mut model = registry();
    let mut expected = Vec::new();
    for milli_bpm in [116_000, 120_000] {
        model.set_tempo(tempo(milli_bpm)).unwrap();
        let take = recording(&mut model, PassOptions::default());
        for row in rows.iter().filter(|row| row[0] == u64::from(milli_bpm)) {
            // The held end can extend beyond the loop without clipping.
            let id = hit(&mut model, take, row[1], row[1] + 3_000_000_000);
            expected.push((id, &row[2..]));
        }
        model.finish(take).unwrap();
    }
    let before = originals(&model);
    for _ in 0..3 {
        for (column, length) in [(0, 1), (3, 8), (1, 2), (2, 4), (0, 1)] {
            model.set_loop_bars(bars(length)).unwrap();
            let visible: Vec<_> = expected
                .iter()
                .filter(|(_, flags)| flags[column] == 1)
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(audible(&model), visible);
        }
        model.set_tempo(tempo(58_000)).unwrap();
        assert_eq!(originals(&model), before);
        // Tempo edits keep musical visibility, not original wall-clock length.
        let visible: Vec<_> = expected
            .iter()
            .filter(|(_, flags)| flags[0] == 1)
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(audible(&model), visible);
    }
    assert_eq!(originals(&model), before);
    for invalid in [0, 3, 5, 7, 9, u8::MAX] {
        assert_eq!(
            LoopBars::try_from(invalid),
            Err(PerformanceError::InvalidLoopBars)
        );
    }
}

#[test]
fn takes_replace_cancel_and_failed_finish_preserve_previous_takes_atomically() {
    let mut model = three_overdubs();
    model.set_muted(TakeId(701), true).unwrap();
    model.select(TakeId(700)).unwrap();
    let before = originals(&model);
    let active = audible(&model);
    let options = PassOptions {
        mode: PassMode::Replace,
        count_in: CountIn::OneBar,
        metronome: true,
    };
    let cancelled = model.arm(options).unwrap();
    assert_eq!(model.pass(cancelled).unwrap().options(), options);
    assert_eq!(audible(&model), active);
    model.start(cancelled).unwrap();
    model
        .note_on(cancelled, pad(), key(7, 0, 60), velocity(90), time(10))
        .unwrap();
    assert_eq!(
        model.finish(cancelled),
        Err(TakesError::HeldNotes(cancelled))
    );
    assert_eq!(model.selected(), Some(TakeId(700)));
    assert_eq!(audible(&model), active);
    assert_eq!(originals(&model), before);
    model.cancel(cancelled).unwrap();
    assert_eq!(
        model.cancel(cancelled),
        Err(TakesError::UnknownPass(cancelled))
    );
    assert_eq!(audible(&model), active);
    assert_eq!(originals(&model), before);
    let replacement = recording(&mut model, options);
    assert_eq!(replacement, TakeId(cancelled.0 + 1));
    assert_eq!(
        model.finish(replacement),
        Err(TakesError::EmptyPass(replacement))
    );
    assert_eq!(audible(&model), active);
    assert_eq!(originals(&model), before);
    assert_eq!(model.selected(), Some(TakeId(700)));
    let event = hit(&mut model, replacement, 99, 101);
    assert_eq!(event.sequence, 0);
    model.finish(replacement).unwrap();
    assert_eq!(audible(&model), vec![event]);
    assert_eq!(model.selected(), Some(replacement));
    for id in [700, 701, 702] {
        assert_eq!(
            model.take(TakeId(id)).unwrap().removal(),
            Some(Removal::ReplacedBy(replacement))
        );
    }
    assert_eq!(&originals(&model)[..3], before);
    model.remove(replacement).unwrap();
    assert!(audible(&model).is_empty());
    // Removing a replacement is not a hidden "undo all"; restore is explicit.
    for id in [700, 701, 702] {
        model.restore(TakeId(id)).unwrap();
    }
    assert_eq!(audible(&model), active);
    assert!(model.take(TakeId(701)).unwrap().muted());
    // Replace does not become the default of the next pass.
    let overdub = recording(&mut model, PassOptions::default());
    hit(&mut model, overdub, 20, 30);
    model.finish(overdub).unwrap();
    assert_eq!(audible(&model).len(), active.len() + 1);
}

#[test]
fn takes_replace_snapshots_only_live_takes_at_arm_and_preserves_new_overdubs() {
    let mut model = registry();
    let old = committed(&mut model, 10);
    let removed = committed(&mut model, 20);
    model.remove(removed).unwrap();
    let replacement = recording(
        &mut model,
        PassOptions {
            mode: PassMode::Replace,
            ..PassOptions::default()
        },
    );
    let newer = committed(&mut model, 30);
    model.restore(removed).unwrap();
    hit(&mut model, replacement, 40, 50);
    model.finish(replacement).unwrap();
    assert_eq!(
        model.take(old).unwrap().removal(),
        Some(Removal::ReplacedBy(replacement))
    );
    assert_eq!(model.take(removed).unwrap().removal(), None);
    assert_eq!(model.take(newer).unwrap().removal(), None);
    assert_eq!(audible(&model).len(), 3);
    // A user removal made during the pass retains its own restoration boundary.
    let replacing = recording(
        &mut model,
        PassOptions {
            mode: PassMode::Replace,
            ..PassOptions::default()
        },
    );
    model.remove(removed).unwrap();
    hit(&mut model, replacing, 60, 70);
    model.finish(replacing).unwrap();
    assert_eq!(model.take(removed).unwrap().removal(), Some(Removal::User));
    model.restore(old).unwrap();
    assert_eq!(audible(&model).len(), 2);
}

#[test]
fn takes_tempo_and_loop_lock_until_every_armed_or_recording_pass_is_resolved() {
    let mut model = registry();
    let armed = model.arm(PassOptions::default()).unwrap();
    let playing = recording(&mut model, PassOptions::default());
    let locked = Err(TakesError::Performance(PerformanceError::TempoLocked));
    assert_eq!(model.set_tempo(tempo(120_000)), locked);
    assert_eq!(model.set_tempo(tempo(116_000)), locked);
    assert_eq!(model.set_loop_bars(bars(1)), Err(TakesError::LoopLocked));
    hit(&mut model, playing, 1, 2);
    model.finish(playing).unwrap();
    assert_eq!(model.set_tempo(tempo(120_000)), locked);
    assert_eq!(model.set_loop_bars(bars(1)), Err(TakesError::LoopLocked));
    assert_eq!(model.pass(armed).unwrap().phase(), TakePhase::Armed);
    model.start(armed).unwrap();
    assert_eq!(model.finish(armed), Err(TakesError::EmptyPass(armed)));
    assert_eq!(model.set_tempo(tempo(120_000)), locked);
    model.cancel(armed).unwrap();
    model.set_tempo(tempo(120_000)).unwrap();
    model.set_loop_bars(bars(1)).unwrap();
    let next = recording(&mut model, PassOptions::default());
    assert_eq!(model.pass(next).unwrap().recording_tempo(), tempo(120_000));
    assert_eq!(
        model.take(playing).unwrap().timing().recording_tempo(),
        tempo(116_000)
    );
    model.cancel(next).unwrap();
    assert_eq!(model.loop_bars(), bars(1));
}

#[test]
fn takes_fifo_rejections_keep_held_notes_sources_and_event_sequence() {
    let mut model = registry();
    let take = recording(&mut model, PassOptions::default());
    let a = key(7, 0, 60);
    let b = key(8, 0, 60);
    let first = model
        .note_on(take, pad(), a, velocity(1), time(100))
        .unwrap();
    let second = model
        .note_on(take, pad(), a, velocity(127), time(110))
        .unwrap();
    let other = model
        .note_on(take, pad(), b, velocity(33), time(105))
        .unwrap();
    assert_eq!(
        model.note_off(take, a, time(99)),
        Err(TakesError::Performance(PerformanceError::EndBeforeStart))
    );
    assert_eq!(
        model.note_off(take, key(7, 1, 60), time(200)),
        Err(TakesError::Performance(PerformanceError::UnmatchedNoteOff(
            key(7, 1, 60)
        )))
    );
    assert_eq!(model.pass(take).unwrap().held_count(), 3);
    assert_eq!(model.note_off(take, b, time(120)).unwrap(), other);
    assert_eq!(model.note_off(take, a, time(130)).unwrap(), first);
    assert_eq!(
        model.note_off(take, a, time(109)),
        Err(TakesError::Performance(PerformanceError::EndBeforeStart))
    );
    assert_eq!(model.pass(take).unwrap().held_count(), 1);
    assert_eq!(model.finish(take), Err(TakesError::HeldNotes(take)));
    assert_eq!(model.note_off(take, a, time(140)).unwrap(), second);
    assert_eq!(
        model.note_off(take, a, time(150)),
        Err(TakesError::Performance(PerformanceError::UnmatchedNoteOff(
            a
        )))
    );
    let next = hit(&mut model, take, 160, 170);
    assert_eq!(next.sequence, 3);
    let completed: Vec<_> = model
        .pass(take)
        .unwrap()
        .completed_notes()
        .map(|note| note.on().id)
        .collect();
    assert_eq!(completed, vec![first, second, other, next]);
    model.finish(take).unwrap();
    assert_eq!(audible(&model), completed);
}

#[test]
fn takes_completed_originals_keep_extreme_times_and_post_record_musical_positions() {
    let mut model = registry();
    model.set_tempo(tempo(u32::MAX)).unwrap();
    let take = recording(&mut model, PassOptions::default());
    hit(&mut model, take, 0, 0);
    hit(&mut model, take, u64::MAX - 1, u64::MAX);
    model.finish(take).unwrap();
    let before = originals(&model);
    let stored = model.take(take).unwrap();
    assert_eq!(stored.notes().len(), 2);
    assert_eq!(stored.notes()[1].duration_nanos(), 1);
    let extreme = stored.timing().view(&stored.notes()[1]).unwrap();
    assert_eq!(
        extreme.start.at_tempo_nanos(tempo(u32::MAX)).unwrap(),
        u64::MAX - 1
    );
    assert_eq!(
        extreme.end.at_tempo_nanos(tempo(u32::MAX)).unwrap(),
        u64::MAX
    );
    model.set_tempo(tempo(1)).unwrap();
    assert_eq!(
        extreme.start.at_tempo_nanos(model.tempo()),
        Err(PerformanceError::Overflow)
    );
    assert_eq!(audible(&model).len(), 1);
    assert_eq!(originals(&model), before);

    let mut model = registry();
    model.set_tempo(tempo(120_000)).unwrap();
    let take = committed(&mut model, 500_000_000);
    model.set_tempo(tempo(60_000)).unwrap();
    let take = model.take(take).unwrap();
    let view = take.timing().view(&take.notes()[0]).unwrap();
    assert_eq!(
        view.start.at_tempo_nanos(model.tempo()).unwrap(),
        1_000_000_000
    );
    assert_eq!(
        view.end.at_tempo_nanos(model.tempo()).unwrap(),
        1_000_000_020
    );
    assert_eq!(take.notes()[0].on().original_time.nanos(), 500_000_000);
}

#[test]
fn takes_unknown_operations_and_take_id_exhaustion_preserve_prior_state() {
    let mut model = Takes::new(bars(1), TakeId(u128::MAX));
    let last = committed(&mut model, 10);
    assert_eq!(last, TakeId(u128::MAX));
    let before = originals(&model);
    for _ in 0..2 {
        assert_eq!(
            model.arm(PassOptions::default()),
            Err(TakesError::TakeIdsExhausted)
        );
        assert_eq!(
            model.start(TakeId(0)),
            Err(TakesError::UnknownPass(TakeId(0)))
        );
        assert_eq!(model.finish(last), Err(TakesError::UnknownPass(last)));
        assert_eq!(model.cancel(last), Err(TakesError::UnknownPass(last)));
        assert_eq!(
            model.select(TakeId(0)),
            Err(TakesError::UnknownTake(TakeId(0)))
        );
        assert_eq!(
            model.set_muted(TakeId(0), true),
            Err(TakesError::UnknownTake(TakeId(0)))
        );
        assert_eq!(
            model.remove(TakeId(0)),
            Err(TakesError::UnknownTake(TakeId(0)))
        );
        assert_eq!(
            model.restore(TakeId(0)),
            Err(TakesError::UnknownTake(TakeId(0)))
        );
        assert_eq!(
            model.note_on(last, pad(), key(7, 0, 60), velocity(1), time(0)),
            Err(TakesError::UnknownPass(last))
        );
        assert_eq!(originals(&model), before);
        assert_eq!(model.selected(), Some(last));
        model.set_tempo(tempo(120_000)).unwrap();
    }
}

#[test]
fn takes_last_event_id_is_accepted_once_and_exhaustion_keeps_held_notes() {
    let mut model = registry();
    let prior = committed(&mut model, 1);
    let take = recording(&mut model, PassOptions::default());
    // Boundary fixture: simulate a pass that has reached its last sequence.
    model.passes.get_mut(&take).unwrap().next_sequence = Some(u64::MAX);
    let id = model
        .note_on(take, pad(), key(7, 0, 60), velocity(90), time(5))
        .unwrap();
    assert_eq!(
        id,
        EventId {
            take,
            sequence: u64::MAX
        }
    );
    for _ in 0..2 {
        assert_eq!(
            model.note_on(take, pad(), key(7, 0, 60), velocity(90), time(6)),
            Err(TakesError::EventIdsExhausted)
        );
        assert_eq!(model.pass(take).unwrap().held_count(), 1);
        assert_eq!(model.take(prior).unwrap().notes().len(), 1);
    }
    assert_eq!(model.note_off(take, key(7, 0, 60), time(7)).unwrap(), id);
    model.finish(take).unwrap();
    assert_eq!(model.take(take).unwrap().notes()[0].on().id, id);
    assert_eq!(audible(&model).len(), 2);
}
