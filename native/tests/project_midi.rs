use brownnote_native::{
    assets::AssetRegistry,
    engine::Slots,
    import::SessionMedia,
    midi::{
        input::{Controller, MappingDocumentError, MappingsDocument},
        mapping::{ControlIntent, Intent, Mappings, NoteTarget},
        parser::parse_message,
    },
    performance::{InstrumentId, SourceId, TakeId},
    project::{
        Document, DocumentError, Pad,
        recovery::OpenChoice,
        session::{Completion, Error, Request, Service, Snapshot},
        store::{CommitState, Store, StoreError},
    },
};
use serde_json::json;
use std::{
    error::Error as StdError,
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

type Result<T = ()> = std::result::Result<T, Box<dyn StdError>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "brownnote-project-midi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        Ok(Self(root))
    }

    fn root(&self) -> PathBuf {
        self.0.join("project")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mappings() -> Result<Mappings> {
    let document: MappingsDocument = serde_json::from_value(json!({
        "version": 1,
        "mappings": [
            {"type": "note", "channel": 1, "pitch": 40,
             "target": {"type": "one_shot", "pad": 3}},
            {"type": "note", "channel": 2, "pitch": 40,
             "target": {"type": "held",
                        "instrument": "340282366920938463463374607431768211455"}},
            {"type": "note", "channel": 15, "pitch": 127,
             "target": {"type": "one_shot", "pad": 15}},
            {"type": "controller", "channel": 1, "number": 64, "action": "sustain"},
            {"type": "controller", "channel": 2, "number": 64, "action": "panic"},
            {"type": "controller", "channel": 15, "number": 123, "action": "all_notes_off"}
        ]
    }))?;
    Ok(document.to_mappings()?)
}

fn document() -> Result<Document> {
    Ok(Document {
        midi_mappings: Some(MappingsDocument::from_mappings(&mappings()?)),
        ..Document::default()
    })
}

fn assert_channel_recall(mappings: &Mappings) -> Result {
    let resolve = |bytes: &[u8]| {
        let message = parse_message(bytes).map_err(|error| format!("{error:?}"))?;
        Ok::<_, Box<dyn StdError>>(mappings.resolve(SourceId(9), message))
    };
    assert!(matches!(
        resolve(&[0x91, 40, 127])?,
        Some(Intent::Start {
            target: NoteTarget::OneShot(pad),
            velocity,
            ..
        }) if pad.index() == 3 && velocity.value() == 127
    ));
    assert!(matches!(
        resolve(&[0x92, 40, 1])?,
        Some(Intent::Start {
            target: NoteTarget::Held(InstrumentId(u128::MAX)),
            velocity,
            ..
        }) if velocity.value() == 1
    ));
    assert_eq!(resolve(&[0x90, 40, 127])?, None);
    assert!(matches!(
        resolve(&[0x9f, 127, 127])?,
        Some(Intent::Start { target: NoteTarget::OneShot(pad), .. }) if pad.index() == 15
    ));
    for (bytes, expected) in [
        ([0xb1, 64, 127], ControlIntent::Sustain(true)),
        ([0xb1, 64, 0], ControlIntent::Sustain(false)),
        ([0xb2, 64, 127], ControlIntent::Panic),
        ([0xbf, 123, 0], ControlIntent::AllNotesOff),
    ] {
        assert!(matches!(
            resolve(&bytes)?,
            Some(Intent::Control { action, .. }) if action == expected
        ));
    }
    assert_eq!(resolve(&[0xb0, 64, 127])?, None);
    Ok(())
}

fn snapshot(mappings: &Mappings) -> Result<Snapshot> {
    Ok(Snapshot::capture(
        &AssetRegistry::new(Slots::empty()),
        std::array::from_fn(|_| Pad::default()),
        0.8,
        true,
        Arc::new(SessionMedia::default()),
    )?
    .with_midi_mappings(mappings))
}

fn complete(service: &Service, request: Request) -> Result<Completion> {
    let job = service.start(request)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = job.take() {
            return Ok(result?);
        }
        if Instant::now() >= deadline {
            return Err("project worker did not complete".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn save_reopen_recalls_channel_keyed_notes_cc_and_instrument_ids() -> Result {
    let fixture = Fixture::new()?;
    let document = document()?;
    let receipt = Store::new(fixture.root()).save(&document, &[])?;
    let prepared = Store::new(fixture.root()).open(OpenChoice::Current)?;
    assert_eq!(prepared.document(), &document);
    assert_eq!(prepared.inspection().generation, receipt.generation);
    let value: serde_json::Value = serde_json::from_slice(&prepared.inspection().bytes)?;
    assert_eq!(value["version"], 1);
    assert_eq!(value["midi_mappings"]["version"], 1);
    assert_eq!(
        value["midi_mappings"]["mappings"][1]["target"]["instrument"],
        "340282366920938463463374607431768211455"
    );
    assert!(value.get("midi_device").is_none());
    let mut controller = Controller::new(TakeId(7), Arc::default());
    controller.set_mappings(prepared.document().mappings()?);
    assert_channel_recall(controller.mappings())
}

#[test]
fn historical_v1_without_mappings_opens_with_an_empty_bank() -> Result {
    let fixture = Fixture::new()?;
    let root = fixture.root();
    fs::create_dir_all(root.join("media"))?;
    let bytes = include_bytes!("fixtures/project-v1/project.json");
    fs::write(root.join("project.json"), bytes)?;
    let project = Store::new(root.clone()).open(OpenChoice::Current)?;
    assert_eq!(project.inspection().bytes, bytes);
    assert_eq!(project.document().midi_mappings, None);
    assert!(project.document().mappings()?.entries().is_empty());
    match complete(
        &Service::new()?,
        Request::Open {
            root: root.clone(),
            choice: OpenChoice::Current,
        },
    )? {
        Completion::Opened(prepared) => {
            let mut controller = Controller::new(TakeId(7), Arc::default());
            controller.set_mappings(mappings()?);
            controller.set_mappings(prepared.mappings);
            assert!(controller.mappings().entries().is_empty());
        }
        _ => return Err("legacy open did not prepare a project".into()),
    }
    assert_eq!(fs::read(root.join("project.json"))?, bytes);
    assert!(!root.join(".brownnote-store").exists());
    let encoded: serde_json::Value = serde_json::from_slice(&Document::default().encode()?)?;
    assert!(encoded.get("midi_mappings").is_none());
    let mut null = encoded;
    null["midi_mappings"] = serde_json::Value::Null;
    assert!(
        Document::decode(&serde_json::to_vec(&null)?)?
            .mappings()?
            .entries()
            .is_empty()
    );
    Ok(())
}

#[test]
fn unsupported_project_versions_still_fail_before_schema_or_store_changes() -> Result {
    let fixture = Fixture::new()?;
    for (version, expected) in [
        (0, DocumentError::OlderVersion(0)),
        (2, DocumentError::NewerVersion(2)),
        (u32::MAX, DocumentError::NewerVersion(u32::MAX)),
    ] {
        let bytes = serde_json::to_vec(&json!({
            "version": version, "midi_mappings": {"future_schema": true}
        }))?;
        assert_eq!(Document::decode(&bytes), Err(expected.clone()));
        let mut document = document()?;
        document.version = version;
        let failure = Store::new(fixture.root())
            .save(&document, &[])
            .err()
            .ok_or("unsupported version saved")?;
        assert_eq!(failure.commit, CommitState::Unchanged);
        assert_eq!(failure.error, StoreError::Document(expected));
        assert!(!fixture.root().exists());
    }
    Ok(())
}

#[test]
fn invalid_mapping_values_duplicates_and_versions_are_typed_at_project_boundary() -> Result {
    let valid = serde_json::to_value(document()?)?;
    let cases = [
        (
            "/midi_mappings/version",
            json!(0),
            MappingDocumentError::UnsupportedVersion(0),
        ),
        (
            "/midi_mappings/version",
            json!(2),
            MappingDocumentError::UnsupportedVersion(2),
        ),
        (
            "/midi_mappings/mappings/0/channel",
            json!(16),
            MappingDocumentError::InvalidChannel(16),
        ),
        (
            "/midi_mappings/mappings/0/pitch",
            json!(128),
            MappingDocumentError::InvalidPitch(128),
        ),
        (
            "/midi_mappings/mappings/3/number",
            json!(128),
            MappingDocumentError::InvalidController(128),
        ),
        (
            "/midi_mappings/mappings/1/target/instrument",
            json!("-1"),
            MappingDocumentError::InvalidInstrument("-1".into()),
        ),
    ];
    for (pointer, value, error) in cases {
        let mut invalid = valid.clone();
        *invalid
            .pointer_mut(pointer)
            .ok_or("missing fixture field")? = value;
        let expected = DocumentError::MidiMappings(error);
        assert_eq!(
            Document::decode(&serde_json::to_vec(&invalid)?),
            Err(expected.clone())
        );
        let typed: Document = serde_json::from_value(invalid)?;
        assert_eq!(typed.encode(), Err(expected));
    }
    let mut duplicate = valid;
    duplicate["midi_mappings"]["mappings"][1] = duplicate["midi_mappings"]["mappings"][0].clone();
    assert!(matches!(
        Document::decode(&serde_json::to_vec(&duplicate)?),
        Err(DocumentError::MidiMappings(
            MappingDocumentError::Duplicate(_)
        ))
    ));
    Ok(())
}

#[test]
fn relocation_preserves_current_and_historical_mapping_banks() -> Result {
    let fixture = Fixture::new()?;
    let original = Document::default();
    let first = Store::new(fixture.root()).save(&original, &[])?;
    let changed = document()?;
    let second = Store::new(fixture.root()).save(&changed, &[])?;
    assert_eq!(second.previous, Some(first.generation));
    let relocated = fixture.0.join("relocated-project");
    fs::rename(fixture.root(), &relocated)?;
    assert!(!fixture.root().exists());
    let mut store = Store::new(relocated);
    let current = store.open(OpenChoice::Current)?;
    assert_eq!(current.document(), &changed);
    assert_channel_recall(&current.document().mappings()?)?;
    let historical = store.open(OpenChoice::Committed(first.generation))?;
    assert_eq!(historical.document(), &original);
    assert!(historical.document().mappings()?.entries().is_empty());
    let receipt = store.save(historical.document(), &[])?;
    assert_eq!(receipt.generation, first.generation);
    assert_eq!(receipt.previous, Some(second.generation));
    assert!(
        store
            .open(OpenChoice::Current)?
            .document()
            .mappings()?
            .entries()
            .is_empty()
    );
    Ok(())
}

#[test]
fn session_snapshots_save_open_and_recover_mappings_without_a_device() -> Result {
    let fixture = Fixture::new()?;
    let service = Service::new()?;
    let mut controller = Controller::new(TakeId(7), Arc::default());
    controller.set_mappings(mappings()?);
    let snapshot = snapshot(controller.mappings())?;
    controller.set_mappings(Document::default().mappings()?);
    assert!(controller.mappings().entries().is_empty());
    let expected = snapshot.document.clone();
    let first = match complete(
        &service,
        Request::Save {
            root: fixture.root(),
            snapshot: Box::new(snapshot),
            new_root: false,
        },
    )? {
        Completion::Saved(receipt) => receipt.generation,
        _ => return Err("save did not return a receipt".into()),
    };
    let prepared = match complete(
        &service,
        Request::Open {
            root: fixture.root(),
            choice: OpenChoice::Current,
        },
    )? {
        Completion::Opened(prepared) => prepared,
        _ => return Err("open did not prepare a project".into()),
    };
    assert_eq!(prepared.document, expected);
    assert_channel_recall(&prepared.mappings)?;

    // Re-saving the prepared project to a separate recovery root keeps the DTO.
    let recovered_root = fixture.0.join("recovered");
    let recovered = Snapshot::capture(
        &prepared.registry,
        prepared.document.pads.clone(),
        prepared.document.master,
        prepared.document.choke,
        prepared.media,
    )?
    .with_midi_mappings(&prepared.mappings);
    complete(
        &service,
        Request::Save {
            root: recovered_root.clone(),
            snapshot: Box::new(recovered),
            new_root: true,
        },
    )?;
    fs::write(fixture.root().join("project.json"), b"{")?;
    let before = fs::read(fixture.root().join("project.json"))?;
    match complete(
        &service,
        Request::Recovery {
            root: fixture.root(),
        },
    )? {
        Completion::Recovery(options) => {
            assert_eq!(
                options.current,
                Err(StoreError::Document(DocumentError::MalformedJson))
            );
            assert!(options.recoverable.contains(&first));
        }
        _ => return Err("recovery did not return options".into()),
    }
    match complete(
        &service,
        Request::Open {
            root: fixture.root(),
            choice: OpenChoice::Committed(first),
        },
    )? {
        Completion::Opened(prepared) => {
            assert_eq!(prepared.document, expected);
            assert_channel_recall(&prepared.mappings)?;
        }
        _ => return Err("recovery open did not prepare a project".into()),
    }
    assert_eq!(fs::read(fixture.root().join("project.json"))?, before);
    assert_channel_recall(
        &Store::new(recovered_root)
            .open(OpenChoice::Current)?
            .document()
            .mappings()?,
    )
}

#[test]
fn invalid_mappings_cannot_replace_a_saved_project_through_the_session() -> Result {
    let fixture = Fixture::new()?;
    let original = document()?;
    Store::new(fixture.root()).save(&original, &[])?;
    let before = fs::read(fixture.root().join("project.json"))?;
    let mut invalid = snapshot(&mappings()?)?;
    invalid.document.midi_mappings = Some(MappingsDocument {
        version: 2,
        mappings: Vec::new(),
    });
    let error = complete(
        &Service::new()?,
        Request::Save {
            root: fixture.root(),
            snapshot: Box::new(invalid),
            new_root: false,
        },
    )
    .err()
    .ok_or("invalid mappings saved")?;
    assert!(matches!(
        error.downcast_ref::<Error>(),
        Some(Error::Store(StoreError::Document(
            DocumentError::MidiMappings(MappingDocumentError::UnsupportedVersion(2))
        )))
    ));
    assert_eq!(fs::read(fixture.root().join("project.json"))?, before);
    assert_channel_recall(
        &Store::new(fixture.root())
            .open(OpenChoice::Current)?
            .document()
            .mappings()?,
    )
}
