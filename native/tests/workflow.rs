//! FIXTURE-INTEGRITY / EXPECTATION CHECKS ONLY. No native or DAW workflow runs.
//! The runtime driver awaits the owners' public APIs; see the manual runbook.
#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};
use std::{collections::HashSet, fs, path::PathBuf, process::Command};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/workflow/score-v1.json")).unwrap()
}

#[test]
fn fixture_integrity_score_preserves_exact_originals_counts_and_overlaps() {
    let f = fixture();
    assert_eq!(f["fixture_version"], 1);
    assert_eq!(f["scope"], "fixture-integrity-only");
    assert_eq!(f["reference_unit"], "nanoseconds");
    assert_eq!(
        (
            f["tempo_bpm"].as_u64(),
            f["beats_per_bar"].as_u64(),
            f["loop_bars"].as_u64()
        ),
        (Some(120), Some(4), Some(2))
    );
    assert_eq!(f["loop_ns"], 4_000_000_000_u64);
    assert_eq!(
        f["score_columns"],
        json!([
            "cue",
            "take",
            "destination",
            "pitch",
            "channel",
            "onset_ns",
            "span_ns",
            "velocity"
        ])
    );
    let rows = f["score"].as_array().unwrap();
    // Independent reference, not expected values obtained from a native run.
    assert_eq!(
        f["score"],
        json!([
            [
                "pad-a",
                "base",
                "custom-pad",
                null,
                1,
                60_000_000,
                1_000_000_000,
                48
            ],
            [
                "pad-b",
                "base",
                "custom-pad",
                null,
                1,
                185_000_000,
                1_000_000_000,
                112
            ],
            [
                "note-a",
                "base",
                "instrument",
                60,
                1,
                510_000_000,
                1_000_000_000,
                80
            ],
            [
                "note-b",
                "base",
                "instrument",
                60,
                1,
                760_000_000,
                1_000_000_000,
                96
            ],
            [
                "pad-c",
                "overdub",
                "custom-pad",
                null,
                1,
                2_060_000_000_u64,
                1_000_000_000,
                72
            ],
            [
                "note-c",
                "overdub",
                "instrument",
                67,
                1,
                2_510_000_000_u64,
                1_000_000_000,
                88
            ]
        ])
    );
    assert_eq!(
        rows.iter()
            .map(|r| r[0].as_str().unwrap())
            .collect::<HashSet<_>>()
            .len(),
        6
    );
    for (key, column, value, expected) in [
        ("base_count", 1, "base", 4),
        ("overdub_count", 1, "overdub", 2),
        ("pad_count", 2, "custom-pad", 3),
        ("note_count", 2, "instrument", 3),
    ] {
        assert_eq!(rows.iter().filter(|r| r[column] == value).count(), expected);
        assert_eq!(f["expectations"][key], expected);
    }
    assert_eq!(f["expectations"]["combined_count"], rows.len());
    assert_eq!(
        rows[0][5].as_u64().unwrap() + rows[0][6].as_u64().unwrap() - rows[1][5].as_u64().unwrap(),
        875_000_000
    );
    assert_eq!(
        rows[2][5].as_u64().unwrap() + rows[2][6].as_u64().unwrap() - rows[3][5].as_u64().unwrap(),
        750_000_000
    );
    assert_eq!(
        f["note_offs_columns"],
        json!(["offset_ns", "channel", "pitch", "oldest_cue_ended"])
    );
    assert_eq!(
        f["note_offs"],
        json!([
            [1_510_000_000, 1, 60, "note-a"],
            [1_760_000_000, 1, 60, "note-b"],
            [3_510_000_000_u64, 1, 67, "note-c"]
        ])
    );
    for off in f["note_offs"].as_array().unwrap() {
        let row = rows.iter().find(|r| r[0] == off[3]).unwrap();
        assert_eq!(
            row[5].as_u64().unwrap() + row[6].as_u64().unwrap(),
            off[0].as_u64().unwrap()
        );
        assert_eq!(row[3], off[2]);
        assert_eq!(row[4], off[1]);
    }
}

