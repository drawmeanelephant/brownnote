//! The CLAP offline adapter through the public renderer: the real
//! `brownnote-plugin-host clap` helper, the fault-fixture CLAP plugin, and the
//! pinned Airwindows Consolidated effect.
//!
//! `make native-test` builds `native/plugin-host` and sets
//! `BROWNNOTE_CLAP_HELPER`, `BROWNNOTE_CLAP_FAULT_FIXTURE` and
//! `BROWNNOTE_REQUIRE_CLAP=1`. The pinned effect runs when
//! `BROWNNOTE_CLAP_EFFECT` names its bundle; `make native-clap-effect-test`
//! sets `BROWNNOTE_REQUIRE_CLAP_EFFECT=1` so it cannot skip there.
#![allow(clippy::unwrap_used)]

use brownnote_native::pedals::offline::{
    self, Backend, Cancel, ErrorKind, Hash256, HelperCommand, Helpers, Layout, Limits, Param,
    Phase, RenderPolicy, Rendered, Request, Source, Stage,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const RATE: u32 = 48_000;

fn skip(require: &str, reason: &str) {
    assert!(
        std::env::var_os(require).is_none(),
        "{require} is set, so this cannot skip: {reason}"
    );
    println!("skipped: {reason}");
}

fn env_path(name: &str, require: &str) -> Option<PathBuf> {
    match std::env::var_os(name).map(PathBuf::from) {
        Some(path) if path.exists() => Some(path),
        Some(path) => {
            skip(
                require,
                &format!("{name}={} does not exist", path.display()),
            );
            None
        }
        None => {
            skip(require, &format!("{name} is not set"));
            None
        }
    }
}

fn helpers() -> Option<Helpers> {
    let helper = env_path("BROWNNOTE_CLAP_HELPER", "BROWNNOTE_REQUIRE_CLAP")?;
    Some(Helpers::new().with(
        Backend::Clap,
        HelperCommand {
            program: helper,
            args: vec!["clap".into()],
        },
    ))
}

/// The helper and the fault fixture, or `None` after a permitted skip.
fn fixture() -> Option<(Helpers, PathBuf)> {
    let helpers = helpers()?;
    let fixture = env_path("BROWNNOTE_CLAP_FAULT_FIXTURE", "BROWNNOTE_REQUIRE_CLAP")?;
    Some((helpers, fixture))
}

fn limits() -> Limits {
    Limits {
        open: Duration::from_secs(10),
        block: Duration::from_secs(5),
        close: Duration::from_secs(5),
        total: Duration::from_secs(120),
    }
}

fn stage(helpers: &Helpers, id: &str, location: &Path) -> Stage {
    let info = offline::probe(
        helpers,
        Backend::Clap,
        id,
        location,
        RATE,
        Layout::Stereo,
        &limits(),
        &Cancel::new(),
    )
    .unwrap();
    Stage {
        plugin: info.plugin,
        location: location.into(),
        params: Vec::new(),
        state: None,
    }
}

/// `frames` of a 440 Hz sine at half scale, computed like the #96 probe's
/// scenario input so the reference render below is comparable.
fn sine(frames: usize) -> Source {
    let samples: Vec<f32> = (0..frames)
        .map(|frame| {
            let t = frame as f64 / f64::from(RATE);
            0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32
        })
        .collect();
    Source {
        channels: 1,
        rate: RATE,
        samples: samples.into(),
    }
}

fn request(source: Source, chain: Vec<Stage>, block_frames: u32) -> Request {
    Request {
        source,
        chain,
        policy: RenderPolicy {
            block_frames,
            ..RenderPolicy::standard(Layout::Stereo, RATE)
        },
    }
}

fn render(helpers: &Helpers, request: &Request) -> Result<Rendered, offline::RenderError> {
    offline::render(request, helpers, &limits(), &Cancel::new())
}

fn sha256_file(path: &Path) -> Hash256 {
    Hash256::of(&fs::read(path).unwrap())
}

#[test]
fn fixture_passthrough_renders_exactly_with_state() {
    let Some((helpers, fixture)) = fixture() else {
        return;
    };
    let info = offline::probe(
        &helpers,
        Backend::Clap,
        "org.brownnote.fault.passthrough",
        &fixture,
        RATE,
        Layout::Stereo,
        &limits(),
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(info.plugin.artifact, sha256_file(&fixture));
    assert_eq!(info.plugin.version, "0.1.0");
    assert!(
        info.plugin.abi.starts_with("clap 1."),
        "{}",
        info.plugin.abi
    );
    assert_eq!(info.latency_frames, 0);
    assert_eq!(info.tail_frames, None);

    let source = sine(4800);
    let mut passthrough = stage(&helpers, "org.brownnote.fault.passthrough", &fixture);
    let rendered = render(
        &helpers,
        &request(source.clone(), vec![passthrough.clone()], 256),
    )
    .unwrap();
    assert_eq!(rendered.frames(), 4800);
    let (frames, _) = rendered.samples.as_chunks::<2>();
    for (frame, input) in frames.iter().zip(source.samples.iter()) {
        assert_eq!(*frame, [*input, *input]);
    }
    // The fixture's state counts saves: a fresh instance saves generation 1.
    assert_eq!(
        rendered.stages[0].state.as_deref(),
        Some(&1u64.to_le_bytes()[..])
    );
    passthrough.state = Some(Arc::from(&41u64.to_le_bytes()[..]));
    let restored = render(&helpers, &request(source, vec![passthrough], 256)).unwrap();
    assert_eq!(
        restored.stages[0].state.as_deref(),
        Some(&42u64.to_le_bytes()[..])
    );
}

#[test]
fn fixture_layout_and_load_failures_are_typed() {
    let Some((helpers, fixture)) = fixture() else {
        return;
    };
    let base = stage(&helpers, "org.brownnote.fault.passthrough", &fixture);
    let kind = |stage: Stage, layout: Layout| {
        let mut request = request(sine(480), vec![stage], 256);
        request.policy.layout = layout;
        render(&helpers, &request).unwrap_err().kind
    };
    let unavailable = |stage: Stage| match kind(stage, Layout::Stereo) {
        ErrorKind::Unavailable { code, .. } => code,
        other => panic!("{other:?}"),
    };

    assert!(matches!(
        kind(base.clone(), Layout::Mono),
        ErrorKind::Unsupported(_)
    ));

    for (id, code) in [
        ("org.brownnote.fault.init-fails", "init-failed"),
        ("org.brownnote.fault.activate-fails", "activate-failed"),
        ("org.brownnote.fault.nothing", "unknown-plugin-id"),
    ] {
        let mut s = base.clone();
        s.plugin.id = id.into();
        assert_eq!(unavailable(s), code, "{id}");
    }

    let dir = std::env::temp_dir().join(format!("brownnote-clap-offline-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut missing = base.clone();
    missing.location = dir.join("Gone.clap");
    assert_eq!(unavailable(missing), "bundle-missing");
    let text = dir.join("not-a-plugin.clap");
    fs::write(&text, "not a plugin").unwrap();
    let mut garbage = base.clone();
    garbage.location = text;
    assert_eq!(unavailable(garbage), "dlopen-failed");
    let mut param = base.clone();
    param.params = vec![Param { id: 1, value: 0.5 }];
    assert_eq!(unavailable(param), "param-rejected");
    let mut state = base.clone();
    state.state = Some(Arc::from(&[1u8, 2][..]));
    assert_eq!(unavailable(state), "state-rejected");

    #[cfg(target_os = "macos")]
    {
        // A macOS bundle resolves to its executable, which is what gets hashed.
        let bundle = dir.join("Fixture.clap");
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        let executable = bundle.join("Contents/MacOS/Fixture");
        fs::copy(&fixture, &executable).unwrap();
        let bundled = stage(&helpers, "org.brownnote.fault.passthrough", &bundle);
        assert_eq!(bundled.plugin.artifact, sha256_file(&executable));
        render(&helpers, &request(sine(480), vec![bundled], 256)).unwrap();
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fixture_faults_fail_without_taking_the_app_down() {
    let Some((helpers, fixture)) = fixture() else {
        return;
    };
    let base = stage(&helpers, "org.brownnote.fault.passthrough", &fixture);
    let fault = |id: &str| {
        let mut s = base.clone();
        s.plugin.id = id.into();
        request(sine(48_00), vec![s], 256)
    };
    let quick = Limits {
        block: Duration::from_millis(300),
        ..limits()
    };

    let error = render(&helpers, &fault("org.brownnote.fault.crash")).unwrap_err();
    assert_eq!(error.stage, Some(0));
    match error.kind {
        ErrorKind::Exited { status, .. } => assert!(status.contains("signal: 11"), "{status}"),
        other => panic!("{other:?}"),
    }

    // The fixture faults on its ninth block.
    let error = render(&helpers, &fault("org.brownnote.fault.nonfinite")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NonFinite { frame: 8 * 256 });

    let started = Instant::now();
    let error = offline::render(
        &fault("org.brownnote.fault.hang"),
        &helpers,
        &quick,
        &Cancel::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout(Phase::Block(8)));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );

    let cancel = Cancel::new();
    let canceller = {
        let cancel = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            cancel.cancel();
            Instant::now()
        })
    };
    let error = offline::render(
        &fault("org.brownnote.fault.hang"),
        &helpers,
        &limits(),
        &cancel,
    )
    .unwrap_err();
    let returned = Instant::now();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    let latency = returned.duration_since(canceller.join().unwrap());
    assert!(
        latency < Duration::from_millis(250),
        "cancel took {latency:?}"
    );

    // The same helper still serves a healthy plugin afterwards.
    render(&helpers, &request(sine(480), vec![base], 256)).unwrap();
}

// Airwindows Consolidated 1.2026.313, pinned by #96 (native/docs/spikes/clap/pin.json).
const EFFECT_ID: &str = "com.airwindows.consolidated";
const EFFECT_VERSION: &str = "1.2026.313";
const EFFECT_EXECUTABLE_SHA256: &str =
    "92152f95a7053ecd65c4e5f356e719c6b82a7dfe4083bc12848f5c755077ad4d";
/// SHA-256 of the interleaved little-endian f32 samples of the #96 probe's
/// in-process render `evidence/effect/burst-512.wav`: this input, block 512,
/// offline mode, default state, 192000 frames.
const PROBE_BURST_512_SHA256: &str =
    "1fff4401e4922bc9c17f0dc725520ba7a6abcc319e0b6eca9481ad478c5c4f62";
const DECLARED_TAIL: u32 = 2 * RATE;

fn effect() -> Option<(Helpers, PathBuf)> {
    let helpers = helpers()?;
    let bundle = env_path("BROWNNOTE_CLAP_EFFECT", "BROWNNOTE_REQUIRE_CLAP_EFFECT")?;
    Some((helpers, bundle))
}

fn first_frames_sha256(rendered: &Rendered, frames: usize) -> String {
    let bytes: Vec<u8> = rendered.samples[..frames * 2]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    Hash256::of(&bytes).to_string()
}

#[test]
fn pinned_effect_matches_the_probe_reference_and_measures_its_tail() {
    let Some((helpers, bundle)) = effect() else {
        return;
    };
    let info = offline::probe(
        &helpers,
        Backend::Clap,
        EFFECT_ID,
        &bundle,
        RATE,
        Layout::Stereo,
        &limits(),
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(info.plugin.version, EFFECT_VERSION);
    assert_eq!(info.plugin.artifact.to_string(), EFFECT_EXECUTABLE_SHA256);
    assert_eq!(info.latency_frames, 0);
    assert_eq!(info.tail_frames, Some(DECLARED_TAIL));
    assert!(!info.params.is_empty());
    for param in &info.params {
        assert!(
            param.min.is_finite()
                && param.max.is_finite()
                && (param.min..=param.max).contains(&param.default),
            "{param:?}"
        );
    }

    let effect = Stage {
        plugin: info.plugin.clone(),
        location: bundle.clone(),
        params: Vec::new(),
        state: None,
    };
    let burst = request(sine(12_000), vec![effect.clone()], 512);
    let started = Instant::now();
    let rendered = render(&helpers, &burst).unwrap();
    let elapsed = started.elapsed();
    let report = &rendered.stages[0];
    println!(
        "pinned effect: {} frames ({} tail, declared {DECLARED_TAIL}, capped {}) in {elapsed:?}",
        rendered.frames(),
        report.tail_frames,
        report.tail_capped
    );
    assert!(rendered.frames() >= 192_000);
    assert_eq!(
        first_frames_sha256(&rendered, 192_000),
        PROBE_BURST_512_SHA256,
        "the helper must reproduce the #96 in-process render sample for sample"
    );
    assert!(!report.tail_capped, "the -90 dBFS tail fits in 20 s");
    assert!(
        report.tail_frames > 4 * DECLARED_TAIL as usize,
        "measured tail {} must far exceed the declared {DECLARED_TAIL}",
        report.tail_frames
    );
    let state = report.state.clone().expect("the effect saves state");

    // Repeatable in fresh helper processes; block size does not change the sound.
    assert_eq!(render(&helpers, &burst).unwrap().samples, rendered.samples);
    let by_256 = render(&helpers, &request(sine(12_000), vec![effect.clone()], 256)).unwrap();
    assert_ne!(by_256.identity, rendered.identity);
    assert_eq!(by_256.samples, rendered.samples);

    // A generic control changes the sound and the identity.
    let dry_wet = info.params.iter().find(|p| p.name == "Dry/Wet").unwrap();
    let mut half = effect.clone();
    half.params = vec![Param {
        id: dry_wet.id,
        value: 0.5,
    }];
    let half = render(&helpers, &request(sine(12_000), vec![half], 512)).unwrap();
    assert_ne!(half.identity, rendered.identity);
    assert_ne!(half.samples[..192_000 * 2], rendered.samples[..192_000 * 2]);

    // #96: reloading even the default state changes this reverb's output, so
    // the rendered audio, not the state blob, is what a project must keep.
    let mut reloaded = effect;
    reloaded.state = Some(state);
    let reloaded = render(&helpers, &request(sine(12_000), vec![reloaded], 512)).unwrap();
    assert_ne!(
        reloaded.samples, rendered.samples,
        "characterize: state reload no longer changes the output; revisit retained-audio guidance"
    );
}
