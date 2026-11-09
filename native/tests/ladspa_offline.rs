//! The LADSPA offline adapter through the public renderer: the real
//! `brownnote-plugin-host ladspa` helper, the LADSPA fault fixture, and the
//! pinned CAPS Plate reverb (#97's target).
//!
//! - `BROWNNOTE_LADSPA_HELPER` and `BROWNNOTE_LADSPA_FIXTURE` name the
//!   helper and `libbrownnote_ladspa_fault_fixture`, both built from
//!   `native/plugin-host`. With `BROWNNOTE_REQUIRE_LADSPA=1` these tests
//!   fail instead of skipping when either is missing.
//! - `BROWNNOTE_LADSPA_PLATE` names the pinned `caps.so` that
//!   `native/plugin-host/caps/provision.py` builds. With
//!   `BROWNNOTE_REQUIRE_LADSPA_PLATE=1` the Plate test cannot skip.
//! - `BROWNNOTE_SAMPLES` overrides the `recordings/` directory.
#![allow(clippy::unwrap_used)]

use brownnote_native::pedals::offline::{
    self, Backend, Cancel, ErrorKind, Hash256, HelperCommand, Helpers, Layout, Limits, Param,
    Phase, RenderPolicy, Rendered, Request, Source, Stage, TailPolicy, protocol::PluginInfo,
};
use std::{
    ffi::OsString,
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

/// Every fixture plugin, which the helper loads only when allowed by name.
const FIXTURES: &[&str] = &[
    "9801:pass_stereo",
    "9802:pass_mono",
    "9803:split",
    "9804:sum",
    "9805:controls",
    "9806:nan",
    "9807:endless",
    "9808:hang",
    "9809:crash",
    "9810:init_fails",
    "9811:realtime",
    "9812:wide",
    "9813:no_output",
    "9814:bad_port",
    "9815:bad_hint",
    "9816:no_run",
];

/// The helper, optionally run through `env` with extra variables, allowed
/// to load `allow` on top of its built-in allow-list.
fn helpers(helper: &Path, env: &[(&str, &Path)], allow: &[&str]) -> Helpers {
    let mut args: Vec<OsString> = vec!["ladspa".into()];
    for id in allow {
        args.extend(["--allow".into(), (*id).into()]);
    }
    let command = if env.is_empty() {
        HelperCommand {
            program: helper.into(),
            args,
        }
    } else {
        let mut wrapped: Vec<OsString> = env
            .iter()
            .map(|(name, value)| {
                let mut pair = OsString::from(format!("{name}="));
                pair.push(value.as_os_str());
                pair
            })
            .collect();
        wrapped.push(helper.into());
        wrapped.extend(args);
        HelperCommand {
            program: "/usr/bin/env".into(),
            args: wrapped,
        }
    };
    Helpers::new().with(Backend::Ladspa, command)
}

struct Fixture {
    helper: PathBuf,
    library: PathBuf,
    helpers: Helpers,
}

fn fixture() -> Option<Fixture> {
    let helper = env_path("BROWNNOTE_LADSPA_HELPER", "BROWNNOTE_REQUIRE_LADSPA")?;
    let library = env_path("BROWNNOTE_LADSPA_FIXTURE", "BROWNNOTE_REQUIRE_LADSPA")?;
    Some(Fixture {
        helpers: helpers(&helper, &[], FIXTURES),
        helper,
        library,
    })
}

fn limits() -> Limits {
    Limits {
        open: Duration::from_secs(10),
        block: Duration::from_secs(5),
        close: Duration::from_secs(5),
        total: Duration::from_secs(120),
    }
}

fn probe_at(
    helpers: &Helpers,
    id: &str,
    location: &Path,
    rate: u32,
) -> Result<PluginInfo, offline::RenderError> {
    probe_layout(helpers, id, location, rate, Layout::Stereo)
}

fn probe_layout(
    helpers: &Helpers,
    id: &str,
    location: &Path,
    rate: u32,
    layout: Layout,
) -> Result<PluginInfo, offline::RenderError> {
    offline::probe(
        helpers,
        Backend::Ladspa,
        id,
        location,
        rate,
        layout,
        &limits(),
        &Cancel::new(),
    )
}

/// A stage for `id` whose identity is what the helper reports for
/// `pass_stereo`, with the ID swapped. Used for plugins that cannot be
/// probed because they fail to open.
fn stage(fixture: &Fixture, id: &str) -> Stage {
    let info = probe_at(&fixture.helpers, "9801:pass_stereo", &fixture.library, RATE).unwrap();
    let mut plugin = info.plugin;
    plugin.id = id.into();
    Stage {
        plugin,
        location: fixture.library.clone(),
        params: Vec::new(),
        state: None,
    }
}

fn params(values: &[(u32, f64)]) -> Vec<Param> {
    values
        .iter()
        .map(|&(id, value)| Param { id, value })
        .collect()
}

/// Two distinct, deterministic channels.
fn stereo(frames: usize, rate: u32) -> Source {
    let samples: Vec<f32> = (0..frames)
        .flat_map(|i| {
            [
                ((i % 97) as f32 - 48.0) / 128.0,
                ((i % 31) as f32 - 15.0) / 64.0,
            ]
        })
        .collect();
    Source {
        channels: 2,
        rate,
        samples: samples.into(),
    }
}

fn mono(frames: usize, rate: u32) -> Source {
    let samples: Vec<f32> = (0..frames)
        .map(|i| ((i % 53) as f32 - 26.0) / 64.0)
        .collect();
    Source {
        channels: 1,
        rate,
        samples: samples.into(),
    }
}

/// Short tail hold and cap, so pass-through fixtures finish quickly.
fn quick_policy(layout: Layout) -> RenderPolicy {
    RenderPolicy {
        layout,
        block_frames: 256,
        tail: TailPolicy {
            threshold_dbfs: -90.0,
            hold_frames: 256,
            max_frames: 4800,
        },
        compensate_latency: true,
    }
}

fn request(source: Source, stage: Stage, layout: Layout) -> Request {
    Request {
        source,
        chain: vec![stage],
        policy: quick_policy(layout),
    }
}

fn render(helpers: &Helpers, request: &Request) -> Result<Rendered, offline::RenderError> {
    offline::render(request, helpers, &limits(), &Cancel::new())
}

fn kind(helpers: &Helpers, request: &Request) -> ErrorKind {
    render(helpers, request).unwrap_err().kind
}

fn unavailable(helpers: &Helpers, request: &Request) -> String {
    match kind(helpers, request) {
        ErrorKind::Unavailable { code, .. } => code,
        other => panic!("{other:?}"),
    }
}

fn sha256_file(path: &Path) -> Hash256 {
    Hash256::of(&fs::read(path).unwrap())
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "brownnote-ladspa-offline-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fixture_identity_and_controls_follow_the_hints_at_both_rates() {
    let Some(fixture) = fixture() else {
        return;
    };
    let id = "9805:controls";
    // Independently derived from the LADSPA SDK's hint rules: SAMPLE_RATE
    // scales the bounds, a logarithmic middle is the geometric mean, an
    // integer LOW default is round(0.75 * 1 + 0.25 * 8), a missing default
    // is the lower bound, and an unbounded one is 0.
    let max = f64::from(f32::MAX);
    for rate in [44_100, 48_000] {
        let info = probe_at(&fixture.helpers, id, &fixture.library, rate).unwrap();
        assert_eq!(info.plugin.backend, Backend::Ladspa);
        assert_eq!(info.plugin.id, id);
        assert_eq!(info.plugin.abi, "ladspa 1.1");
        assert_eq!(info.plugin.version, "unversioned");
        assert_eq!(info.plugin.artifact, sha256_file(&fixture.library));
        assert_eq!(info.name, "controls");
        assert_eq!(info.latency_frames, 0);
        assert_eq!(info.tail_frames, None);
        let fs = f64::from(rate);
        let expected = [
            (3, "rate", 0.0, 0.5 * fs, 0.5 * fs, false),
            (4, "log", 20.0, 20_000.0, (20.0f64 * 20_000.0).sqrt(), false),
            (5, "int", 1.0, 8.0, 3.0, true),
            (6, "toggle", 0.0, 1.0, 1.0, true),
            (7, "bounded", -2.0, 5.0, -2.0, false),
            (8, "free", -max, max, 0.0, false),
        ];
        assert_eq!(info.params.len(), expected.len(), "{:?}", info.params);
        for (param, (id, name, min, max, default, stepped)) in info.params.iter().zip(expected) {
            assert_eq!(
                (param.id, param.name.as_str(), param.stepped),
                (id, name, stepped)
            );
            // JSON does not round-trip every f64 exactly (the f32::MAX bounds).
            let close = |a: f64, b: f64| (a - b).abs() <= 1e-12 * b.abs().max(1.0);
            assert!(
                close(param.min, min) && close(param.max, max) && close(param.default, default),
                "{param:?} at {rate}"
            );
        }

        // The plugin receives those defaults: its left output carries one
        // control input per frame.
        let mut stage = Stage {
            plugin: info.plugin.clone(),
            location: fixture.library.clone(),
            params: Vec::new(),
            state: None,
        };
        let rendered = render(
            &fixture.helpers,
            &request(mono(480, rate), stage.clone(), Layout::Stereo),
        )
        .unwrap();
        let left: Vec<f32> = rendered
            .samples
            .iter()
            .step_by(2)
            .take(7)
            .copied()
            .collect();
        let defaults: Vec<f32> = expected.iter().map(|e| e.4 as f32).collect();
        assert_eq!(left[..6], defaults[..], "at {rate}");
        assert_eq!(left[6], 0.0);

        // Values within the hints arrive exactly, at every bound.
        let set = [
            (3, 0.5 * fs),
            (4, 20.0),
            (5, 8.0),
            (6, 0.0),
            (7, 5.0),
            (8, -123.5),
        ];
        stage.params = params(&set);
        let changed = render(
            &fixture.helpers,
            &request(mono(480, rate), stage.clone(), Layout::Stereo),
        )
        .unwrap();
        let left: Vec<f32> = changed.samples.iter().step_by(2).take(6).copied().collect();
        let wanted: Vec<f32> = set.iter().map(|(_, v)| *v as f32).collect();
        assert_eq!(left, wanted);
        assert_ne!(changed.identity, rendered.identity);
    }

    // The sample-rate bound moves with the rate.
    let info = probe_at(&fixture.helpers, id, &fixture.library, 48_000).unwrap();
    let mut stage = Stage {
        plugin: info.plugin,
        location: fixture.library.clone(),
        params: params(&[(3, 23_000.0)]),
        state: None,
    };
    render(
        &fixture.helpers,
        &request(mono(480, 48_000), stage.clone(), Layout::Stereo),
    )
    .unwrap();
    assert_eq!(
        unavailable(
            &fixture.helpers,
            &request(mono(480, 44_100), stage.clone(), Layout::Stereo)
        ),
        "param-rejected"
    );

    // Out of range, not whole, not 0/1, beyond f32, or not a control input:
    // rejected, never clamped.
    for (port, value) in [
        (4, 19.0),
        (5, 2.5),
        (6, 0.5),
        (7, 5.5),
        (8, 1e39),
        (1, 0.0),
        (9, 0.0),
        (99, 0.0),
    ] {
        stage.params = params(&[(port, value)]);
        assert_eq!(
            unavailable(
                &fixture.helpers,
                &request(mono(480, RATE), stage.clone(), Layout::Stereo)
            ),
            "param-rejected",
            "port {port} = {value}"
        );
    }
}

#[test]
fn fixture_channel_wiring_is_exact_and_unsupported_layouts_are_refused() {
    let Some(fixture) = fixture() else {
        return;
    };
    let h = &fixture.helpers;
    let probe = |id, layout| {
        let info = probe_layout(h, id, &fixture.library, RATE, layout).unwrap();
        Stage {
            plugin: info.plugin,
            location: fixture.library.clone(),
            params: Vec::new(),
            state: None,
        }
    };
    let source = stereo(2000, RATE);

    // Two in, two out: identity. The plugin is INPLACE_BROKEN, writes NaN
    // if its buffers alias, aborts if any port is unconnected, and its
    // run_adding writes NaN: exact output proves none of that happened.
    let rendered = render(
        h,
        &request(
            source.clone(),
            probe("9801:pass_stereo", Layout::Stereo),
            Layout::Stereo,
        ),
    )
    .unwrap();
    assert_eq!(rendered.samples, source.samples);
    assert_eq!(rendered.stages[0].tail_frames, 0);
    assert_eq!(rendered.stages[0].state, None);

    // One in, two out, stereo render: the input is the average of the two
    // channels.
    let split = render(
        h,
        &request(
            source.clone(),
            probe("9803:split", Layout::Stereo),
            Layout::Stereo,
        ),
    )
    .unwrap();
    let (frames, _) = split.samples.as_chunks::<2>();
    let (inputs, _) = source.samples.as_chunks::<2>();
    for (out, [l, r]) in frames.iter().zip(inputs) {
        let average = 0.5 * (l + r);
        assert_eq!(*out, [average, -average]);
    }

    // A mono source in a stereo render reaches a one-input plugin unchanged.
    let narrow = mono(2000, RATE);
    let split = render(
        h,
        &request(
            narrow.clone(),
            probe("9803:split", Layout::Stereo),
            Layout::Stereo,
        ),
    )
    .unwrap();
    let (frames, _) = split.samples.as_chunks::<2>();
    for (out, x) in frames.iter().zip(narrow.samples.iter()) {
        assert_eq!(*out, [*x, -*x]);
    }

    // Two in, one out, mono render: the mono signal feeds both inputs.
    let sum = render(
        h,
        &request(
            narrow.clone(),
            probe("9804:sum", Layout::Mono),
            Layout::Mono,
        ),
    )
    .unwrap();
    let doubled: Vec<f32> = narrow.samples.iter().map(|x| x + x).collect();
    assert_eq!(&sum.samples[..], &doubled[..]);
    let mono_pass = render(
        h,
        &request(
            narrow.clone(),
            probe("9802:pass_mono", Layout::Mono),
            Layout::Mono,
        ),
    )
    .unwrap();
    assert_eq!(mono_pass.samples, narrow.samples);

    // The plugin's output count must equal the render's channels.
    for (id, layout) in [
        ("9803:split", Layout::Mono),
        ("9802:pass_mono", Layout::Stereo),
        ("9812:wide", Layout::Stereo),
        ("9813:no_output", Layout::Stereo),
    ] {
        let source = if layout == Layout::Mono {
            narrow.clone()
        } else {
            source.clone()
        };
        assert!(
            matches!(
                kind(h, &request(source, stage(&fixture, id), layout)),
                ErrorKind::Unsupported(_)
            ),
            "{id} {layout:?}"
        );
    }
}

#[test]
fn fixture_lifecycle_and_library_discovery() {
    let Some(fixture) = fixture() else {
        return;
    };
    let dir = temp_dir("lifecycle");
    let log = dir.join("lifecycle.log");
    let logged = helpers(
        &fixture.helper,
        &[("BROWNNOTE_LADSPA_FIXTURE_LOG", &log)],
        FIXTURES,
    );
    let pass = stage(&fixture, "9801:pass_stereo");
    let source = stereo(1000, RATE);
    render(
        &logged,
        &request(source.clone(), pass.clone(), Layout::Stereo),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "pass_stereo instantiate\npass_stereo activate\npass_stereo deactivate\npass_stereo cleanup\n"
    );

    // A bare file name is looked up in LADSPA_PATH.
    let name = fixture.library.file_name().unwrap().to_str().unwrap();
    let folder = fixture.library.parent().unwrap();
    let searched = helpers(&fixture.helper, &[("LADSPA_PATH", folder)], FIXTURES);
    let mut bare = pass.clone();
    bare.location = name.into();
    let found = render(
        &searched,
        &request(source.clone(), bare.clone(), Layout::Stereo),
    )
    .unwrap();
    assert_eq!(found.samples, source.samples);
    assert_eq!(found.stages[0].plugin, pass.plugin);
    let empty = helpers(&fixture.helper, &[("LADSPA_PATH", &dir)], FIXTURES);
    assert_eq!(
        unavailable(&empty, &request(source, bare, Layout::Stereo)),
        "bundle-missing"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fixture_load_failures_are_typed_and_nothing_unlisted_is_loaded() {
    let Some(fixture) = fixture() else {
        return;
    };
    let h = &fixture.helpers;
    let source = stereo(480, RATE);
    let code = |stage: Stage| unavailable(h, &request(source.clone(), stage, Layout::Stereo));
    let unsupported = |helpers: &Helpers, stage: Stage| match kind(
        helpers,
        &request(source.clone(), stage, Layout::Stereo),
    ) {
        ErrorKind::Unsupported(message) => message,
        other => panic!("{other:?}"),
    };

    for (id, expected) in [
        ("9814:bad_port", "invalid-descriptor"),
        ("9815:bad_hint", "invalid-descriptor"),
        ("9816:no_run", "invalid-descriptor"),
        ("9810:init_fails", "init-failed"),
    ] {
        assert_eq!(code(stage(&fixture, id)), expected, "{id}");
    }
    let unknown = helpers(
        &fixture.helper,
        &[],
        &["9801:nothing", "9899:pass_stereo", "pass"],
    );
    for id in ["9801:nothing", "9899:pass_stereo", "pass"] {
        assert_eq!(
            unavailable(
                &unknown,
                &request(source.clone(), stage(&fixture, id), Layout::Stereo)
            ),
            "unknown-plugin-id",
            "{id}"
        );
    }
    assert!(unsupported(h, stage(&fixture, "9811:realtime")).contains("REALTIME"));

    // LADSPA has no state; a state blob is refused, not ignored.
    let mut with_state = stage(&fixture, "9801:pass_stereo");
    with_state.state = Some(Arc::from(&[1u8, 2, 3][..]));
    assert!(unsupported(h, with_state).contains("no state"));

    // Without --allow, only CAPS Plate may load: the fixture is refused
    // before its library is opened, so even a corrupt file is "unsupported".
    let default = helpers(&fixture.helper, &[], &[]);
    let dir = temp_dir("load");
    let junk = dir.join("junk.so");
    fs::write(&junk, "not a library").unwrap();
    let mut unlisted = stage(&fixture, "9801:pass_stereo");
    assert!(unsupported(&default, unlisted.clone()).contains("allow-list"));
    unlisted.location = junk.clone();
    assert!(unsupported(&default, unlisted).contains("allow-list"));

    let mut missing = stage(&fixture, "9801:pass_stereo");
    missing.location = dir.join("absent.so");
    assert_eq!(code(missing), "bundle-missing");
    let mut corrupt = stage(&fixture, "9801:pass_stereo");
    corrupt.location = junk;
    assert_eq!(code(corrupt), "dlopen-failed");
    // A real dynamic library without `ladspa_descriptor`: the CLAP fault
    // fixture, built beside this one.
    let clap = fixture.library.with_file_name(
        fixture
            .library
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace("ladspa_fault_fixture", "clap_fault_fixture"),
    );
    if clap.exists() {
        let mut no_entry = stage(&fixture, "9801:pass_stereo");
        no_entry.location = clap;
        assert_eq!(code(no_entry), "no-entry");
    } else {
        skip(
            "BROWNNOTE_REQUIRE_LADSPA",
            &format!("{} does not exist", clap.display()),
        );
    }

    // A library that is not the one the request names.
    let mut changed = stage(&fixture, "9801:pass_stereo");
    changed.plugin.artifact = Hash256([7; 32]);
    assert!(matches!(
        kind(h, &request(source.clone(), changed, Layout::Stereo)),
        ErrorKind::ArtifactChanged { .. }
    ));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fixture_faults_fail_without_taking_the_app_down() {
    let Some(fixture) = fixture() else {
        return;
    };
    let h = &fixture.helpers;
    let source = stereo(4800, RATE);
    let fault = |id: &str| request(source.clone(), stage(&fixture, id), Layout::Stereo);

    // Faults start at frame 1000, inside block 3 of 256 frames.
    let error = render(h, &fault("9809:crash")).unwrap_err();
    assert_eq!(error.stage, Some(0));
    assert_eq!(error.plugin.as_ref().unwrap().id, "9809:crash");
    match error.kind {
        ErrorKind::Exited { status, .. } => assert!(status.contains("signal: 11"), "{status}"),
        other => panic!("{other:?}"),
    }

    assert_eq!(
        kind(h, &fault("9806:nan")),
        ErrorKind::NonFinite { frame: 1000 }
    );

    let quick = Limits {
        block: Duration::from_millis(300),
        ..limits()
    };
    let started = Instant::now();
    let error = offline::render(&fault("9808:hang"), h, &quick, &Cancel::new()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout(Phase::Block(3)));
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
    let error = offline::render(&fault("9808:hang"), h, &limits(), &cancel).unwrap_err();
    let returned = Instant::now();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    let latency = returned.duration_since(canceller.join().unwrap());
    assert!(
        latency < Duration::from_millis(250),
        "cancel took {latency:?}"
    );

    // A tail that never decays stops at the cap and says so.
    let endless = render(h, &fault("9807:endless")).unwrap();
    let report = &endless.stages[0];
    assert!(report.tail_capped);
    assert_eq!(report.tail_frames, 4800);
    assert_eq!(endless.frames(), 4800 + 4800);

    // The same helper still serves a healthy plugin afterwards.
    let ok = render(h, &fault("9801:pass_stereo")).unwrap();
    assert_eq!(ok.samples, source.samples);
}

// CAPS 0.9.26 Plate, as pinned and built by native/plugin-host/caps (#97).
const PLATE_ID: &str = "1779:Plate";
const PLATE_LIBRARY_SHA256: &str =
    "83b72440184988e806eb2c0d0d94ccf9fe5fa030b41fbcf16a415e017ccc4957";
/// SHA-256 of the interleaved little-endian f32 samples in #97's reference
/// render (`plate_pad/render.wav`, file SHA-256 `e2e04bc4…c9c4`):
/// `recordings/fart_dry.wav` through Plate at its defaults, block 256,
/// tail until 50 ms below -90 dBFS.
const PLATE_REFERENCE_SHA256: &str =
    "8091ae74c2edd34f08a62361075e23dc50bc0a90533d70cc045e3c18e4882800";
const REFERENCE_FRAMES: usize = 228_936;
const REFERENCE_TAIL: usize = 149_556;

fn samples_dir() -> PathBuf {
    std::env::var_os("BROWNNOTE_SAMPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../recordings"))
}

/// Decoded like #97's probe: PCM16 divided by 32768.
fn pad() -> Source {
    let mut reader = hound::WavReader::open(samples_dir().join("fart_dry.wav")).unwrap();
    let spec = reader.spec();
    assert_eq!((spec.channels, spec.bits_per_sample), (1, 16));
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect();
    Source {
        channels: 1,
        rate: spec.sample_rate,
        samples: samples.into(),
    }
}

/// #97's measurement: 50 ms of hold below -90 dBFS, capped at 30 s.
fn plate_policy(rate: u32, block_frames: u32) -> RenderPolicy {
    RenderPolicy {
        layout: Layout::Stereo,
        block_frames,
        tail: TailPolicy {
            threshold_dbfs: -90.0,
            hold_frames: rate.div_ceil(20),
            max_frames: 30 * rate,
        },
        compensate_latency: true,
    }
}

fn samples_sha256(rendered: &Rendered) -> String {
    let bytes: Vec<u8> = rendered
        .samples
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    Hash256::of(&bytes).to_string()
}

#[test]
fn pinned_plate_reproduces_the_reference_render() {
    let Some(helper) = env_path("BROWNNOTE_LADSPA_HELPER", "BROWNNOTE_REQUIRE_LADSPA_PLATE") else {
        return;
    };
    let Some(library) = env_path("BROWNNOTE_LADSPA_PLATE", "BROWNNOTE_REQUIRE_LADSPA_PLATE") else {
        return;
    };
    // No --allow: Plate is on the helper's built-in allow-list.
    let h = helpers(&helper, &[], &[]);
    let source = pad();
    let rate = source.rate;
    assert_eq!((rate, source.frames()), (44_100, 79_380));

    let mut probed = Vec::new();
    for rate in [44_100, 48_000] {
        let info = probe_at(&h, PLATE_ID, &library, rate).unwrap();
        assert_eq!(info.plugin.artifact.to_string(), PLATE_LIBRARY_SHA256);
        assert_eq!(info.plugin.abi, "ladspa 1.1");
        assert_eq!(info.plugin.version, "unversioned");
        assert_eq!(info.name, "C* Plate - Versatile plate reverb");
        assert_eq!((info.latency_frames, info.tail_frames), (0, None));
        let params: Vec<_> = info
            .params
            .iter()
            .map(|p| (p.id, p.name.as_str(), p.min, p.max, p.default, p.stepped))
            .collect();
        assert_eq!(
            params,
            [
                (0, "bandwidth", 0.0, 1.0, 0.5, false),
                (1, "tail", 0.0, 1.0, 0.75, false),
                (2, "damping", 0.0, 1.0, 0.5, false),
                (3, "blend", 0.0, 1.0, 0.25, false),
            ]
        );
        probed.push(info);
    }
    let plate = Stage {
        plugin: probed.remove(0).plugin,
        location: library.clone(),
        params: Vec::new(),
        state: None,
    };
    let reference = Request {
        source: source.clone(),
        chain: vec![plate.clone()],
        policy: plate_policy(rate, 256),
    };
    let started = Instant::now();
    let rendered = render(&h, &reference).unwrap();
    let report = &rendered.stages[0];
    println!(
        "plate: {} frames, tail {} ({:.3} s), capped {}, in {:?}",
        rendered.frames(),
        report.tail_frames,
        report.tail_frames as f64 / f64::from(rate),
        report.tail_capped,
        started.elapsed()
    );
    assert_eq!(rendered.frames(), REFERENCE_FRAMES);
    assert_eq!(report.tail_frames, REFERENCE_TAIL);
    assert!(!report.tail_capped);
    assert_eq!(report.state, None);
    assert_eq!(
        samples_sha256(&rendered),
        PLATE_REFERENCE_SHA256,
        "the helper must reproduce #97's reference render sample for sample"
    );

    // A fresh helper process, a 64-frame block, and discovery through
    // LADSPA_PATH all give the same samples.
    assert_eq!(render(&h, &reference).unwrap().samples, rendered.samples);
    let mut by_64 = reference.clone();
    by_64.policy.block_frames = 64;
    let by_64 = render(&h, &by_64).unwrap();
    assert_ne!(by_64.identity, rendered.identity);
    assert_eq!(by_64.samples, rendered.samples);
    let searched = helpers(&helper, &[("LADSPA_PATH", library.parent().unwrap())], &[]);
    let mut bare = reference.clone();
    bare.chain[0].location = library.file_name().unwrap().into();
    assert_eq!(bare.identity(), reference.identity());
    assert_eq!(render(&searched, &bare).unwrap().samples, rendered.samples);

    // blend = 0 is exactly the dry pad on both channels, with no tail.
    let mut dry = reference.clone();
    dry.chain[0].params = params(&[(3, 0.0)]);
    let dry = render(&h, &dry).unwrap();
    assert_ne!(dry.identity, rendered.identity);
    assert_eq!(dry.stages[0].tail_frames, 0);
    let (frames, _) = dry.samples.as_chunks::<2>();
    assert_eq!(frames.len(), source.frames());
    for (frame, x) in frames.iter().zip(source.samples.iter()) {
        assert_eq!(*frame, [*x, *x]);
    }

    // Plate's tail scales with the sample rate: a 100 ms burst decays in
    // the same time at 44.1 and 48 kHz.
    let mut tails = Vec::new();
    for (index, rate) in [44_100u32, 48_000].into_iter().enumerate() {
        let frames = rate as usize / 10;
        let burst: Vec<f32> = (0..frames)
            .map(|i| {
                0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32
            })
            .collect();
        let mut stage = plate.clone();
        stage.plugin = if index == 0 {
            plate.plugin.clone()
        } else {
            probed[0].plugin.clone()
        };
        let burst = render(
            &h,
            &Request {
                source: Source {
                    channels: 1,
                    rate,
                    samples: burst.into(),
                },
                chain: vec![stage],
                policy: plate_policy(rate, 256),
            },
        )
        .unwrap();
        tails.push(burst.stages[0].tail_frames as f64 / f64::from(rate));
    }
    println!("burst tails: {tails:?} s");
    assert!(tails[0] > 2.0, "{tails:?}");
    assert!((tails[1] / tails[0] - 1.0).abs() < 0.01, "{tails:?}");

    // One input and two outputs: a mono render is refused rather than
    // downmixed, and LADSPA has no state to restore.
    let mut mono_render = reference.clone();
    mono_render.policy.layout = Layout::Mono;
    assert!(matches!(kind(&h, &mono_render), ErrorKind::Unsupported(_)));
    let mut stateful = reference;
    stateful.chain[0].state = Some(Arc::from(&[0u8][..]));
    assert!(matches!(kind(&h, &stateful), ErrorKind::Unsupported(_)));
}
