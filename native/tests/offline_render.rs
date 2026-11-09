//! The backend-neutral offline render boundary, driven through real helper
//! processes. `fixtures/offline_render/fake_helper.py` hosts scripted fake
//! plugins, including ones that crash, hang, emit NaN or break the protocol.
#![allow(clippy::unwrap_used)]

use brownnote_native::pedals::offline::{
    self, Backend, Cancel, ErrorKind, Hash256, HelperCommand, Helpers, Layout, Limits, Param,
    Phase, Plugin, RenderPolicy, Request, Source, Stage, TailPolicy, protocol::PluginInfo,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const RATE: u32 = 48_000;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/offline_render/fake_helper.py")
}

fn helpers_with(args: Vec<std::ffi::OsString>) -> Helpers {
    let mut all = vec![fixture().into_os_string()];
    all.extend(args);
    let command = HelperCommand {
        program: "python3".into(),
        args: all,
    };
    Helpers::new()
        .with(Backend::Clap, command.clone())
        .with(Backend::Ladspa, command)
}

fn helpers() -> Helpers {
    helpers_with(Vec::new())
}

struct Dir(PathBuf);
impl Dir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("brownnote-offline-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    /// A stand-in plugin binary; its bytes are the artifact identity.
    fn artifact(&self, bytes: &[u8]) -> PathBuf {
        let path = self.0.join("plugin.bin");
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn limits() -> Limits {
    Limits {
        open: Duration::from_secs(10),
        block: Duration::from_secs(5),
        close: Duration::from_secs(5),
        total: Duration::from_secs(60),
    }
}

fn probe(id: &str, location: &Path) -> PluginInfo {
    offline::probe(
        &helpers(),
        Backend::Clap,
        id,
        location,
        RATE,
        Layout::Mono,
        &limits(),
        &Cancel::new(),
    )
    .unwrap()
}

fn stage(id: &str, location: &Path) -> Stage {
    Stage {
        plugin: probe(id, location).plugin,
        location: location.into(),
        params: Vec::new(),
        state: None,
    }
}

/// A 10 ms 440 Hz burst.
fn burst() -> Source {
    let samples: Vec<f32> = (0..480)
        .map(|i| 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin())
        .collect();
    Source {
        channels: 1,
        rate: RATE,
        samples: samples.into(),
    }
}

fn policy() -> RenderPolicy {
    RenderPolicy {
        layout: Layout::Mono,
        block_frames: 256,
        tail: TailPolicy {
            threshold_dbfs: -90.0,
            hold_frames: 2400,
            max_frames: RATE,
        },
        compensate_latency: true,
    }
}

fn request(chain: Vec<Stage>) -> Request {
    Request {
        source: burst(),
        chain,
        policy: policy(),
    }
}

fn render(request: &Request) -> Result<offline::Rendered, offline::RenderError> {
    offline::render(request, &helpers(), &limits(), &Cancel::new())
}

fn kind(request: &Request) -> ErrorKind {
    render(request).unwrap_err().kind
}

#[test]
fn probe_reports_identity_latency_tail_and_params() {
    let dir = Dir::new("probe");
    let location = dir.artifact(b"echo v1");
    let info = probe("fake.echo", &location);
    assert_eq!(
        info.plugin,
        Plugin {
            backend: Backend::Clap,
            id: "fake.echo".into(),
            artifact: Hash256::of(b"echo v1"),
            abi: "fake 1".into(),
            version: "1.0".into(),
        }
    );
    assert_eq!(info.latency_frames, 0);
    assert_eq!(info.tail_frames, Some(RATE / 20));
    assert_eq!(info.params[0].name, "Gain");
}

#[test]
fn renders_gain_exactly_and_returns_state() {
    let dir = Dir::new("gain");
    let location = dir.artifact(b"gain");
    let mut gain = stage("fake.gain", &location);
    gain.params = vec![Param { id: 0, value: 0.25 }];
    let request = request(vec![gain]);
    let rendered = render(&request).unwrap();
    let source = burst();
    assert_eq!(rendered.identity, request.identity());
    assert_eq!(rendered.source_frames, 480);
    assert_eq!(rendered.frames(), 480, "a gain has no tail");
    for (out, inp) in rendered.samples.iter().zip(source.samples.iter()) {
        assert_eq!(*out, inp * 0.25);
    }
    let report = &rendered.stages[0];
    assert_eq!(report.tail_frames, 0);
    assert!(!report.tail_capped);
    assert_eq!(
        report.state.as_deref(),
        Some(&0.25f64.to_le_bytes()[..]),
        "the state after rendering is returned"
    );
}

#[test]
fn state_is_applied_before_params() {
    let dir = Dir::new("state");
    let location = dir.artifact(b"gain");
    let mut gain = stage("fake.gain", &location);
    gain.state = Some(Arc::from(&2.0f64.to_le_bytes()[..]));
    let from_state = render(&request(vec![gain.clone()])).unwrap();
    assert_eq!(from_state.samples[100], burst().samples[100] * 2.0);
    gain.params = vec![Param { id: 0, value: 0.75 }];
    let overridden = render(&request(vec![gain])).unwrap();
    assert_eq!(overridden.samples[100], burst().samples[100] * 0.75);
}

#[test]
fn tail_is_measured_not_trusted() {
    let dir = Dir::new("tail");
    let location = dir.artifact(b"echo");
    let rendered = render(&request(vec![stage("fake.echo", &location)])).unwrap();
    let report = &rendered.stages[0];
    let declared = report.declared_tail_frames.unwrap() as usize;
    assert!(
        report.tail_frames > declared,
        "measured tail {} must exceed the declared {declared}",
        report.tail_frames
    );
    assert!(!report.tail_capped);
    assert_eq!(rendered.frames(), 480 + report.tail_frames);
    let threshold = 10f32.powf(-90.0 / 20.0);
    let last = *rendered.samples.last().unwrap();
    assert!(
        last.abs() >= threshold,
        "output ends at its last audible frame"
    );
    // 0.5 feedback every 480 frames needs about 13 repeats to fall 90 dB.
    assert!(
        (480 * 10..480 * 16).contains(&report.tail_frames),
        "{}",
        report.tail_frames
    );
}

#[test]
fn an_endless_tail_is_capped_and_reported() {
    let dir = Dir::new("drone");
    let location = dir.artifact(b"drone");
    let mut request = request(vec![stage("fake.drone", &location)]);
    request.policy.tail.max_frames = 4800;
    let rendered = render(&request).unwrap();
    assert!(rendered.tail_capped());
    assert_eq!(rendered.stages[0].tail_frames, 4800);
    assert_eq!(rendered.frames(), 480 + 4800);
    assert_eq!(rendered.stages[0].declared_tail_frames, Some(u32::MAX));
}

#[test]
fn latency_is_compensated_when_asked() {
    let dir = Dir::new("latency");
    let location = dir.artifact(b"latency");
    let source = burst();
    let mut request = request(vec![stage("fake.latency", &location)]);
    let aligned = render(&request).unwrap();
    assert_eq!(aligned.stages[0].latency_frames, 64);
    assert_eq!(&aligned.samples[..480], &source.samples[..]);
    request.policy.compensate_latency = false;
    let late = render(&request).unwrap();
    assert!(late.samples[..64].iter().all(|s| *s == 0.0));
    assert_eq!(&late.samples[64..544], &source.samples[..]);
    assert_ne!(aligned.identity, late.identity);
}

#[test]
fn mono_sources_are_duplicated_into_stereo() {
    let dir = Dir::new("stereo");
    let location = dir.artifact(b"gain");
    let mut request = request(vec![stage("fake.gain", &location)]);
    request.policy.layout = Layout::Stereo;
    let rendered = render(&request).unwrap();
    assert_eq!(rendered.frames(), 480);
    let (frames, _) = rendered.samples.as_chunks::<2>();
    for (frame, inp) in frames.iter().zip(burst().samples.iter()) {
        assert_eq!(*frame, [inp * 0.5, inp * 0.5]);
    }
    let mut stereo_into_mono = request.clone();
    stereo_into_mono.policy.layout = Layout::Mono;
    stereo_into_mono.source.channels = 2;
    stereo_into_mono.source.samples = rendered.samples.clone();
    assert!(matches!(kind(&stereo_into_mono), ErrorKind::Invalid(_)));
}

#[test]
fn chain_order_changes_audio_and_identity() {
    let dir = Dir::new("order");
    let location = dir.artifact(b"x");
    let gain = stage("fake.gain", &location);
    let drone = stage("fake.drone", &location);
    let mut first = request(vec![gain.clone(), drone.clone()]);
    first.policy.tail.max_frames = 0;
    let mut second = first.clone();
    second.chain = vec![drone, gain];
    let a = render(&first).unwrap();
    let b = render(&second).unwrap();
    assert_ne!(a.identity, b.identity);
    // gain then drone: x/2 + 0.1; drone then gain: (x + 0.1)/2.
    let x = burst().samples[0];
    assert_eq!(a.samples[0], x * 0.5 + 0.1);
    assert_eq!(b.samples[0], (x + 0.1) * 0.5);
}

#[test]
fn every_identity_component_changes_the_key() {
    let dir = Dir::new("identity");
    let location = dir.artifact(b"gain");
    let base = request(vec![stage("fake.gain", &location)]);
    assert_eq!(base.identity(), base.clone().identity());
    let mut moved = base.clone();
    moved.chain[0].location = PathBuf::from("/elsewhere/plugin.bin");
    assert_eq!(
        base.identity(),
        moved.identity(),
        "location is machine-local"
    );

    type Change = fn(&mut Request);
    let changes: Vec<(&str, Change)> = vec![
        ("source sample", |r| {
            let mut s = r.source.samples.to_vec();
            s[7] += 1e-6;
            r.source.samples = s.into();
        }),
        ("source rate", |r| r.source.rate = 44_100),
        ("source channels", |r| {
            r.source.channels = 2;
            r.policy.layout = Layout::Stereo;
            r.source.samples = r.source.samples.iter().flat_map(|s| [*s, *s]).collect();
        }),
        ("layout", |r| r.policy.layout = Layout::Stereo),
        ("backend", |r| r.chain[0].plugin.backend = Backend::Ladspa),
        ("plugin id", |r| r.chain[0].plugin.id = "fake.other".into()),
        ("artifact", |r| {
            r.chain[0].plugin.artifact = Hash256::of(b"other")
        }),
        ("abi", |r| r.chain[0].plugin.abi = "fake 2".into()),
        ("version", |r| r.chain[0].plugin.version = "1.1".into()),
        ("param value", |r| {
            r.chain[0].params = vec![Param { id: 0, value: 0.5 }]
        }),
        ("param id", |r| {
            r.chain[0].params = vec![Param { id: 1, value: 0.5 }]
        }),
        ("state", |r| r.chain[0].state = Some(Arc::from(&[1u8][..]))),
        ("state bytes", |r| {
            r.chain[0].state = Some(Arc::from(&[2u8][..]))
        }),
        ("empty state", |r| {
            r.chain[0].state = Some(Arc::from(&[][..]))
        }),
        ("chain length", |r| r.chain.push(r.chain[0].clone())),
        ("block size", |r| r.policy.block_frames = 512),
        ("tail threshold", |r| r.policy.tail.threshold_dbfs = -80.0),
        ("tail hold", |r| r.policy.tail.hold_frames += 1),
        ("tail cap", |r| r.policy.tail.max_frames += 1),
        ("latency policy", |r| r.policy.compensate_latency = false),
    ];
    let mut seen = vec![base.identity()];
    for (name, change) in changes {
        let mut changed = base.clone();
        change(&mut changed);
        let identity = changed.identity();
        assert!(
            !seen.contains(&identity),
            "{name} did not change the identity"
        );
        seen.push(identity);
    }
    let mut ordered = base.clone();
    ordered.chain.push(stage("fake.echo", &location));
    let mut reversed = ordered.clone();
    reversed.chain.reverse();
    assert_ne!(ordered.identity(), reversed.identity(), "chain order");
}

#[test]
fn invalid_requests_are_rejected_before_any_helper_starts() {
    let dir = Dir::new("invalid");
    let location = dir.artifact(b"gain");
    let base = request(vec![stage("fake.gain", &location)]);
    type Change = fn(&mut Request);
    let changes: Vec<(&str, Change)> = vec![
        ("empty source", |r| r.source.samples = Arc::from(&[][..])),
        ("NaN source", |r| {
            r.source.samples = Arc::from(&[0.0, f32::NAN][..])
        }),
        ("rate", |r| r.source.rate = 1000),
        ("partial frame", |r| {
            r.source.channels = 2;
            r.policy.layout = Layout::Stereo;
            r.source.samples = Arc::from(&[0.0, 0.0, 0.0][..]);
        }),
        ("empty chain", |r| r.chain.clear()),
        ("long chain", |r| r.chain = vec![r.chain[0].clone(); 5]),
        ("NaN param", |r| {
            r.chain[0].params = vec![Param {
                id: 0,
                value: f64::NAN,
            }]
        }),
        ("infinite param", |r| {
            r.chain[0].params = vec![Param {
                id: 0,
                value: f64::INFINITY,
            }]
        }),
        ("unsorted params", |r| {
            r.chain[0].params = vec![Param { id: 2, value: 0.0 }, Param { id: 1, value: 0.0 }]
        }),
        ("duplicate params", |r| {
            r.chain[0].params = vec![Param { id: 1, value: 0.0 }, Param { id: 1, value: 0.0 }]
        }),
        ("tiny block", |r| r.policy.block_frames = 8),
        ("huge block", |r| r.policy.block_frames = 16_384),
        ("threshold", |r| r.policy.tail.threshold_dbfs = f64::NAN),
        ("loud threshold", |r| r.policy.tail.threshold_dbfs = -6.0),
        ("no hold", |r| r.policy.tail.hold_frames = 0),
        ("tail cap", |r| r.policy.tail.max_frames = 31 * RATE),
        ("empty id", |r| r.chain[0].plugin.id = String::new()),
        ("control id", |r| r.chain[0].plugin.id = "a\nb".into()),
    ];
    for (name, change) in changes {
        let mut invalid = base.clone();
        change(&mut invalid);
        let error =
            offline::render(&invalid, &Helpers::new(), &limits(), &Cancel::new()).unwrap_err();
        assert!(
            matches!(error.kind, ErrorKind::Invalid(_)),
            "{name}: {error}"
        );
        assert_eq!(error.stage, None);
    }
}

#[test]
fn load_failures_are_typed_and_name_the_stage() {
    let dir = Dir::new("load");
    let location = dir.artifact(b"gain");
    let gain = stage("fake.gain", &location);

    let mut missing = gain.clone();
    missing.location = dir.0.join("gone.bin");
    let error = render(&request(vec![gain.clone(), missing])).unwrap_err();
    assert_eq!(error.stage, Some(1));
    assert_eq!(error.plugin.as_deref(), Some(&gain.plugin));
    assert!(matches!(&error.kind, ErrorKind::Unavailable { code, .. } if code == "bundle-missing"));
    assert!(
        error
            .to_string()
            .starts_with("Plugin stage 2 (CLAP fake.gain): plugin unavailable")
    );

    let mut unknown = gain.clone();
    unknown.plugin.id = "fake.nothing".into();
    assert!(matches!(
        kind(&request(vec![unknown])),
        ErrorKind::Unavailable { code, .. } if code == "unknown-plugin-id"
    ));

    let mut realtime = gain.clone();
    realtime.plugin.id = "fake.realtime-only".into();
    assert!(matches!(
        kind(&request(vec![realtime])),
        ErrorKind::Unsupported(_)
    ));

    let mut bad_state = gain.clone();
    bad_state.state = Some(Arc::from(&[1u8, 2, 3][..]));
    assert!(matches!(
        kind(&request(vec![bad_state])),
        ErrorKind::Unavailable { code, .. } if code == "state-rejected"
    ));

    let mut wrong = gain.clone();
    wrong.plugin.id = "fake.wrong-id".into();
    assert!(matches!(
        kind(&request(vec![wrong])),
        ErrorKind::Protocol(_)
    ));

    let none = offline::render(
        &request(vec![gain.clone()]),
        &Helpers::new(),
        &limits(),
        &Cancel::new(),
    )
    .unwrap_err();
    assert_eq!(none.kind, ErrorKind::NoHelper(Backend::Clap));

    let absent = Helpers::new().with(
        Backend::Clap,
        HelperCommand {
            program: dir.0.join("no-such-helper"),
            args: Vec::new(),
        },
    );
    let error =
        offline::render(&request(vec![gain]), &absent, &limits(), &Cancel::new()).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::Spawn(_)), "{error}");
}

