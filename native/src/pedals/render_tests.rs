use super::*;
use crate::{engine::Sample, pedals::catalog::Catalog};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
    time::{Duration, Instant, SystemTime},
};

const FIXTURE: &str = include_str!("../../tests/fixtures/bench-list.txt");
static DIRS: AtomicUsize = AtomicUsize::new(0);

fn catalog() -> Arc<Catalog> {
    Arc::new(Catalog::parse(FIXTURE).unwrap())
}

fn source() -> Sample {
    Sample {
        data: Arc::from([0.0, 0.25, -0.5, 0.75]),
        rate: 44_100,
    }
}

fn stage(effect: &str, pots: &[(&str, u8)], mix: Option<u8>) -> Stage {
    Stage {
        effect: effect.into(),
        pots: pots.iter().map(|(l, v)| ((*l).into(), *v)).collect(),
        mix,
    }
}

fn chain(stages: &[Stage]) -> Chain {
    Chain {
        stages: stages.to_vec(),
    }
}

fn dir() -> PathBuf {
    let n = DIRS.fetch_add(1, Relaxed);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("pedals-render-{}-{n}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

/// A fake brownnote in `dir`, with a `runs` marker it appends to.
fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("brownnote");
    fs::write(
        &path,
        format!("#!/bin/sh\necho run >> \"$(dirname \"$0\")/runs\"\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn runs(dir: &Path) -> usize {
    fs::read_to_string(dir.join("runs"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn job(bin: &Path, cache: &Path, chain: Chain, source: Sample, catalog: Arc<Catalog>) -> RenderJob {
    RenderJob {
        brownnote: bin.into(),
        cache: cache.into(),
        catalog,
        source,
        chain,
        timeout: Duration::from_secs(5),
    }
}

/// The sidecar the cache keeps its clipping count in.
fn clip(cache_file: &Path) -> PathBuf {
    cache_file.with_extension("clip")
}

fn wav_files(cache: &Path) -> Vec<PathBuf> {
    fs::read_dir(cache)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("wav"))
        .collect()
}

#[test]
fn argv_for_three_stages_is_exact() {
    let chain = chain(&[
        stage("Rat Sketch", &[("Distortion", 90)], None),
        stage("Tape Echo", &[], Some(40)),
        stage("Reverb", &[("Room", 100), ("Damp", 20)], None),
    ]);
    assert_eq!(
        argv(&chain),
        [
            "--effect",
            "Rat Sketch",
            "--pot",
            "Rat Sketch:Distortion=90",
            "--effect",
            "Tape Echo",
            "--mix",
            "Tape Echo=40",
            "--effect",
            "Reverb",
            "--pot",
            "Reverb:Room=100",
            "--pot",
            "Reverb:Damp=20",
        ]
    );
}

#[test]
fn every_invalid_chain_is_refused_before_a_process_starts() {
    let dir = dir();
    let bin = script(&dir, "exit 0");
    let cache = dir.join("cache");
    let catalog = catalog();
    let cases = [
        chain(&[]),
        chain(&[
            stage("Rat Sketch", &[], None),
            stage("Tape Echo", &[], None),
            stage("Reverb", &[], None),
            stage("Flanger", &[], None),
            stage("Phaser", &[], None),
        ]),
        chain(&[stage("No Such Pedal", &[], None)]),
        chain(&[stage("Signal Chain", &[], None)]),
        chain(&[stage("Reverb", &[], None), stage("Reverb", &[], None)]),
        chain(&[stage("Reverb", &[("Nope", 1)], None)]),
        chain(&[stage("Rat Sketch", &[("Mode", 3)], None)]),
        chain(&[stage("Reverb", &[], Some(121))]),
        chain(&[stage("Hum Filter", &[], Some(10))]),
    ];
    for chain in cases {
        let error = render(&job(&bin, &cache, chain, source(), catalog.clone())).unwrap_err();
        assert!(matches!(error, RenderError::Invalid(_)), "{error}");
    }
    assert!(
        !dir.join("runs").exists(),
        "a process ran for an invalid chain"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_passthrough_render_trims_the_tail_and_then_hits_the_cache() {
    let dir = dir();
    let bin = script(&dir, "cat");
    let cache = dir.join("cache");
    let catalog = catalog();
    let source = source();
    let make = || {
        job(
            &bin,
            &cache,
            chain(&[stage("Reverb", &[], None)]),
            source.clone(),
            catalog.clone(),
        )
    };

    let first = render(&make()).unwrap();
    assert!(!first.cached);
    assert_eq!(first.clipped, None);
    assert_eq!(first.sample.rate, 44_100);
    assert_eq!(first.sample.data.len(), source.data.len());
    for (got, expected) in first.sample.data.iter().zip(source.data.iter()) {
        assert!((got - expected).abs() <= 1.0 / 32768.0);
    }
    assert_eq!(runs(&dir), 1);
    assert_eq!(wav_files(&cache).len(), 1);

    let second = render(&make()).unwrap();
    assert!(second.cached);
    assert_eq!(runs(&dir), 1, "the cached call ran brownnote again");
    assert_eq!(second.sample.data.len(), source.data.len());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failures_keep_their_kind_and_leave_no_partial_files() {
    let cases = [
        (
            "echo 'brownnote: no bench binary found (--bench PATH)' >&2\nexit 2",
            "nobench",
        ),
        (
            "echo 'cannot route Signal Chain: already routed, or it is not routable' >&2\nexit 1",
            "notroutable",
        ),
        ("echo boom >&2\nexit 1", "bench"),
        ("echo 'bad option' >&2\nexit 2", "usage"),
    ];
    for (body, name) in cases {
        let dir = dir();
        let bin = script(&dir, body);
        let cache = dir.join("cache");
        let error = render(&job(
            &bin,
            &cache,
            chain(&[stage("Reverb", &[], None)]),
            source(),
            catalog(),
        ))
        .unwrap_err();
        match (name, &error) {
            ("nobench", RenderError::NoBench(_))
            | ("notroutable", RenderError::NotRoutable(_))
            | ("usage", RenderError::Usage(_)) => {}
            ("bench", RenderError::Bench { code: 1, stderr }) if stderr == "boom" => {}
            _ => panic!("{name}: unexpected {error:?}"),
        }
        assert!(
            wav_files(&cache).is_empty() && fs::read_dir(&cache).unwrap().next().is_none(),
            "{name}: a failed render left a file behind"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn a_brownnote_that_does_not_answer_is_asked_to_stop_and_then_killed() {
    let dir = dir();
    // Stops on SIGTERM like the real brownnote, which forwards it to the bench
    // before leaving.
    let bin = script(
        &dir,
        "d=$(dirname \"$0\")\nsleep 30 & child=$!\n\
         trap 'kill $child 2>/dev/null; echo term > \"$d/term\"; exit 0' TERM\n\
         wait $child",
    );
    let cache = dir.join("cache");
    let mut job = job(
        &bin,
        &cache,
        chain(&[stage("Reverb", &[], None)]),
        source(),
        catalog(),
    );
    job.timeout = Duration::from_millis(200);
    let start = Instant::now();
    let error = render(&job).unwrap_err();
    assert!(matches!(error, RenderError::Timeout), "{error:?}");
    assert!(start.elapsed() < Duration::from_millis(1200));
    assert!(
        dir.join("term").exists(),
        "the timeout skipped SIGTERM, so the bench would never hear about it"
    );
    assert!(fs::read_dir(&cache).unwrap().next().is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_brownnote_that_ignores_sigterm_is_still_killed_within_the_grace() {
    let dir = dir();
    // An ignored signal stays ignored across exec, so this stands in for a
    // bench that will not act on the signal brownnote forwards.
    let bin = script(&dir, "trap '' TERM\nexec sleep 30");
    let cache = dir.join("cache");
    let mut job = job(
        &bin,
        &cache,
        chain(&[stage("Reverb", &[], None)]),
        source(),
        catalog(),
    );
    job.timeout = Duration::from_millis(200);
    let start = Instant::now();
    let error = render(&job).unwrap_err();
    assert!(matches!(error, RenderError::Timeout), "{error:?}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the grace ran long: {:?}",
        start.elapsed()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_missing_binary_and_a_missing_output_are_reported() {
    let dir = dir();
    let cache = dir.join("cache");
    let missing = dir.join("no-such-brownnote");
    let error = render(&job(
        &missing,
        &cache,
        chain(&[stage("Reverb", &[], None)]),
        source(),
        catalog(),
    ))
    .unwrap_err();
    assert!(matches!(error, RenderError::NotFound(_)), "{error:?}");

    let bin = script(&dir, "exit 0");
    let error = render(&job(
        &bin,
        &cache,
        chain(&[stage("Reverb", &[], None)]),
        source(),
        catalog(),
    ))
    .unwrap_err();
    assert!(matches!(error, RenderError::Output(_)), "{error:?}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn eviction_leaves_recordings_it_does_not_own_alone() {
    let dir = dir();
    let bin = script(&dir, "cat");
    let cache = dir.join("cache");
    fs::create_dir_all(&cache).unwrap();
    // `--pedal-cache` takes any directory; 257 songs in it must all survive.
    for i in 0..257 {
        fs::write(cache.join(format!("song-{i:03}.wav")), b"x").unwrap();
    }
    fs::write(cache.join("keep.txt"), b"x").unwrap();

    render(&job(
        &bin,
        &cache,
        chain(&[stage("Reverb", &[], None)]),
        source(),
        catalog(),
    ))
    .unwrap();

    for i in 0..257 {
        assert!(
            cache.join(format!("song-{i:03}.wav")).exists(),
            "song-{i:03}.wav was evicted"
        );
    }
    assert!(cache.join("keep.txt").exists());
    assert_eq!(wav_files(&cache).len(), 258);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn eviction_keeps_the_newest_256_cache_entries() {
    let dir = dir();
    let bin = script(&dir, "cat");
    let cache = dir.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let owned = |i: usize| cache.join(format!("{i:016x}.wav"));
    for i in 0..300 {
        let path = owned(i);
        fs::write(&path, b"x").unwrap();
        fs::write(clip(&path), b"3\n").unwrap();
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(i as u64);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    render(&job(
        &bin,
        &cache,
        chain(&[stage("Reverb", &[], None)]),
        source(),
        catalog(),
    ))
    .unwrap();

    let files = wav_files(&cache);
    assert_eq!(files.len(), 256);
    assert!(!owned(0).exists());
    assert!(!owned(44).exists());
    assert!(owned(45).exists());
    assert!(!clip(&owned(0)).exists(), "a clip sidecar outlived its wav");
    assert!(clip(&owned(45)).exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_clipping_warning_survives_a_cache_hit() {
    let dir = dir();
    let bin = script(&dir, "echo 'clipped 12 samples' >&2\ncat");
    let cache = dir.join("cache");
    let catalog = catalog();
    let make = || {
        job(
            &bin,
            &cache,
            chain(&[stage("Reverb", &[], None)]),
            source(),
            catalog.clone(),
        )
    };

    let first = render(&make()).unwrap();
    assert_eq!(first.clipped, Some(12));
    let second = render(&make()).unwrap();
    assert!(second.cached);
    assert_eq!(
        second.clipped,
        Some(12),
        "a cached render forgot that it clipped"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_source_long_enough_to_exceed_the_kit_limit_still_renders_and_caches() {
    let dir = dir();
    let bin = script(&dir, "cat");
    let cache = dir.join("cache");
    let catalog = catalog();
    // 29 s at 8 kHz plus the 2 s tail is 248 000 frames, past the 240 000 the
    // kit accepts, and exactly what a long kit sample would produce.
    let frames = 8_000 * 29;
    let source = Sample {
        data: Arc::from(vec![0.1_f32; frames]),
        rate: 8_000,
    };
    let make = || {
        job(
            &bin,
            &cache,
            chain(&[stage("Reverb", &[], None)]),
            source.clone(),
            catalog.clone(),
        )
    };
    let first = render(&make()).unwrap();
    assert_eq!(first.sample.data.len(), frames);
    let second = render(&make()).unwrap();
    assert!(second.cached);
    assert_eq!(second.sample.data.len(), frames);
    assert_eq!(runs(&dir), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_cache_key_is_stable_and_covers_every_input() {
    let catalog = catalog();
    let base = || {
        job(
            Path::new("/tmp/brownnote"),
            Path::new("/tmp/cache"),
            chain(&[stage("Reverb", &[("Room", 90)], Some(30))]),
            source(),
            catalog.clone(),
        )
    };
    let key = |job: &RenderJob| cache_key(job, &argv(&job.chain));

    let first = key(&base());
    assert_eq!(first, key(&base()), "the key is not deterministic");
    assert_eq!(first, 0x885e_6313_8f4f_61f5, "the fixed key changed");

    let mut pot = base();
    pot.chain.stages[0].pots[0].1 = 91;
    assert_ne!(first, key(&pot));

    let mut mix = base();
    mix.chain.stages[0].mix = None;
    assert_ne!(first, key(&mix));

    let mut order = base();
    order.chain.stages.push(stage("Flanger", &[], None));
    assert_ne!(first, key(&order));

    let mut data = base();
    data.source = Sample {
        data: Arc::from([0.0, 0.25, -0.5, 0.76]),
        rate: 44_100,
    };
    assert_ne!(first, key(&data));

    let mut rate = base();
    rate.source = Sample {
        data: Arc::from([0.0, 0.25, -0.5, 0.75]),
        rate: 48_000,
    };
    assert_ne!(first, key(&rate));

    let mut text = base();
    text.catalog = Arc::new(Catalog {
        effects: catalog.effects.clone(),
        source: format!("{} ", catalog.source),
    });
    assert_ne!(first, key(&text));
}
