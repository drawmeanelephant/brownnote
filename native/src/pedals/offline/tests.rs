#![allow(clippy::unwrap_used)]
use super::{
    Backend, Hash256, Param, Plugin,
    protocol::{self, FromHelper, HelperError, Open, PluginInfo, ToHelper},
};
use std::io::Cursor;

fn plugin() -> Plugin {
    Plugin {
        backend: Backend::Ladspa,
        id: "1234:delay".into(),
        artifact: Hash256::of(b"lib"),
        abi: "ladspa 1.1".into(),
        version: "unversioned".into(),
    }
}

#[test]
fn messages_round_trip() {
    let to = [
        ToHelper::Open(
            Open {
                protocol: protocol::VERSION,
                backend: Backend::Clap,
                plugin_id: "a.b".into(),
                location: "/x.clap".into(),
                rate: 48_000,
                channels: 2,
                max_block_frames: 512,
                params: vec![Param { id: 3, value: 0.5 }],
                state: true,
            },
            Some(vec![0, 1, 2]),
        ),
        ToHelper::Open(
            Open {
                protocol: protocol::VERSION,
                backend: Backend::Ladspa,
                plugin_id: "1:x".into(),
                location: "/x.so".into(),
                rate: 44_100,
                channels: 1,
                max_block_frames: 16,
                params: Vec::new(),
                state: false,
            },
            None,
        ),
        ToHelper::Process {
            frames: 2,
            samples: vec![0.25, -1.0, f32::MIN_POSITIVE, 3.0],
        },
        ToHelper::End,
    ];
    let mut bytes = Vec::new();
    for message in &to {
        protocol::write_to_helper(&mut bytes, message).unwrap();
    }
    let mut input = Cursor::new(bytes);
    for message in &to {
        assert_eq!(
            protocol::read_to_helper(&mut input).unwrap().as_ref(),
            Some(message)
        );
    }
    assert_eq!(protocol::read_to_helper(&mut input).unwrap(), None);

    let from = [
        FromHelper::Ready(PluginInfo {
            protocol: protocol::VERSION,
            plugin: plugin(),
            name: "Delay".into(),
            latency_frames: 0,
            tail_frames: None,
            params: Vec::new(),
        }),
        FromHelper::Block(vec![1.0, f32::NAN]),
        FromHelper::Done(None),
        FromHelper::Done(Some(Vec::new())),
        FromHelper::Error(HelperError {
            code: "bundle-missing".into(),
            message: "gone".into(),
        }),
    ];
    let mut bytes = Vec::new();
    for message in &from {
        protocol::write_from_helper(&mut bytes, message).unwrap();
    }
    let mut input = Cursor::new(bytes);
    for message in &from {
        let read = protocol::read_from_helper(&mut input).unwrap().unwrap();
        match (message, &read) {
            (FromHelper::Block(a), FromHelper::Block(b)) => {
                assert_eq!(a[0], b[0]);
                assert!(b[1].is_nan());
            }
            _ => assert_eq!(&read, message),
        }
    }
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    let cases: [&[u8]; 5] = [
        b"Q\x00\x00\x00\x00",
        b"B\x03\x00\x00\x00abc",
        b"D\x00\x00\x00\x00",
        b"R\x02\x00\x00\x00{}",
        b"B\xff\xff\xff\xff",
    ];
    for case in cases {
        assert!(
            protocol::read_from_helper(&mut Cursor::new(case)).is_err(),
            "{case:?}"
        );
    }
    let truncated = b"B\x08\x00\x00\x00abcd";
    let error = protocol::read_from_helper(&mut Cursor::new(truncated)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn hashes_parse_and_print() {
    let hash = Hash256::of(b"x");
    assert_eq!(hash.to_string().parse::<Hash256>().unwrap(), hash);
    assert!("ABC".parse::<Hash256>().is_err());
    assert!("G".repeat(64).parse::<Hash256>().is_err());
    let json = serde_json::to_string(&plugin()).unwrap();
    assert_eq!(serde_json::from_str::<Plugin>(&json).unwrap(), plugin());
}
