#![allow(clippy::unwrap_used, clippy::expect_used)]
// Bootstrap until the integrator exports project and adds serde/sha2.
use brownnote_native::pedals;
#[path = "../src/project/mod.rs"]
mod project;

use pedals::render::{Chain, Stage};
use project::{
    Asset, AssetId, AssetRef, Document, DocumentError, MediaKind, PadId, SampleAssignment,
    media::{self, MediaError},
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "brownnote-project-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("media")).unwrap();
        Self(path)
    }
    fn media(&self, bytes: &[u8]) -> AssetRef {
        let reference = AssetRef::of(bytes);
        fs::write(self.0.join(reference.id.relative_path()), bytes).unwrap();
        reference
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn document() -> Document {
    let source = AssetRef::of(b"source fixture");
    let retained = AssetRef::of(b"retained fixture");
    let mut document = Document {
        assets: vec![
            Asset {
                reference: source.clone(),
                kind: MediaKind::Source,
            },
            Asset {
                reference: retained.clone(),
                kind: MediaKind::RetainedRender,
            },
        ],
        ..Document::default()
    };
    for (index, pad) in document.pads.iter_mut().enumerate() {
        pad.sample = Some(SampleAssignment {
            source: source.id,
            retained: Some(retained.id),
        });
        pad.gain = if index % 2 == 0 { 0.0 } else { 1.5 };
        pad.pan = if index % 2 == 0 { -1.0 } else { 1.0 };
        pad.semitones = if index % 2 == 0 { -12.0 } else { 12.0 };
        pad.chain = Chain {
            stages: vec![
                Stage {
                    effect: "Tape Echo".into(),
                    pots: vec![("Time".into(), 0), ("Feedback".into(), 255)],
                    mix: None,
                },
                Stage {
                    effect: "Fuzz".into(),
                    pots: vec![("Drive".into(), 42)],
                    mix: Some(120),
                },
            ],
        };
    }
    document
}

#[test]
fn sha256_known_answer_and_canonical_portable_reference() {
    let id = AssetId::of(b"abc");
    assert_eq!(
        id.to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(id.to_string().parse::<AssetId>().unwrap(), id);
    assert_eq!(id.relative_path(), PathBuf::from(format!("media/{id}.wav")));
    for bad in [
        "../a.wav",
        "/tmp/a.wav",
        "ABC",
        "",
        &"a".repeat(65),
        &"A".repeat(64),
    ] {
        assert_eq!(bad.parse::<AssetId>(), Err(MediaError::MalformedId));
    }
}

#[test]
fn empty_kit_and_retained_output_reused_as_source_round_trip() {
    let empty = Document::default();
    assert_eq!(Document::decode(&empty.encode().unwrap()).unwrap(), empty);
    let mut doc = document();
    let rendered = doc.assets[1].reference.id;
    doc.pads[0].sample = Some(SampleAssignment {
        source: rendered,
        retained: Some(rendered),
    });
    assert_eq!(Document::decode(&doc.encode().unwrap()).unwrap(), doc);
    doc.assets[1].kind = MediaKind::Source;
    assert_eq!(doc.validate(), Err(DocumentError::WrongMediaKind(rendered)));
}

#[test]
fn duplicate_document_fields_are_not_silently_overwritten() {
    let bytes = document().encode().unwrap();
    let json = String::from_utf8(bytes).unwrap();
    let duplicate = json.replacen('{', "{\"version\":1,", 1);
    assert_eq!(
        Document::decode(duplicate.as_bytes()),
        Err(DocumentError::InvalidDocument)
    );
}

#[test]
fn all_16_pads_settings_shared_assets_and_ordered_typed_chains_round_trip() {
    let document = document();
    let encoded = document.encode().unwrap();
    assert_eq!(Document::decode(&encoded).unwrap(), document);
    assert_eq!(document.assets.len(), 2);
    for index in 0..16u8 {
        let id = PadId::try_from(index).unwrap();
        assert_eq!(id.index(), usize::from(index));
        assert_eq!(
            document.pads[id.index()].chain.stages[0].effect,
            "Tape Echo"
        );
        assert_eq!(document.pads[id.index()].chain.stages[1].mix, Some(120));
    }
    assert!(matches!(
        PadId::try_from(16),
        Err(DocumentError::InvalidPad(16))
    ));
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    for excluded in [
        "device",
        "midi_device",
        "cache",
        "window",
        "voices",
        "slot",
        "focus",
        "take",
    ] {
        assert!(value.get(excluded).is_none());
    }
    assert_eq!(project::DOCUMENT_NAME, "project.json");
    let _cache = media::DisposableCache {
        path: PathBuf::from("disposable"),
    };
}

#[test]
fn document_failures_are_typed_and_leave_input_untouched() {
    for (bytes, expected) in [
        (b"{".as_slice(), DocumentError::MalformedJson),
        (b"{}".as_slice(), DocumentError::InvalidDocument),
        (
            b"{\"version\":0}".as_slice(),
            DocumentError::OlderVersion(0),
        ),
        (
            b"{\"version\":2,\"future_schema\":true}".as_slice(),
            DocumentError::NewerVersion(2),
        ),
    ] {
        let before = bytes.to_vec();
        assert_eq!(Document::decode(bytes), Err(expected));
        assert_eq!(bytes, before);
    }
    let value = serde_json::to_value(document()).unwrap();
    for mutate in [
        |v: &mut serde_json::Value| v["assets"][0]["reference"]["id"] = "../escape".into(),
        |v: &mut serde_json::Value| v["assets"][0]["reference"]["path"] = "/elsewhere.wav".into(),
        |v: &mut serde_json::Value| v["pads"].as_array_mut().unwrap().pop().map(|_| ()).unwrap(),
        |v: &mut serde_json::Value| v["audio_device"] = "machine preference".into(),
    ] {
        let mut invalid = value.clone();
        mutate(&mut invalid);
        let bytes = serde_json::to_vec(&invalid).unwrap();
        assert_eq!(
            Document::decode(&bytes),
            Err(DocumentError::InvalidDocument)
        );
    }
}

#[test]
fn invalid_settings_references_and_chains_never_substitute_audio() {
    let mut doc = document();
    for value in [f32::NAN, f32::INFINITY, -0.01, 1.51] {
        doc.pads[0].gain = value;
        assert!(matches!(
            doc.encode(),
            Err(DocumentError::Setting { name: "gain", .. })
        ));
    }
    doc = document();
    doc.master = 1.01;
    assert!(matches!(
        doc.validate(),
        Err(DocumentError::Setting { name: "master", .. })
    ));
    doc = document();
    let missing = doc.assets.remove(0).reference.id;
    assert_eq!(doc.validate(), Err(DocumentError::MissingAsset(missing)));
    doc = document();
    doc.assets.push(doc.assets[0].clone());
    assert!(matches!(
        doc.validate(),
        Err(DocumentError::DuplicateAsset(_))
    ));
    doc = document();
    doc.pads[0].sample.as_mut().unwrap().retained = None;
    assert!(matches!(
        doc.validate(),
        Err(DocumentError::MissingRetained(_))
    ));
    doc = document();
    doc.pads[0]
        .chain
        .stages
        .push(doc.pads[0].chain.stages[0].clone());
    assert!(matches!(
        doc.validate(),
        Err(DocumentError::InvalidChain(_))
    ));
    doc = document();
    doc.pads[0].chain.stages[0].mix = Some(121);
    assert!(matches!(
        doc.validate(),
        Err(DocumentError::InvalidChain(_))
    ));
}

#[test]
fn relocate_after_removing_original_and_play_retained_without_bench_or_cache() {
    let original = Fixture::new();
    let relocated = Fixture::new();
    let source = original.media(b"source fixture");
    let retained = original.media(b"retained fixture");
    let doc = document();
    fs::write(
        original.0.join(project::DOCUMENT_NAME),
        doc.encode().unwrap(),
    )
    .unwrap();
    for reference in [&source, &retained] {
        fs::copy(
            original.0.join(reference.id.relative_path()),
            relocated.0.join(reference.id.relative_path()),
        )
        .unwrap();
    }
    fs::copy(
        original.0.join(project::DOCUMENT_NAME),
        relocated.0.join(project::DOCUMENT_NAME),
    )
    .unwrap();
    fs::remove_dir_all(&original.0).unwrap();
    assert!(!original.0.exists());
    let loaded =
        Document::decode(&fs::read(relocated.0.join(project::DOCUMENT_NAME)).unwrap()).unwrap();
    assert!(media::verify_all(&relocated.0, &loaded.assets).is_empty());
    let audible = loaded.pads[0].sample.as_ref().unwrap().audible();
    assert_eq!(audible, retained.id);
    let path = media::resolve_verified(&relocated.0, &retained).unwrap();
    assert_eq!(fs::read(path).unwrap(), b"retained fixture");
    // There is no process/bench invocation in this boundary at all.
}

#[test]
fn missing_truncated_and_same_size_corrupt_media_are_read_only_diagnostics() {
    let fixture = Fixture::new();
    let reference = fixture.media(b"abcd");
    let path = fixture.0.join(reference.id.relative_path());
    fs::write(&path, b"wxyz").unwrap();
    assert!(matches!(
        media::resolve_verified(&fixture.0, &reference),
        Err(MediaError::HashMismatch { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), b"wxyz");
    fs::write(&path, b"a").unwrap();
    assert!(matches!(
        media::resolve_verified(&fixture.0, &reference),
        Err(MediaError::SizeMismatch { actual: 1, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), b"a");
    fs::remove_file(path).unwrap();
    assert_eq!(
        media::resolve_verified(&fixture.0, &reference),
        Err(MediaError::Missing(reference.id))
    );
}

#[cfg(unix)]
#[test]
fn symlink_cannot_escape_project_owned_media() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let reference = outside.media(b"outside");
    let link = fixture.0.join(reference.id.relative_path());
    std::os::unix::fs::symlink(outside.0.join(reference.id.relative_path()), &link).unwrap();
    assert_eq!(
        media::resolve_verified(&fixture.0, &reference),
        Err(MediaError::EscapesProject(reference.id))
    );
    assert_eq!(fs::read(link).unwrap(), b"outside");
}
