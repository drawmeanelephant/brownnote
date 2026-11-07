//! Tests against the real `brownnote` and the real GuitarPedal bench.
//!
//! They skip, printing why, when either is absent. `make` builds brownnote
//! and `make -C vendor/GuitarPedal/Validation bench` builds the bench.
#![allow(clippy::unwrap_used)]

use brownnote_native::pedals::{
    brownnote::{self, CatalogError, DEFAULT_BIN},
    catalog::Catalog,
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

fn catalog() -> Option<Catalog> {
    let path = binary();
    if !path.exists() {
        println!("skipped: no brownnote at {}", path.display());
        return None;
    }
    match brownnote::list(&path, Duration::from_secs(5)) {
        Ok(catalog) => Some(catalog),
        Err(CatalogError::NoBench(_)) => {
            println!("skipped: brownnote found no GuitarPedal bench");
            None
        }
        Err(CatalogError::NotFound(_)) => {
            println!("skipped: no brownnote at {}", path.display());
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
