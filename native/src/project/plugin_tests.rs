#![allow(clippy::unwrap_used)]
use super::*;
use crate::{
    pedals::{
        offline::{Backend, RenderPolicy},
        render::{Chain, Stage as PedalStage},
    },
    project::{Asset, AssetRef, Document, DocumentError, MediaKind, PadId, SampleAssignment},
};

fn plugin(backend: Backend, id: &str) -> Plugin {
    Plugin {
        backend,
        id: id.into(),
        artifact: Hash256::of(id.as_bytes()),
        abi: "clap 1.2.2".into(),
        version: "1.0".into(),
    }
}

fn source() -> Source {
    Source {
        channels: 1,
        rate: 48_000,
        samples: (0..480).map(|i| (i as f32 * 0.01).sin()).collect(),
    }
}

fn chain() -> PluginChain {
    let request = Request {
        source: source(),
        chain: vec![
            Stage {
                plugin: plugin(Backend::Clap, "com.example.verb"),
                location: "/Library/Audio/Plug-Ins/CLAP/Verb.clap".into(),
                params: vec![
                    Param {
                        id: 3,
                        value: 0.1 + 0.2,
                    },
                    Param {
                        id: 9,
                        value: 1.0 / 3.0,
                    },
                ],
                state: Some(Arc::from(&b"\x00\x01binary\xffstate"[..])),
            },
            Stage {
                plugin: plugin(Backend::Ladspa, "1234:delay"),
                location: "/usr/lib/ladspa/delay.so".into(),
                params: vec![Param { id: 2, value: -0.0 }],
                state: None,
            },
        ],
        policy: RenderPolicy::standard(Layout::Stereo, 48_000),
    };
    let identity = request.identity();
    PluginChain::record(&request, &["Verb".into(), "Delay".into()], identity)
}

fn document(plugins: Option<PluginChain>, retained: bool) -> Document {
    let source = AssetRef::of(b"source");
    let render = AssetRef::of(b"render");
    let mut document = Document {
        assets: vec![
            Asset {
                reference: source.clone(),
                kind: MediaKind::Source,
            },
            Asset {
                reference: render.clone(),
                kind: MediaKind::RetainedRender,
            },
        ],
        ..Default::default()
    };
    document.pads[2].sample = Some(SampleAssignment {
        source: source.id,
        retained: retained.then_some(render.id),
    });
    document.pads[2].plugins = plugins;
    document
}

#[test]
fn a_plugin_chain_reopens_with_the_exact_identity() {
    let chain = chain();
    assert_eq!(chain.identity(source()), chain.render);
    let bytes = document(Some(chain.clone()), true).encode().unwrap();
    let reopened = Document::decode(&bytes).unwrap();
    let back = reopened.pads[2].plugins.clone().unwrap();
    assert_eq!(back, chain);
    assert_eq!(back.identity(source()), chain.render);
    assert_eq!(
        back.stages[0].params[0].value.0.to_bits(),
        (0.1f64 + 0.2).to_bits()
    );
    assert!(back.stages[1].params[0].value.0.is_sign_negative());
    // Locations are hints, not identity: a relocated plugin renders the same.
    let moved = back.request(source(), |_, _| "/elsewhere".into());
    assert_eq!(moved.identity(), chain.render);
}

#[test]
fn exact_numbers_survive_json_for_every_bit_pattern_tried() {
    let mut bits = 0x9e37_79b9_7f4a_7c15_u64;
    for _ in 0..20_000 {
        bits ^= bits << 13;
        bits ^= bits >> 7;
        bits ^= bits << 17;
        let value = f64::from_bits(bits);
        if !value.is_finite() {
            continue;
        }
        let json = serde_json::to_string(&Exact(value)).unwrap();
        let back: Exact = serde_json::from_str(&json).unwrap();
        assert_eq!(back.0.to_bits(), value.to_bits(), "{json}");
    }
    assert!(serde_json::from_str::<Exact>("\"NaN\"").is_err());
    assert!(serde_json::from_str::<Exact>("\"inf\"").is_err());
    assert!(serde_json::from_str::<Exact>("0.5").is_err());
}

#[test]
fn state_bytes_round_trip_and_reject_non_canonical_base64() {
    for length in 0..70 {
        let bytes: Vec<u8> = (0..length).map(|i| (i * 37 + 11) as u8).collect();
        let text = String::from(StateBytes(bytes.clone().into()));
        assert_eq!(&*StateBytes::try_from(text).unwrap().0, &bytes[..]);
    }
    assert_eq!(String::from(StateBytes(Arc::from(&b"Man"[..]))), "TWFu");
    for bad in ["TWF", "TW==TWFu", "TWF=", "T@Fu", "===="] {
        assert!(StateBytes::try_from(bad.to_string()).is_err(), "{bad}");
    }
}

#[test]
fn documents_without_plugins_do_not_mention_them() {
    let bytes = document(None, false).encode().unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("plugins"));
}

#[test]
fn a_plugin_chain_needs_retained_audio_and_excludes_guitarpedal_stages() {
    let pad = PadId::try_from(2).unwrap();
    assert_eq!(
        document(Some(chain()), false).validate(),
        Err(DocumentError::MissingRetained(pad))
    );
    let mut both = document(Some(chain()), true);
    both.pads[2].chain = Chain {
        stages: vec![PedalStage {
            effect: "Tape Echo".into(),
            pots: Vec::new(),
            mix: None,
        }],
    };
    assert_eq!(both.validate(), Err(DocumentError::InvalidPluginChain(pad)));
    let mut unordered = chain();
    unordered.stages[0].params.reverse();
    assert_eq!(
        document(Some(unordered), true).validate(),
        Err(DocumentError::InvalidPluginChain(pad))
    );
    let mut empty = chain();
    empty.stages.clear();
    assert_eq!(
        document(Some(empty), true).validate(),
        Err(DocumentError::InvalidPluginChain(pad))
    );
    let mut no_pad = document(Some(chain()), true);
    no_pad.pads[2].sample = None;
    assert_eq!(no_pad.validate(), Err(DocumentError::MissingRetained(pad)));
}

#[test]
fn unknown_plugin_fields_are_rejected_not_dropped() {
    let bytes = document(Some(chain()), true).encode().unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let edited = text.replacen("\"name\": \"Verb\"", "\"name\": \"Verb\", \"editor\": 1", 1);
    assert_ne!(edited, text);
    assert_eq!(
        Document::decode(edited.as_bytes()),
        Err(DocumentError::InvalidDocument)
    );
}

#[test]
fn the_description_names_the_plugin_and_the_loss() {
    let chain = chain();
    let line = chain.stages[0].describe();
    assert!(line.starts_with("CLAP plugin \"Verb\" (com.example.verb 1.0, clap 1.2.2"));
    assert!(line.contains("2 parameters set"));
    assert!(line.contains("not editable in other hosts"));
    assert!(
        chain.stages[1]
            .describe()
            .starts_with("LADSPA plugin \"Delay\"")
    );
}