#[test]
fn fixture_expectations_edits_are_reversible_without_a_runtime_edit_implementation() {
    let f = fixture();
    let e = &f["expectations"];
    let rows = f["score"].as_array().unwrap();
    assert_eq!(e["quantize_grid_ns"], 125_000_000);
    assert_eq!(
        e["quantized_onsets_ns"],
        json!([
            0,
            125_000_000,
            500_000_000,
            750_000_000,
            2_000_000_000,
            2_500_000_000_u64
        ])
    );
    for (row, onset) in rows
        .iter()
        .zip(e["quantized_onsets_ns"].as_array().unwrap())
    {
        let original = row[5].as_u64().unwrap();
        let quantized = onset.as_u64().unwrap();
        assert_eq!(quantized % 125_000_000, 0);
        assert!(original.abs_diff(quantized) < 62_500_000);
    }
    assert_eq!(e["move_note_c_after_quantize_ns"], 2_625_000_000_u64);
    assert_eq!(
        e["move_note_c_after_quantize_ns"].as_u64().unwrap()
            - e["quantized_onsets_ns"][5].as_u64().unwrap(),
        125_000_000
    );
    assert_eq!(e["delete_pad_b_count"], rows.len() - 1);
    assert_eq!(
        e["restored_onsets_ns"],
        json!([
            60_000_000,
            185_000_000,
            510_000_000,
            760_000_000,
            2_060_000_000_u64,
            2_510_000_000_u64
        ])
    );
    assert_eq!(
        rows.iter().map(|r| r[5].clone()).collect::<Vec<_>>(),
        *e["restored_onsets_ns"].as_array().unwrap()
    );
    let visible: Vec<_> = rows
        .iter()
        .filter(|r| r[5].as_u64().unwrap() < 2_000_000_000)
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(visible, *e["one_bar_visible_cues"].as_array().unwrap());
}

#[test]
fn fixture_integrity_requires_custom_pedals_but_leaves_d18_unresolved() {
    let f = fixture();
    assert_eq!(
        f["takes"],
        json!([
            {"label": "base", "mode": "initial"}, {"label": "overdub", "mode": "overdub"}
        ])
    );
    assert_eq!(f["custom_pad"]["label"], "synthetic-custom-pad");
    assert_eq!(f["custom_pad"]["choke"], false);
    assert_eq!(
        f["custom_pad"]["required_pedal_order"],
        json!(["Rat Sketch", "Tape Echo"])
    );
    assert_eq!(
        f["instrument_gate"],
        json!({
            "issue": 108, "decision": "D18", "status": "unresolved", "backend": null, "asset": null
        })
    );
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn fixture_integrity_synthetic_wav_is_exact_repeatable_and_never_overwrites() {
    let f = fixture();
    assert_eq!(
        f["custom_pad"]["source"],
        json!({
            "file": "workflow-pad-v1.wav", "sample_rate": 48000, "channels": 1,
            "bits_per_sample": 16, "frames": 48000, "period_frames": 240,
            "peak_pcm": 4096, "fade_frames": 240
        })
    );
    let path = std::env::temp_dir().join(format!("brownnote-workflow-{}", std::process::id()));
    fs::create_dir(&path).unwrap();
    let scratch = Scratch(path);
    let generator =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workflow/generate_audio.py");
    let generate = |name: &str| {
        Command::new("python3")
            .arg(&generator)
            .arg(scratch.0.join(name))
            .output()
            .unwrap_or_else(|error| panic!("fixture-integrity requires Python 3: {error}"))
    };
    for name in ["first.wav", "second.wav"] {
        let output = generate(name);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let path = scratch.0.join("first.wav");
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes, fs::read(scratch.0.join("second.wav")).unwrap());
    assert!(
        !generate("first.wav").status.success(),
        "must refuse existing files"
    );
    assert_eq!(
        bytes,
        fs::read(&path).unwrap(),
        "overwrite attempt changed media"
    );
    let mut wav = hound::WavReader::open(path).unwrap();
    let spec = wav.spec();
    assert_eq!(
        (spec.sample_rate, spec.channels, spec.bits_per_sample),
        (48_000, 1, 16)
    );
    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    assert_eq!(wav.duration(), 48_000);
    for (i, sample) in wav.samples::<i16>().enumerate() {
        let envelope = i.min(47_999 - i).min(240);
        let magnitude = (4096 * envelope / 240) as i16;
        let expected = if i % 240 < 120 { magnitude } else { -magnitude };
        assert_eq!(sample.unwrap(), expected, "PCM frame {i}");
    }
}
