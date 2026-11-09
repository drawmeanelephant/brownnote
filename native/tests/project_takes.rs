#![allow(clippy::unwrap_used)]
use brownnote_native::{
    assets::AssetRegistry,
    edit::{EditModel, EditsDocument, EditsDocumentError, EventEditDocument},
    engine::Slots,
    import::SessionMedia,
    performance::{
        Channel, EventId, InstrumentId, LoopBars, NoteKey, OriginalTime, Pitch, SourceId, TakeId,
        Target, Velocity,
    },
    project::{
        Document, DocumentError, Pad, PadId,
        recovery::OpenChoice,
        session::{Error, Snapshot},
        store::Store,
    },
    takes::{PassMode, PassOptions, Removal, Takes, TakesDocument, TakesDocumentError},
};
use std::{
    error::Error as StdError,
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

type Result<T = ()> = std::result::Result<T, Box<dyn StdError>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "brownnote-project-takes-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        Ok(Self(root))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn key(source: u128, channel: u8, pitch: u8) -> Result<NoteKey> {
    Ok(NoteKey {
        source: SourceId(source),
        channel: Channel::try_from(channel)?,
        pitch: Pitch::try_from(pitch)?,
    })
}

fn at(nanos: u64) -> OriginalTime {
    OriginalTime::from_nanos(nanos)
}

/// Take 1: a pad flam (velocity 1 and 127) and two overlapping held notes on
/// one key (FIFO ends). Take 2 replaces it. Edits on take 2 only.
fn recorded() -> Result<(Takes, EditModel)> {
    let mut takes = Takes::new(LoopBars::try_from(1)?, TakeId(1));
    let one = takes.arm(PassOptions::default())?;
    takes.start(one)?;
    let pad = Target::Pad(PadId::try_from(4)?);
    let held = Target::Instrument(InstrumentId(u128::MAX));
    let flam = key(u128::MAX - 1, 0, 4)?;
    for (time, velocity) in [(200_000_000, 1), (215_000_000, 127)] {
        takes.note_on(one, pad, flam, Velocity::try_from(velocity)?, at(time))?;
        takes.note_off(one, flam, at(time))?;
    }
    let long = key(4242, 0, 60)?;
    takes.note_on(one, held, long, Velocity::try_from(100)?, at(0))?;
    takes.note_on(one, held, long, Velocity::try_from(90)?, at(100_000_000))?;
    takes.note_off(one, long, at(500_000_000))?;
    takes.note_off(one, long, at(900_000_000))?;
    takes.finish(one)?;
    let two = takes.arm(PassOptions {
        mode: PassMode::Replace,
        ..PassOptions::default()
    })?;
    takes.start(two)?;
    let hit = key(u128::MAX, 0, 2)?;
    for time in [10_000_000, 1_234_567_891] {
        takes.note_on(two, pad, hit, Velocity::try_from(127)?, at(time))?;
        takes.note_off(two, hit, at(time))?;
    }
    takes.finish(two)?;
    let mut model = EditModel::new();
    model.toggle_quantization(&takes, two)?;
    let first = EventId {
        take: two,
        sequence: 0,
    };
    model.set_velocity(&takes, first, Velocity::try_from(64)?)?;
    model.nudge(
        &takes,
        EventId {
            sequence: 1,
            ..first
        },
        -3_000_000,
    )?;
    Ok((takes, model))
}

fn snapshot() -> Result<Snapshot> {
    Ok(Snapshot::capture(
        &AssetRegistry::new(Slots::empty()),
        std::array::from_fn(|_| Pad::default()),
        0.8,
        true,
        Arc::new(SessionMedia::default()),
    )?)
}

#[test]
fn takes_and_edits_save_reopen_with_original_times_and_effective_view() -> Result {
    let fixture = Fixture::new()?;
    let (takes, model) = recorded()?;
    let document = snapshot()?.with_takes(&takes, &model)?.document;
    Store::new(fixture.0.join("project")).save(&document, &[])?;
    let reopened = Store::new(fixture.0.join("project")).open(OpenChoice::Current)?;
    assert_eq!(reopened.document(), &document);
    let value: serde_json::Value = serde_json::from_slice(&reopened.inspection().bytes)?;
    assert_eq!(value["takes"]["version"], 1);
    assert_eq!(value["takes"]["takes"][0]["removal"]["type"], "replaced_by");
    assert_eq!(value["takes"]["takes"][0]["notes"][2]["source"], "4242");
    assert_eq!(value["edits"]["quantized"][0], "2");

    let (restored, edits) = reopened.document().takes()?.ok_or("takes missing")?;
    assert_eq!(
        TakesDocument::from_takes(&restored),
        TakesDocument::from_takes(&takes)
    );
    assert_eq!(restored.loop_bars(), takes.loop_bars());
    assert_eq!(restored.selected(), Some(TakeId(2)));
    assert_eq!(
        restored.take(TakeId(1)).ok_or("take 1")?.removal(),
        Some(Removal::ReplacedBy(TakeId(2)))
    );
    // D9/D14: exact originals and FIFO durations, never quantized on disk.
    let notes: Vec<(u64, u64, u8)> = restored
        .take(TakeId(1))
        .ok_or("take 1")?
        .notes()
        .iter()
        .map(|n| {
            (
                n.on().original_time.nanos(),
                n.original_end().nanos(),
                n.on().velocity.value(),
            )
        })
        .collect();
    assert_eq!(
        notes,
        [
            (200_000_000, 200_000_000, 1),
            (215_000_000, 215_000_000, 127),
            (0, 500_000_000, 100),
            (100_000_000, 900_000_000, 90),
        ]
    );
    assert_eq!(
        restored.take(TakeId(2)).ok_or("take 2")?.notes()[1]
            .on()
            .original_time,
        at(1_234_567_891)
    );
    assert_eq!(
        edits.effective_notes(&restored)?,
        model.effective_notes(&takes)?
    );
    assert!(!edits.can_undo(), "D15: history is not saved");
    // A reopened registry continues above every saved ID.
    let mut restored = restored;
    assert_eq!(restored.arm(PassOptions::default())?, TakeId(3));
    Ok(())
}

