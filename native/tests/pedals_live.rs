//! Tests against the real `brownnote` and the real GuitarPedal bench.
//!
//! They skip, printing why, when either is absent. `make` builds brownnote
//! and `make -C vendor/GuitarPedal/Validation bench` builds the bench.
//! `BROWNNOTE_REQUIRE_BENCH=1` turns a skip into a failure, so CI cannot pass
//! without them.
#![allow(clippy::unwrap_used)]

use brownnote_native::{
    kit,
    pedals::{
        brownnote::{self, CatalogError, DEFAULT_BIN},
        catalog::Catalog,
        render::{self, Chain, RenderJob, Stage},
    },
};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

fn binary() -> PathBuf {
    std::env::var_os("BROWNNOTE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| DEFAULT_BIN.into())
}

/// Skipping needs a reason, and `BROWNNOTE_REQUIRE_BENCH=1` makes it a failure
/// so the CI job cannot go green without touching the real bench.
fn skip(reason: &str) {
    assert!(
        std::env::var_os("BROWNNOTE_REQUIRE_BENCH").is_none(),
        "BROWNNOTE_REQUIRE_BENCH is set, so this cannot skip: {reason}"
    );
    println!("skipped: {reason}");
}

fn catalog() -> Option<Catalog> {
    let path = binary();
    if !path.exists() {
        skip(&format!("no brownnote at {}", path.display()));
        return None;
    }
    match brownnote::list(&path, Duration::from_secs(5)) {
        Ok(catalog) => Some(catalog),
        Err(CatalogError::NoBench(_)) => {
            skip("brownnote found no GuitarPedal bench");
            None
        }
        Err(CatalogError::NotFound(_)) => {
            skip(&format!("no brownnote at {}", path.display()));
            None
        }
        Err(error) => panic!("brownnote --list failed: {error}"),
    }
}

fn silence(path: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for _ in 0..4_410 {
        writer.write_sample(0_i16).unwrap();
    }
    writer.finalize().unwrap();
}

fn run(path: &Path, input: &Path, output: &Path, effect: &str) -> std::process::Output {
    Command::new(path)
        .arg("--effect")
        .arg(effect)
        .stdin(Stdio::from(File::open(input).unwrap()))
        .stdout(Stdio::from(File::create(output).unwrap()))
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[test]
fn the_real_list_parses_and_lists_the_named_effects() {
    let Some(catalog) = catalog() else { return };
    for name in ["Rat Sketch", "Tape Echo", "Flanger"] {
        let effect = catalog
            .effect(name)
            .unwrap_or_else(|| panic!("{name} missing from the real list"));
        assert!(effect.routable(), "{name} should be routable");
        assert!(!effect.pots.is_empty(), "{name} has no pots");
    }
    assert_eq!(catalog.routable().count() + 5, catalog.effects.len());
}

#[test]
fn routable_effects_render_and_system_blocks_are_refused_by_the_real_bench() {
    let Some(catalog) = catalog() else { return };
    let path = binary();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("pedals-live-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let input = dir.join("silence.wav");
    let output = dir.join("out.wav");
    silence(&input);

    for effect in &catalog.effects {
        let result = run(&path, &input, &output, &effect.name);
        let stderr = String::from_utf8_lossy(&result.stderr);
        if effect.routable() {
            assert!(result.status.success(), "{} failed: {stderr}", effect.name);
        } else {
            assert!(
                !result.status.success(),
                "{} should be refused by the bench",
                effect.name
            );
            assert!(
                stderr.contains("not routable"),
                "{} refusal lacks 'not routable': {stderr}",
                effect.name
            );
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}

fn rms(samples: &[f32]) -> f32 {
    let sum: f64 = samples
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum();
    (sum / samples.len() as f64).sqrt() as f32
}

#[test]
fn live_renders_change_the_sound_ring_out_and_then_hit_the_cache() {
    let Some(catalog) = catalog() else { return };
    let catalog = std::sync::Arc::new(catalog);
    let source = kit::load_wav(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("recordings/fart_dry.wav"),
    )
    .unwrap();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("pedals-live-render-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let job = |stages: Vec<Stage>| RenderJob {
        brownnote: binary(),
        cache: dir.join("cache"),
        catalog: catalog.clone(),
        source: source.clone(),
        chain: Chain { stages },
        timeout: Duration::from_secs(10),
    };
    let effect = |name: &str, pots: Vec<(&str, u8)>| Stage {
        effect: name.into(),
        pots: pots
            .into_iter()
            .map(|(label, value)| (label.into(), value))
            .collect(),
        mix: None,
    };

    for name in ["Rat Sketch", "Tape Echo", "Flanger"] {
        let rendered = render::render(&job(vec![effect(name, vec![])])).unwrap();
        assert!(!rendered.cached);
        let frames = source.data.len().min(rendered.sample.data.len());
        let difference: Vec<f32> = (0..frames)
            .map(|i| rendered.sample.data[i] - source.data[i])
            .collect();
        let changed = rms(&difference);
        assert!(
            changed > 0.01,
            "{name} barely changed the source: rms {changed}"
        );
    }

    let echo = render::render(&job(vec![effect("Tape Echo", vec![("Sustain", 90)])])).unwrap();
    let start = source.data.len();
    let end = start + (0.5 * f64::from(source.rate)) as usize;
    assert!(
        echo.sample.data.len() >= end,
        "echo output is only {} frames, need {end}",
        echo.sample.data.len()
    );
    let tail = rms(&echo.sample.data[start..end]);
    let floor = 10.0_f64.powf(-45.0 / 20.0) as f32;
    assert!(tail > floor, "echo tail is quiet: rms {tail}");

    let again = render::render(&job(vec![effect("Tape Echo", vec![("Sustain", 90)])])).unwrap();
    assert!(again.cached, "a second identical render missed the cache");
    fs::remove_dir_all(&dir).unwrap();
}
