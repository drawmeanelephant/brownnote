use super::{
    brownnote::{self, CatalogError},
    catalog::{Catalog, MixLaw},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
    time::{Duration, Instant},
};

const FIXTURE: &str = include_str!("../../tests/fixtures/bench-list.txt");

#[test]
fn fixture_parses_to_27_effects_of_which_22_are_routable() {
    let catalog = Catalog::parse(FIXTURE).unwrap();
    assert_eq!(catalog.effects.len(), 27);
    assert_eq!(catalog.routable().count(), 22);
    assert_eq!(catalog.effects.len() - catalog.routable().count(), 5);
}

#[test]
fn spot_checks_match_the_bench_list() {
    let catalog = Catalog::parse(FIXTURE).unwrap();
    let rat = catalog.effect("Rat Sketch").unwrap();
    assert_eq!(rat.pots.len(), 5);
    assert_eq!(rat.pots[3].label, "Mode");
    assert_eq!(rat.pots[3].max, 2);
    assert_eq!(rat.mix, MixLaw::Linear);

    let cabinet = catalog.effect("Cabinet").unwrap();
    assert_eq!(cabinet.pots[0].label, "Cabinet");
    assert_eq!(cabinet.pots[0].max, 4);
    assert_eq!(cabinet.pots[0].default, 3);

    let tone = catalog.effect("Tone 1").unwrap();
    let mid_q = tone.pots.iter().find(|p| p.label == "Mid Q").unwrap();
    assert_eq!(mid_q.unit, "");

    assert_eq!(catalog.effect("Vibrato").unwrap().mix, MixLaw::EqualPower);
    assert_eq!(catalog.effect("Parametric EQ").unwrap().pots.len(), 10);

    let usb = catalog.effect("USB Audio").unwrap();
    assert!(usb.always_runs);
    assert!(!usb.routable());
    assert!(usb.pots.iter().any(|p| p.label == "L/R Out"));
}

#[test]
fn effect_lookup_is_exact_and_routable_iterates_in_list_order() {
    let catalog = Catalog::parse(FIXTURE).unwrap();
    assert!(catalog.effect("rat sketch").is_none());
    let names: Vec<_> = catalog.routable().map(|e| e.name.as_str()).collect();
    assert_eq!(names.first(), Some(&"Hum Filter"));
    assert_eq!(names.last(), Some(&"Analog Out Tone"));
}

#[test]
fn a_pot_label_longer_than_the_padding_column_parses() {
    let text = "Effect [mix: none]\n    A Very Long Pot Label 0..120 default 60 dB\n";
    let catalog = Catalog::parse(text).unwrap();
    assert_eq!(catalog.effects[0].pots[0].label, "A Very Long Pot Label");
    assert_eq!(catalog.effects[0].pots[0].unit, "dB");
}

#[test]
fn parse_errors_name_the_line_and_reason() {
    let cases = [
        ("Garbage\n", 1, "neither"),
        ("    Gate  0..1  default 0 \n", 1, "pot before"),
        ("X [mix: cubic]\n    P 0..1 default 0 \n", 1, "unknown mix"),
        (
            "X [mix: none]\n    P 0..2 default 3 \n",
            2,
            "default is above max",
        ),
        (
            "X [mix: none]\n    P 0..300 default 0 \n",
            2,
            "does not fit in u8",
        ),
        (
            "X [mix: none]\n    P 0..120 default 300 \n",
            2,
            "does not fit in u8",
        ),
        ("X [mix: none]\n    P 0..120 default 0\nrest", 3, "neither"),
        ("", 1, "empty effect list"),
    ];
    for (text, line, reason) in cases {
        let error = Catalog::parse(text).unwrap_err();
        assert_eq!(error.line, line, "line for {text:?}");
        assert!(
            error.reason.contains(reason),
            "reason {:?} lacks {reason:?}",
            error.reason
        );
        assert!(error.to_string().contains(&format!("line {line}")));
    }
}

#[test]
fn a_malformed_pot_line_is_an_error_not_a_header() {
    let error = Catalog::parse("X [mix: none]\n  P 0..1 default 0\n").unwrap_err();
    assert_eq!(error.line, 2);
    assert!(error.reason.contains("four spaces"));
}

static SCRIPTS: AtomicUsize = AtomicUsize::new(0);

/// A fake `brownnote` in a temp directory under `native/target/`.
fn script(body: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let n = SCRIPTS.fetch_add(1, Relaxed);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("pedals-fake-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("list.txt"), FIXTURE).unwrap();
    let path = dir.join("brownnote");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    (path, dir)
}

#[test]
fn a_fake_brownnote_that_prints_the_list_gives_a_parsed_catalog() {
    let (path, dir) = script("cat \"$(dirname \"$0\")/list.txt\"");
    let catalog = brownnote::list(&path, Duration::from_secs(5)).unwrap();
    assert_eq!(catalog.effects.len(), 27);
    assert_eq!(catalog.source, FIXTURE);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn exit_two_with_no_bench_is_its_own_error_and_explains_the_fix() {
    let (path, dir) = script(
        "echo 'brownnote: no bench binary found (--bench PATH, BROWNNOTE_BENCH, or vendor/GuitarPedal/Validation/bench/bench)' >&2\nexit 2",
    );
    let error = brownnote::list(&path, Duration::from_secs(5)).unwrap_err();
    assert!(matches!(error, CatalogError::NoBench(_)), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("make -C vendor/GuitarPedal/Validation bench"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn another_non_zero_exit_keeps_the_code_and_stderr() {
    let (path, dir) = script("echo 'boom' >&2\nexit 1");
    let error = brownnote::list(&path, Duration::from_secs(5)).unwrap_err();
    match &error {
        CatalogError::Failed { code, stderr } => {
            assert_eq!(*code, 1);
            assert_eq!(stderr, "boom");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(error.to_string().contains("boom"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_slow_brownnote_is_killed_and_reported_as_a_timeout() {
    let (path, dir) = script("sleep 30");
    let start = Instant::now();
    let error = brownnote::list(&path, Duration::from_millis(200)).unwrap_err();
    assert!(matches!(error, CatalogError::Timeout), "{error:?}");
    assert!(start.elapsed() < Duration::from_millis(1200));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_missing_binary_is_reported_as_not_found() {
    let path = std::env::temp_dir().join("definitely-no-brownnote-here");
    let error = brownnote::list(&path, Duration::from_secs(1)).unwrap_err();
    assert!(matches!(error, CatalogError::NotFound(_)), "{error:?}");
    assert!(error.to_string().contains("run `make`"));
}