#[test]
fn an_armed_or_recording_pass_never_saves() -> Result {
    let (mut takes, model) = recorded()?;
    let pass = takes.arm(PassOptions::default())?;
    assert!(matches!(
        snapshot()?.with_takes(&takes, &model),
        Err(Error::PassActive)
    ));
    takes.start(pass)?;
    assert!(matches!(
        snapshot()?.with_takes(&takes, &model),
        Err(Error::PassActive)
    ));
    takes.cancel(pass)?;
    assert!(snapshot()?.with_takes(&takes, &model).is_ok());
    Ok(())
}

#[test]
fn invalid_take_and_edit_documents_are_rejected() -> Result {
    let (takes, model) = recorded()?;
    let good = snapshot()?.with_takes(&takes, &model)?.document;
    let check = |change: &dyn Fn(&mut Document)| {
        let mut document = good.clone();
        change(&mut document);
        document.validate()
    };
    let takes_error = |change: &dyn Fn(&mut Document)| match check(change) {
        Err(DocumentError::Takes(error)) => Some(*error),
        _ => None,
    };
    let edits_error = |change: &dyn Fn(&mut Document)| match check(change) {
        Err(DocumentError::Edits(error)) => Some(*error),
        _ => None,
    };
    assert!(check(&|_| {}).is_ok());
    assert_eq!(
        takes_error(&|d| d.takes.as_mut().unwrap().version = 2),
        Some(TakesDocumentError::UnsupportedVersion(2))
    );
    assert_eq!(
        takes_error(&|d| d.takes.as_mut().unwrap().loop_bars = 3),
        Some(TakesDocumentError::InvalidLoopBars(3))
    );
    assert_eq!(
        takes_error(&|d| d.takes.as_mut().unwrap().next_take = "2".into()),
        Some(TakesDocumentError::NextTakeNotAbove(TakeId(2)))
    );
    assert_eq!(
        takes_error(&|d| { d.takes.as_mut().unwrap().selected = Some("1".into()) }),
        Some(TakesDocumentError::UnknownTake(TakeId(1))),
        "a replaced take cannot be selected"
    );
    assert_eq!(
        takes_error(&|d| d.takes.as_mut().unwrap().takes[1].id = "02".into()),
        Some(TakesDocumentError::InvalidIdentity("02".into()))
    );
    assert_eq!(
        takes_error(&|d| {
            let take = &mut d.takes.as_mut().unwrap().takes[0];
            take.notes[2].end_nanos = 1;
            take.notes[2].on_nanos = 2;
        }),
        Some(TakesDocumentError::InvalidNote(EventId {
            take: TakeId(1),
            sequence: 2
        })),
        "an end before its start is not a note"
    );
    assert_eq!(
        takes_error(&|d| {
            let take = &mut d.takes.as_mut().unwrap().takes[0];
            take.notes.swap(0, 1);
        }),
        Some(TakesDocumentError::SequenceOrder(EventId {
            take: TakeId(1),
            sequence: 0
        }))
    );
    assert_eq!(
        edits_error(&|d| {
            d.edits
                .as_mut()
                .unwrap_or_else(|| unreachable!())
                .events
                .push(EventEditDocument {
                    take: "2".into(),
                    sequence: 9,
                    deleted: true,
                    velocity: None,
                    nudge_nanos: 0,
                })
        }),
        Some(EditsDocumentError::UnknownEvent(EventId {
            take: TakeId(2),
            sequence: 9
        }))
    );
    assert_eq!(
        edits_error(&|d| d.takes = None),
        Some(EditsDocumentError::UnknownTake(TakeId(2))),
        "edits without takes reference nothing"
    );
    let mut value = serde_json::to_value(&good)?;
    value["takes"]["takes"][0]["notes"][0]["quantized"] = true.into();
    assert!(
        serde_json::from_value::<Document>(value).is_err(),
        "no unknown fields"
    );
    Ok(())
}

#[test]
fn projects_without_takes_open_empty_and_save_without_the_fields() -> Result {
    let legacy: Document =
        serde_json::from_slice(include_bytes!("fixtures/project-v1/project.json"))?;
    assert_eq!(legacy.takes, None);
    assert_eq!(legacy.edits, None);
    assert!(legacy.takes()?.is_none());
    let encoded: serde_json::Value = serde_json::from_slice(&Document::default().encode()?)?;
    assert!(encoded.get("takes").is_none());
    assert!(encoded.get("edits").is_none());
    let empty = EditsDocument::from_model(&EditModel::new());
    assert!(empty.is_empty());
    Ok(())
}
