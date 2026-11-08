#![allow(clippy::unwrap_used)]

use brownnote_native::{
    engine::{Command as AudioCommand, Meters, Mixer, SLOTS, Slots},
    kit::{self, Kit},
    paths::{DEFAULT_CACHE, Environment, PathErrorKind, Resolver, Resource},
    pedals::{
        brownnote::{self, CatalogError},
        render::{self, Chain, RenderJob, Stage},
    },
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const FIXTURE: &str = include_str!("fixtures/bench-list.txt");

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "brownnote-paths-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn resolver(&self, environment: Environment) -> Resolver {
        Resolver::new(
            self.0.join("layout/probe"),
            self.0.join("unrelated"),
            environment,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wav(path: &Path, amplitude: i16) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for _ in 0..44_100 {
        writer.write_sample(amplitude).unwrap();
    }
    writer.finalize().unwrap();
}

fn samples(path: &Path, amplitude: i16) {
    fs::create_dir_all(path).unwrap();
    for file in kit::FILES {
        wav(&path.join(file), amplitude);
    }
}

fn executable(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn layout(path: &Path) {
    samples(&path.join("resources/recordings"), 8192);
    fs::create_dir_all(path.join("tools")).unwrap();
    fs::copy(std::env::current_exe().unwrap(), path.join("probe")).unwrap();
    fs::write(path.join("tools/list.txt"), FIXTURE).unwrap();
    executable(&path.join("tools/bench"), "exit 0");
    executable(
        &path.join("tools/brownnote"),
        r#"
test "$1" = "--bench" || exit 41
test "$2" = "$(dirname "$0")/bench" || exit 42
test -x "$2" || exit 43
shift 2
if test "$1" = "--list"; then
  cat "$(dirname "$0")/list.txt"
else
  cat
fi"#,
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let to = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn probe(layout: &Path, cwd: &Path, home: &Path, mode: &str, extra: &[(&str, &Path)]) {
    let mut command = Command::new(layout.join("probe"));
    command
        .args(["--exact", "consumer_probe", "--nocapture"])
        .current_dir(cwd)
        .env("BROWNNOTE_PATH_PROBE", mode)
        .env("HOME", home);
    for variable in [
        "BROWNNOTE_SAMPLES",
        "BROWNNOTE_BIN",
        "BROWNNOTE_BENCH",
        "BROWNNOTE_CACHE",
        "BROWNNOTE_RECOVERY",
    ] {
        command.env_remove(variable);
    }
    for (name, path) in extra {
        command.env(name, path);
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "probe {mode} failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

// Re-exec the actual test artifact under each layout and cwd. Resolver::current,
// Kit::load, list, render, and the mixer are production code, not injected fakes.
#[test]
fn consumer_probe() {
    let Ok(mode) = std::env::var("BROWNNOTE_PATH_PROBE") else {
        return;
    };
    let kit = Kit::load(Path::new(kit::DEFAULT_SAMPLES));
    if mode == "invalid-samples" {
        assert!(!kit.banner.is_empty());
        assert!(kit.samples.iter().all(|s| s.data.is_empty()));
        assert!(kit.errors.iter().all(Option::is_some));
        return;
    }
    assert!(kit.banner.is_empty(), "{}", kit.banner);
    assert!(kit.samples.iter().all(|s| !s.data.is_empty()));
    let paths = Resolver::current().unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    for resource in [Resource::Cache, Resource::Recovery] {
        let path = paths.resolve(resource, None).unwrap();
        assert!(path.starts_with(&home));
        assert!(!path.components().any(|c| c.as_os_str() == "target"));
        fs::write(path.join("consumer-write"), b"writable").unwrap();
    }
    let result = brownnote::list(Path::new(brownnote::DEFAULT_BIN), Duration::from_secs(2));
    if mode == "missing-bin" {
        assert!(
            matches!(result, Err(CatalogError::NotFound(_))),
            "{result:?}"
        );
    } else if mode == "missing-bench" {
        assert!(
            matches!(result, Err(CatalogError::Path(ref e)) if e.resource == Resource::Bench),
            "{result:?}"
        );
    } else {
        let catalog = Arc::new(result.unwrap());
        assert_eq!(catalog.routable().count(), 22);
        let source = kit.samples[0].clone();
        // A different override must not silently use the bundled quarter-scale sound.
        if mode == "overrides" {
            assert_eq!(source.data[0], 0.5);
        } else {
            assert_eq!(source.data[0], 0.25);
        }
        let job = RenderJob {
            brownnote: brownnote::DEFAULT_BIN.into(),
            cache: DEFAULT_CACHE.into(),
            catalog,
            source,
            chain: Chain {
                stages: vec![Stage {
                    effect: "Reverb".into(),
                    pots: vec![],
                    mix: None,
                }],
            },
            timeout: Duration::from_secs(2),
        };
        let rendered = render::render(&job).unwrap();
        assert!(!rendered.sample.data.is_empty());
        assert!(render::render(&job).unwrap().cached);
    }
    // Missing optional tools must not disable base-pad signal.
    let (retired, _returns) = rtrb::RingBuffer::new(SLOTS);
    let mut mixer = Mixer::new(
        Slots::from_samples(kit.samples),
        44_100,
        Arc::new(Meters::default()),
        retired,
    );
    mixer.command(AudioCommand::Trigger {
        pad: 0,
        slot: 0,
        group: 0,
        velocity: 1.0,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    });
    let mut output = [0.0; 64];
    mixer.render(&mut output, 2);
    assert!(output.iter().any(|sample| *sample > 0.01));
}

#[test]
fn consumers_relocate_twice_with_previous_layout_removed_and_optional_tools_absent() {
    let fixture = Fixture::new();
    let original = fixture.0.join("original");
    let second = fixture.0.join("second copy");
    let third = fixture.0.join("third copy");
    let cwd = fixture.0.join("unrelated");
    let home = fixture.0.join("user");
    fs::create_dir_all(&cwd).unwrap();
    // A tempting, wrong cwd kit must never be selected.
    samples(&cwd.join("recordings"), 16384);
    layout(&original);
    probe(&original, &cwd, &home, "complete", &[]);
    copy_tree(&original, &second);
    fs::remove_dir_all(original).unwrap();
    probe(&second, &cwd, &home, "complete", &[]);
    copy_tree(&second, &third);
    fs::remove_dir_all(second).unwrap();
    probe(&third, &cwd, &home, "complete", &[]);
    fs::remove_file(third.join("tools/bench")).unwrap();
    probe(&third, &cwd, &home, "missing-bench", &[]);
    fs::remove_file(third.join("tools/brownnote")).unwrap();
    probe(&third, &cwd, &home, "missing-bin", &[]);
}

#[test]
fn environment_overrides_reach_real_consumers_and_invalid_samples_do_not_fall_back() {
    let fixture = Fixture::new();
    let root = fixture.0.join("layout");
    let cwd = fixture.0.join("unrelated");
    let home = fixture.0.join("user");
    fs::create_dir_all(&cwd).unwrap();
    layout(&root);
    let custom = fixture.0.join("custom");
    samples(&custom, 16384);
    probe(
        &root,
        &cwd,
        &home,
        "overrides",
        &[("BROWNNOTE_SAMPLES", &custom)],
    );
    let missing = fixture.0.join("missing");
    probe(
        &root,
        &cwd,
        &home,
        "invalid-samples",
        &[("BROWNNOTE_SAMPLES", &missing)],
    );
    fs::write(custom.join("fart_dry.wav"), b"invalid WAV").unwrap();
    let kit = Kit::load(&custom);
    assert!(kit.samples[0].data.is_empty());
    assert!(!kit.samples[1].data.is_empty());
    assert!(kit.errors[0].as_ref().unwrap().contains("fart_dry.wav"));
}

#[test]
fn explicit_paths_beat_environment_for_every_resource_including_relative_overrides() {
    let fixture = Fixture::new();
    let explicit = fixture.0.join("explicit");
    samples(&explicit.join("recordings"), 16384);
    fs::create_dir_all(fixture.0.join("unrelated")).unwrap();
    executable(&explicit.join("brownnote"), "exit 0");
    executable(&explicit.join("bench"), "exit 0");
    let bad = Some(fixture.0.join("missing-environment"));
    let resolver = fixture.resolver(Environment {
        samples: bad.clone(),
        brownnote: bad.clone(),
        bench: bad.clone(),
        cache: bad.clone(),
        recovery: bad,
        ..Environment::default()
    });
    for (resource, relative) in [
        (Resource::Samples, "../explicit/recordings"),
        (Resource::Brownnote, "../explicit/brownnote"),
        (Resource::Bench, "../explicit/bench"),
        (Resource::Cache, "../explicit/cache"),
        (Resource::Recovery, "../explicit/recovery"),
    ] {
        let path = resolver
            .resolve(resource, Some(Path::new(relative)))
            .unwrap();
        assert_eq!(
            fs::canonicalize(&path).unwrap(),
            fs::canonicalize(fixture.0.join("unrelated").join(relative)).unwrap()
        );
        let error = resolver.resolve(resource, Some(Path::new(""))).unwrap_err();
        assert_eq!(error.kind, PathErrorKind::Empty);
        assert_eq!(error.resource, resource);
    }
    let command = resolver
        .pedal_command(
            Some(&explicit.join("brownnote")),
            Some(&explicit.join("bench")),
        )
        .unwrap();
    assert_eq!(
        command.get_program(),
        explicit.join("brownnote").as_os_str()
    );
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec![
            std::ffi::OsStr::new("--bench"),
            explicit.join("bench").as_os_str()
        ]
    );
}

#[test]
#[ignore = "needs a GPU adapter"]
fn actual_app_keeps_default_samples_after_relocation_without_optional_tools() {
    let fixture = Fixture::new();
    let original = fixture.0.join("original app");
    let relocated = fixture.0.join("relocated app");
    let cwd = fixture.0.join("unrelated");
    let home = fixture.0.join("user");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&home).unwrap();
    samples(&cwd.join("recordings"), 0);
    samples(&original.join("resources/recordings"), 8192);
    fs::copy(
        env!("CARGO_BIN_EXE_brownnote-native"),
        original.join("brownnote-native"),
    )
    .unwrap();

    for root in [&original, &relocated] {
        if root == &relocated {
            copy_tree(&original, &relocated);
            fs::remove_dir_all(&original).unwrap();
        }
        let tree_path = root.join("playing.json");
        let mut command = Command::new(root.join("brownnote-native"));
        command
            .current_dir(&cwd)
            .env("HOME", &home)
            .args(["--scene", "playing", "--at", "0.08", "--tree"])
            .arg(&tree_path);
        for variable in [
            "BROWNNOTE_SAMPLES",
            "BROWNNOTE_BIN",
            "BROWNNOTE_BENCH",
            "BROWNNOTE_CACHE",
            "BROWNNOTE_RECOVERY",
        ] {
            command.env_remove(variable);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "actual relocated app failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let tree: serde_json::Value =
            serde_json::from_slice(&fs::read(tree_path).unwrap()).unwrap();
        let elements = tree["elements"].as_array().unwrap();
        assert!(
            !elements
                .iter()
                .any(|element| element["key"] == "asset-errors")
        );
        let voices = elements
            .iter()
            .find(|element| element["key"] == "pad-0-voices")
            .unwrap();
        assert_eq!(voices["text"], "▶ 1");
        let meter = elements
            .iter()
            .find(|element| element["key"] == "meter")
            .unwrap();
        assert_ne!(meter["text"], "−∞ dB");
        assert_eq!(tree["warnings"].as_array().unwrap().len(), 0);
    }
}

#[test]
fn typed_errors_cover_missing_wrong_kind_nonexecutable_malformed_and_unwritable() {
    let fixture = Fixture::new();
    let resolver = fixture.resolver(Environment::default());
    let missing = fixture.0.join("missing");
    for resource in [Resource::Samples, Resource::Brownnote, Resource::Bench] {
        let error = resolver.resolve(resource, Some(&missing)).unwrap_err();
        assert_eq!(error.kind, PathErrorKind::Missing);
        assert!(error.to_string().contains("missing"));
    }
    let file = fixture.0.join("file");
    fs::write(&file, b"not executable").unwrap();
    assert_eq!(
        resolver
            .resolve(Resource::Samples, Some(&file))
            .unwrap_err()
            .kind,
        PathErrorKind::NotDirectory
    );
    assert_eq!(
        resolver
            .resolve(Resource::Brownnote, Some(&fixture.0))
            .unwrap_err()
            .kind,
        PathErrorKind::NotFile
    );
    assert_eq!(
        resolver
            .resolve(Resource::Brownnote, Some(&file))
            .unwrap_err()
            .kind,
        PathErrorKind::NotExecutable
    );
    assert!(matches!(
        resolver
            .resolve(Resource::Samples, Some(Path::new("bad\0path")))
            .unwrap_err()
            .kind,
        PathErrorKind::Io { .. }
    ));
    for resource in [Resource::Cache, Resource::Recovery] {
        assert!(matches!(
            resolver
                .resolve(resource, Some(&file.join("child")))
                .unwrap_err()
                .kind,
            PathErrorKind::Io { .. }
        ));
        assert_eq!(
            resolver.resolve(resource, None).unwrap_err().kind,
            PathErrorKind::NoHome
        );
    }
}

#[test]
fn incomplete_bundle_does_not_fall_back_to_checkout_or_other_sounds() {
    let fixture = Fixture::new();
    let native = fixture.0.join("native");
    fs::create_dir_all(native.join("target/debug/resources")).unwrap();
    fs::write(native.join("Cargo.toml"), "[package]").unwrap();
    samples(&fixture.0.join("recordings"), 8192);
    let resolver = Resolver::new(
        native.join("target/debug/probe"),
        fixture.0.clone(),
        Environment::default(),
    );
    let error = resolver.resolve(Resource::Samples, None).unwrap_err();
    assert_eq!(error.kind, PathErrorKind::Missing);
    assert_eq!(
        error.path.unwrap(),
        native.join("target/debug/resources/recordings")
    );
    fs::remove_dir(native.join("target/debug/resources")).unwrap();
    std::os::unix::fs::symlink(
        fixture.0.join("missing-resource-tree"),
        native.join("target/debug/resources"),
    )
    .unwrap();
    let error = resolver.resolve(Resource::Samples, None).unwrap_err();
    assert_eq!(error.kind, PathErrorKind::Missing);
    assert_eq!(
        error.path.unwrap(),
        native.join("target/debug/resources/recordings")
    );
}