#[test]
fn a_replaced_plugin_binary_is_detected() {
    let dir = Dir::new("replaced");
    let location = dir.artifact(b"gain v1");
    let gain = stage("fake.gain", &location);
    dir.artifact(b"gain v2");
    match kind(&request(vec![gain.clone()])) {
        ErrorKind::ArtifactChanged { expected, found } => {
            assert_eq!(*expected, gain.plugin);
            assert_eq!(found.artifact, Hash256::of(b"gain v2"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn plugin_faults_fail_the_render_without_partial_audio() {
    let dir = Dir::new("faults");
    let location = dir.artifact(b"fault");
    let fault = |id: &str| {
        let mut s = stage("fake.gain", &location);
        s.plugin.id = id.into();
        request(vec![s])
    };

    match kind(&fault("fake.crash")) {
        ErrorKind::Exited { status, .. } => assert!(status.contains("signal"), "{status}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        kind(&fault("fake.nonfinite")),
        ErrorKind::NonFinite { frame: 3 * 256 + 5 }
    );
    assert!(matches!(
        kind(&fault("fake.fail")),
        ErrorKind::Failed { code, .. } if code == "process-failed"
    ));
    assert!(matches!(
        kind(&fault("fake.wrong-length")),
        ErrorKind::Protocol(_)
    ));
    assert!(matches!(
        kind(&fault("fake.garbage")),
        ErrorKind::Protocol(_)
    ));
}

fn alive(pid_file: &Path) -> bool {
    let pid = fs::read_to_string(pid_file).unwrap();
    Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn fault_request(dir: &Dir, id: &str) -> Request {
    let location = dir.artifact(b"fault");
    let mut s = stage("fake.gain", &location);
    s.plugin.id = id.into();
    request(vec![s])
}

#[test]
fn hangs_time_out_and_the_helper_is_killed() {
    let dir = Dir::new("hang");
    let pid_file = dir.0.join("pid");
    let helpers = helpers_with(vec!["--pid-file".into(), pid_file.clone().into()]);
    let limits = Limits {
        block: Duration::from_millis(300),
        open: Duration::from_millis(1500),
        ..limits()
    };

    let request = fault_request(&dir, "fake.hang");
    let started = Instant::now();
    let error = offline::render(&request, &helpers, &limits, &Cancel::new()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout(Phase::Block(3)));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(!alive(&pid_file), "the hung helper must be killed");

    let request = fault_request(&dir, "fake.hang-open");
    let started = Instant::now();
    let error = offline::render(&request, &helpers, &limits, &Cancel::new()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout(Phase::Open));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert!(!alive(&pid_file));

    let request = fault_request(&dir, "fake.slow");
    let total = Limits {
        total: Duration::from_millis(400),
        ..limits
    };
    let error = offline::render(&request, &helpers, &total, &Cancel::new()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout(Phase::Total));
    assert!(!alive(&pid_file));
}

#[test]
fn cancellation_stops_a_running_render_promptly() {
    let dir = Dir::new("cancel");
    let pid_file = dir.0.join("pid");
    let helpers = helpers_with(vec!["--pid-file".into(), pid_file.clone().into()]);
    let request = fault_request(&dir, "fake.hang");
    let cancel = Cancel::new();
    let canceller = {
        let cancel = cancel.clone();
        let pid_file = pid_file.clone();
        thread::spawn(move || {
            while !pid_file.exists() {
                thread::sleep(Duration::from_millis(5));
            }
            thread::sleep(Duration::from_millis(300));
            let at = Instant::now();
            cancel.cancel();
            at
        })
    };
    let error = offline::render(&request, &helpers, &limits(), &cancel).unwrap_err();
    let returned = Instant::now();
    let cancelled_at = canceller.join().unwrap();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    let latency = returned.duration_since(cancelled_at);
    assert!(
        latency < Duration::from_millis(250),
        "cancel took {latency:?}"
    );
    assert!(!alive(&pid_file));

    let early = Cancel::new();
    early.cancel();
    let error = offline::render(&request, &Helpers::new(), &limits(), &early).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn identical_renders_are_repeatable() {
    let dir = Dir::new("repeat");
    let location = dir.artifact(b"echo");
    let request = request(vec![stage("fake.echo", &location)]);
    let a = render(&request).unwrap();
    let b = render(&request).unwrap();
    assert_eq!(a, b);
}
