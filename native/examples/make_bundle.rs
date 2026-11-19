//! Emit a small but real v4 export bundle for adapter testing.
//! Usage: cargo run --release --example make_bundle -- <source_root> <destination>
#![allow(clippy::unwrap_used, clippy::expect_used)]
use brownnote_native::export::{
    bundle::{BundleRequest, SystemWriter, write_bundle},
    fixture::{
        FixtureAsset, FixtureEvent, FixtureExport, FixtureMediaKind, FixturePad, FixturePart,
        FixturePartKind, FixtureTake, FixtureTarget,
    },
    manifest::{AssetId, EventId, PartId, TimeOrigin, Timing},
};
use std::{env, fs, io, path::PathBuf};

fn wav(rate: u32, frames: usize, amplitude: f32) -> Vec<u8> {
    let mut cursor = io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for i in 0..frames {
            // Decaying sine so clips have real audio, not DC.
            let t = i as f32 / rate as f32;
            let env = (-t * 6.0).exp();
            writer
                .write_sample(amplitude * env * (t * 440.0 * 2.0 * std::f32::consts::PI).sin())
                .unwrap();
        }
        writer.finalize().unwrap();
    }
    cursor.into_inner()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let source_root = PathBuf::from(args.next().expect("usage: make_bundle <source_root> <dest>"));
    let destination = PathBuf::from(args.next().expect("usage: make_bundle <source_root> <dest>"));

    const ORIGIN: u64 = 1_000_000_000;
    let source = wav(48_000, 24_000, 0.5);
    let retained = wav(48_000, 36_000, 0.7);
    let source_id = AssetId::of(&source);
    let retained_id = AssetId::of(&retained);
    for (id, bytes) in [(source_id, &source), (retained_id, &retained)] {
        let path = source_root.join(id.relative_path());
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, bytes)?;
    }

    let hit = EventId {
        take: 1,
        sequence: 0,
    };
    let hit2 = EventId {
        take: 1,
        sequence: 1,
    };
    let note = EventId {
        take: 1,
        sequence: 2,
    };
    let pad_hit = |id, start, dur, vel| FixtureEvent {
        id,
        target: FixtureTarget::Pad(3),
        start_nanos: start,
        duration_nanos: dur,
        velocity: Some(vel),
        pitch: None,
        channel: None,
        asset: None,
        variant_of: None,
        edit: None,
    };
    let export = FixtureExport {
        takes: vec![FixtureTake {
            id: 1,
            recording_tempo_milli_bpm: 116_000,
            events: vec![
                pad_hit(hit, ORIGIN, 500_000_000, 100),
                pad_hit(hit2, ORIGIN + 600_000_000, 300_000_000, 88),
                FixtureEvent {
                    id: note,
                    target: FixtureTarget::Instrument(7),
                    start_nanos: ORIGIN + 1_000_000_000,
                    duration_nanos: 250_000_000,
                    velocity: Some(90),
                    pitch: Some(60),
                    channel: Some(0),
                    asset: None,
                    variant_of: None,
                    edit: None,
                },
            ],
        }],
        pads: vec![FixturePad {
            id: 3,
            source: Some(source_id),
            retained: Some(retained_id),
            baked_chain: vec!["Tape Echo".into()],
            choke: true,
        }],
        assets: vec![
            FixtureAsset {
                id: source_id,
                bytes: source.len() as u64,
                kind: FixtureMediaKind::Source,
            },
            FixtureAsset {
                id: retained_id,
                bytes: retained.len() as u64,
                kind: FixtureMediaKind::RetainedRender,
            },
        ],
        parts: vec![
            FixturePart {
                id: PartId(0),
                kind: FixturePartKind::PadClips { pad: 3 },
                event_ids: vec![hit, hit2],
            },
            FixturePart {
                id: PartId(1),
                kind: FixturePartKind::InstrumentNotes { instrument: 7 },
                event_ids: vec![note],
            },
        ],
        losses: Vec::new(),
    };
    let request = BundleRequest {
        source_root,
        destination: destination.clone(),
        origin: TimeOrigin { nanos: ORIGIN },
        timing: Timing {
            tempo_milli_bpm: 116_000,
            sample_rate_hz: 48_000,
        },
    };
    let receipt = write_bundle(&mut SystemWriter, &request, &export)?;
    println!(
        "wrote {} events, {} assets -> {}",
        receipt.event_count,
        receipt.asset_count,
        receipt.destination.display()
    );
    Ok(())
}
