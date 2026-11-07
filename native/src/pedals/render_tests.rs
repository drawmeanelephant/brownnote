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
fn a_brownnote_that_does_not_answer_is_killed_and_reported_as_a_timeout() {
    let dir = dir();
    let bin = script(&dir, "sleep 30");
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
    assert!(fs::read_dir(&cache).unwrap().next().is_none());
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
fn eviction_keeps_the_newest_256_wav_files_and_leaves_other_files_alone() {
    let dir = dir();
    let bin = script(&dir, "cat");
    let cache = dir.join("cache");
    fs::create_dir_all(&cache).unwrap();
    for i in 0..300 {
        let path = cache.join(format!("old-{i:03}.wav"));
        fs::write(&path, b"x").unwrap();
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(i);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(time)
            .unwrap();
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

    let files = wav_files(&cache);
    assert_eq!(files.len(), 256);
    assert!(!cache.join("old-000.wav").exists());
    assert!(!cache.join("old-044.wav").exists());
    assert!(cache.join("old-045.wav").exists());
    assert!(cache.join("keep.txt").exists());
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
