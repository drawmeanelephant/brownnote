//! The plugin pedal workflow through the mounted app: real controls, the real
//! `brownnote-plugin-host clap` helper, the CLAP fault fixture and, when
//! `BROWNNOTE_CLAP_EFFECT` names it, the pinned Airwindows Consolidated
//! bundle. `make native-test` sets `BROWNNOTE_REQUIRE_CLAP=1`, so the fixture
//! tests cannot skip there.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::ui;
use brownnote_native::{
    edit::EditModel,
    export::{
        adapter,
        bundle::{BundleRequest, SystemWriter},
        manifest::{LossRecord, TimeOrigin, Timing},
    },
    kit::{Kit, encode_wav},
    performance::{
        Channel, LoopBars, NoteKey, OriginalTime, Pitch, SourceId, TakeId, Target, Tempo, Velocity,
    },
    project::{Document, PadId, store::Store},
    takes::{PassOptions, Takes},
};
use rune::{Env, Key, KeyEvent, Mods, Runtime};
use std::{
    fs,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

const PAD: usize = 4;
const RATE: u32 = 48_000;
const PASSTHROUGH: &str = "org.brownnote.fault.passthrough";

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
        _ => {
            skip(require, &format!("{name} is not set or missing"));
            None
        }
    }
}

/// The fault fixture, once the default helper configuration can find the
/// real helper (`BROWNNOTE_CLAP_HELPER`).
fn fixture() -> Option<PathBuf> {
    env_path("BROWNNOTE_CLAP_HELPER", "BROWNNOTE_REQUIRE_CLAP")?;
    env_path("BROWNNOTE_CLAP_FAULT_FIXTURE", "BROWNNOTE_REQUIRE_CLAP")
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "brownnote-plugin-pedals-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Five distinct decaying tones at 48 kHz, so renders are not trivial.
fn kit() -> Kit {
    Kit::from_encoded(std::array::from_fn(|index| {
        let frequency = 220.0 * (index + 1) as f32;
        let data: Vec<f32> = (0..RATE as usize / 2)
            .map(|frame| {
                let t = frame as f32 / RATE as f32;
                0.5 * (-6.0 * t).exp() * (std::f32::consts::TAU * frequency * t).sin()
            })
            .collect();
        Ok(encode_wav(&Sample {
            data: data.into(),
            rate: RATE,
        })
        .unwrap())
    }))
}

fn runtime(cache: &Path) -> Runtime<State> {
    let mut state = State::with_cache(kit(), true, true, cache.into(), "/nonexistent".into());
    state.pedals.auto_load = false;
    state.pedals.catalog =
        super::super::CatalogState::Unavailable("GuitarPedal bench not installed".into());
    let mut rt = Runtime::new(state, ui::view, (1120.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.0);
    rt
}

fn flush(rt: &mut Runtime<State>) {
    rt.frame(0.0);
    rt.frame(0.25);
}

fn tap(rt: &mut Runtime<State>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
    rt.pointer_down(x, y);
    rt.frame(0.0);
    rt.pointer_up(x, y);
    flush(rt);
}

fn press(rt: &mut Runtime<State>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(0.0);
}

fn typed(rt: &mut Runtime<State>, field: &str, value: &str) {
    tap(rt, field);
    press(rt, Key::Text(value.into()));
}

fn exists(rt: &Runtime<State>, key: &str) -> bool {
    rt.elements()
        .iter()
        .any(|element| element.key.as_deref() == Some(key))
}

fn open_plugins(rt: &mut Runtime<State>, pad: usize) {
    tap(rt, &format!("pad-{pad}"));
    tap(rt, "editor-tab-pedals");
    if !rt.state.pedals.plugins.shown {
        tap(rt, "pedal-mode-plugins");
    }
    assert!(exists(rt, "plugin-strip"));
}

/// Wait for every probe and render to settle, on real time.
fn settle(rt: &mut Runtime<State>) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        std::thread::sleep(Duration::from_millis(20));
        rt.frame(0.01);
        let plugins = &rt.state.pedals.plugins;
        let busy = plugins.probing()
            || plugins
                .pads
                .iter()
                .any(|pad| pad.status == Status::Rendering || pad.pending.is_some());
        if !busy {
            break;
        }
        assert!(Instant::now() < deadline, "plugin work did not settle");
    }
    flush(rt);
}

fn load(rt: &mut Runtime<State>, path: &Path, id: &str) {
    if !rt.state.pedals.plugins.adding {
        tap(rt, "plugin-add");
    }
    rt.state.pedals.plugins.path = TextEdit::default();
    rt.state.pedals.plugins.plugin_id = TextEdit::default();
    flush(rt);
    typed(rt, "plugin-path", path.to_str().unwrap());
    typed(rt, "plugin-id", id);
    tap(rt, "plugin-load");
    settle(rt);
}

fn audible(rt: &Runtime<State>, pad: usize) -> Arc<[f32]> {
    rt.state
        .assets
        .audible(pad_id(pad))
        .unwrap()
        .imported
        .sample
        .data
        .clone()
}

fn source(rt: &Runtime<State>, pad: usize) -> Arc<[f32]> {
    rt.state
        .assets
        .source(pad_id(pad))
        .unwrap()
        .imported
        .sample
        .data
        .clone()
}

fn playback_slot(rt: &Runtime<State>, pad: usize) -> usize {
    rt.state.assets.playback(pad_id(pad)).unwrap().0
}

fn retained_slot(rt: &Runtime<State>, pad: usize) -> Option<usize> {
    let id = rt
        .state
        .assets
        .pad(pad_id(pad))
        .assignment
        .as_ref()?
        .retained?;
    Some(rt.state.assets.asset(id)?.slot)
}

fn hit(rt: &mut Runtime<State>, pad: usize) {
    let before = rt.state.counts[pad];
    tap(rt, "editor");
    press(rt, Key::Text(PADS[pad].key.to_string()));
    rt.state.output.advance(0.001);
    rt.frame(0.01);
    assert!(
        rt.state.output.meters.counts()[pad] > 0 || rt.state.counts[pad] > before,
        "the pad key did not play"
    );
}

fn status(rt: &Runtime<State>) -> String {
    status_text(&rt.state, PAD)
}

fn save(rt: &mut Runtime<State>, root: &Path) {
    tap(rt, "projects-toggle");
    rt.state.projects.path = TextEdit::default();
    flush(rt);
    typed(rt, "projects-path", root.to_str().unwrap());
    tap(rt, "projects-save");
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.projects.busy() {
        assert!(Instant::now() < deadline, "save timed out");
        std::thread::sleep(Duration::from_millis(5));
        rt.frame(0.01);
    }
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    tap(rt, "projects-toggle");
}

fn open(rt: &mut Runtime<State>, root: &Path) {
    tap(rt, "projects-toggle");
    typed(rt, "projects-path", root.to_str().unwrap());
    tap(rt, "projects-open");
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.projects.busy() {
        assert!(Instant::now() < deadline, "open timed out");
        std::thread::sleep(Duration::from_millis(5));
        rt.frame(0.01);
    }
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
    tap(rt, "projects-toggle");
}

fn saved_document(root: &Path) -> Document {
    Store::new(root.to_path_buf())
        .inspect_committed()
        .unwrap()
        .unwrap()
        .document
}

#[test]
fn typing_in_plugin_fields_never_plays_pads() {
    let root = Root::new();
    let mut rt = runtime(&root.0.join("cache"));
    open_plugins(&mut rt, PAD);
    tap(&mut rt, "plugin-add");
    typed(&mut rt, "plugin-id", "qwerasdf1234");
    assert_eq!(rt.state.pedals.plugins.plugin_id.text, "qwerasdf1234");
    rt.state.output.advance(0.01);
    assert_eq!(rt.state.output.meters.counts(), [0; 16]);
    // Without a path nothing loads, and the message says why.
    tap(&mut rt, "plugin-load");
    assert!(
        rt.state
            .pedals
            .plugins
            .message
            .as_deref()
            .unwrap()
            .contains("absolute path")
    );
    assert!(rt.state.pedals.plugins.pads[PAD].stages.is_empty());
}

#[test]
fn fixture_renders_through_the_app_and_failures_keep_the_previous_sound() {
    let Some(fixture) = fixture() else {
        return;
    };
    let root = Root::new();
    let mut rt = runtime(&root.0.join("cache"));
    open_plugins(&mut rt, PAD);
    let dry_slot = playback_slot(&rt, PAD);

    // A plugin that cannot load leaves the chain empty and says exactly why.
    load(&mut rt, &fixture, "org.brownnote.fault.init-fails");
    assert!(rt.state.pedals.plugins.pads[PAD].stages.is_empty());
    let message = rt.state.pedals.plugins.message.clone().unwrap();
    assert!(message.contains("init-failed"), "{message}");

    load(&mut rt, &fixture, PASSTHROUGH);
    assert!(status(&rt).starts_with("READY"), "{}", status(&rt));
    let first = audible(&rt, PAD);
    // Stereo passthrough of a mono source, downmixed: the exact source.
    assert_eq!(&*first, &*source(&rt, PAD));
    // Identical encoded bytes serve as both source and render, so the slot can
    // stay: what changed is that the pad now plays its retained assignment.
    let first_slot = playback_slot(&rt, PAD);
    assert_eq!(first_slot, dry_slot);
    assert_eq!(
        Some(first_slot),
        retained_slot(&rt, PAD),
        "the next hit plays the retained render"
    );
    assert!(is_current(&rt.state, PAD));

    // A voice already playing keeps its slot while a newer render lands.
    hit(&mut rt, PAD);
    load(&mut rt, &fixture, PASSTHROUGH);
    assert_eq!(rt.state.pedals.plugins.pads[PAD].stages.len(), 2);
    assert!(
        !rt.state.assets.slots().is_empty(first_slot),
        "the playing voice lost its sample"
    );
    let second = audible(&rt, PAD);

    // Crash, NaN: the render fails, the pad keeps playing the last good one.
    for (id, expected) in [
        ("org.brownnote.fault.crash", "the plugin helper stopped"),
        (
            "org.brownnote.fault.nonfinite",
            "non-finite sample at frame 4096",
        ),
    ] {
        load(&mut rt, &fixture, id);
        let text = status(&rt);
        assert!(text.starts_with("ERROR"), "{text}");
        assert!(text.contains(expected), "{text}");
        assert!(text.contains("PREVIOUS SOUND KEPT"), "{text}");
        assert!(Arc::ptr_eq(&audible(&rt, PAD), &second));
        assert_eq!(playback_slot(&rt, PAD), first_slot);
        assert!(!is_current(&rt.state, PAD), "a failed edit is not saved");
        assert!(exists(&rt, "plugin-revert"));
        // Removing the failed stage returns to the landed recipe without a render.
        tap(&mut rt, "plugin-2-remove");
        assert!(status(&rt).starts_with("READY"), "{}", status(&rt));
        assert!(is_current(&rt.state, PAD));
        hit(&mut rt, PAD);
    }

    // Removing every stage puts the dry source back.
    tap(&mut rt, "plugin-1-remove");
    settle(&mut rt);
    tap(&mut rt, "plugin-0-remove");
    settle(&mut rt);
    assert_eq!(status(&rt), "BASE SAMPLE");
    assert!(Arc::ptr_eq(&audible(&rt, PAD), &source(&rt, PAD)));
}

#[test]
fn a_hung_plugin_is_cancelled_or_timed_out_without_touching_the_sound() {
    let Some(fixture) = fixture() else {
        return;
    };
    let root = Root::new();
    let mut rt = runtime(&root.0.join("cache"));
    type Finished = Arc<Mutex<Vec<(Instant, Option<ErrorKind>)>>>;
    let finished: Finished = Arc::default();
    let record = finished.clone();
    rt.state.pedals.plugins.render_fn = Arc::new(move |request, helpers, limits, cancel| {
        let result = offline::render(request, helpers, limits, cancel);
        record.lock().unwrap().push((
            Instant::now(),
            result.as_ref().err().map(|e| e.kind.clone()),
        ));
        result
    });
    open_plugins(&mut rt, PAD);
    load(&mut rt, &fixture, PASSTHROUGH);
    let kept = audible(&rt, PAD);
    let slot = playback_slot(&rt, PAD);
    finished.lock().unwrap().clear();

    // The fixture hangs in its ninth block; cancel it from the strip.
    load_without_settling(&mut rt, &fixture, "org.brownnote.fault.hang");
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(900) {
        std::thread::sleep(Duration::from_millis(10));
        rt.frame(0.01);
    }
    assert_eq!(rt.state.pedals.plugins.pads[PAD].status, Status::Rendering);
    assert!(finished.lock().unwrap().is_empty(), "the hang ended early");
    let cancelled_at = Instant::now();
    tap(&mut rt, "plugin-cancel");
    let deadline = Instant::now() + Duration::from_secs(5);
    while finished.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the helper was not stopped");
        std::thread::sleep(Duration::from_millis(2));
    }
    let (returned, kind) = finished.lock().unwrap()[0].clone();
    let latency = returned.duration_since(cancelled_at);
    println!("cancel stopped the hung helper after {latency:?}");
    assert!(latency < Duration::from_millis(500), "{latency:?}");
    assert_eq!(kind, Some(ErrorKind::Cancelled));
    settle(&mut rt);
    assert!(status(&rt).starts_with("READY"), "{}", status(&rt));
    assert!(Arc::ptr_eq(&audible(&rt, PAD), &kept));
    assert_eq!(playback_slot(&rt, PAD), slot);

    // The same hang under a short block limit times out instead.
    tap(&mut rt, "plugin-revert");
    assert!(is_current(&rt.state, PAD));
    rt.state.pedals.plugins.limits.block = Duration::from_millis(300);
    load(&mut rt, &fixture, "org.brownnote.fault.hang");
    let text = status(&rt);
    assert!(text.contains("timed out during Block(8)"), "{text}");
    assert!(Arc::ptr_eq(&audible(&rt, PAD), &kept));
    tap(&mut rt, "plugin-revert");
    assert_eq!(rt.state.pedals.plugins.pads[PAD].stages.len(), 1);
    assert!(is_current(&rt.state, PAD));
}

fn load_without_settling(rt: &mut Runtime<State>, path: &Path, id: &str) {
    if !rt.state.pedals.plugins.adding {
        tap(rt, "plugin-add");
    }
    rt.state.pedals.plugins.path = TextEdit::default();
    rt.state.pedals.plugins.plugin_id = TextEdit::default();
    flush(rt);
    typed(rt, "plugin-path", path.to_str().unwrap());
    typed(rt, "plugin-id", id);
    tap(rt, "plugin-load");
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.pedals.plugins.probing() {
        assert!(Instant::now() < deadline, "probe timed out");
        std::thread::sleep(Duration::from_millis(5));
        rt.frame(0.01);
    }
    flush(rt);
    assert_eq!(rt.state.pedals.plugins.pads[PAD].status, Status::Rendering);
}

fn one_hit_takes() -> (Takes, EditModel) {
    let mut takes = Takes::new(LoopBars::try_from(1).unwrap(), TakeId(41));
    takes.set_tempo(Tempo::try_from(120_000).unwrap()).unwrap();
    let take = takes.arm(PassOptions::default()).unwrap();
    takes.start(take).unwrap();
    let key = NoteKey {
        source: SourceId(1),
        channel: Channel::try_from(0).unwrap(),
        pitch: Pitch::try_from(40).unwrap(),
    };
    takes
        .note_on(
            take,
            Target::Pad(PadId::try_from(PAD as u8).unwrap()),
            key,
            Velocity::try_from(100).unwrap(),
            OriginalTime::from_nanos(100_000_000),
        )
        .unwrap();
    takes
        .note_off(take, key, OriginalTime::from_nanos(200_000_000))
        .unwrap();
    takes.finish(take).unwrap();
    (takes, EditModel::new())
}

#[test]
fn saved_plugin_sound_survives_cache_plugin_removal_relocation_and_exports_honestly() {
    let Some(fixture) = fixture() else {
        return;
    };
    let root = Root::new();
    let cache = root.0.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let plugins = root.0.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    let installed = plugins.join("Fixture.clap");
    fs::copy(&fixture, &installed).unwrap();

    let mut rt = runtime(&cache);
    open_plugins(&mut rt, PAD);
    load(&mut rt, &installed, PASSTHROUGH);
    let heard: Vec<f32> = audible(&rt, PAD).to_vec();
    // A single project snapshot must preserve every feature's data.
    let (takes, mut edits) = one_hit_takes();
    let event = takes.take(TakeId(41)).unwrap().notes()[0].on().id;
    edits.nudge(&takes, event, 10_000_000).unwrap();
    rt.state.takes = takes;
    rt.state.edit.model = edits;
    ui::midi::fixture(
        &rt.state.midi,
        SourceId(7),
        r#"{"version":1,"mappings":[
            {"type":"note","channel":0,"pitch":60,"target":{"type":"held","instrument":"1"}}
        ]}"#,
        &[],
    );
    let font = root.0.join("mechanics.sf2");
    fs::write(
        &font,
        include_bytes!("../../../docs/spikes/sampled-instrument/fixtures/mechanics.sf2"),
    )
    .unwrap();
    rt.state.instrument.path = TextEdit::new(font.to_string_lossy());
    ui::instrument::load(&mut rt.state);
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.instrument.busy() {
        assert!(Instant::now() < deadline, "instrument load timed out");
        std::thread::sleep(Duration::from_millis(5));
        rt.frame(0.01);
    }
    assert!(
        rt.state.instrument.error.is_none(),
        "{:?}",
        rt.state.instrument.error
    );
    let saved_at = root.0.join("A");
    save(&mut rt, &saved_at);
    let document = saved_document(&saved_at);
    assert!(document.midi_mappings.is_some());
    assert!(document.takes.is_some());
    assert!(document.edits.is_some());
    assert!(document.instrument.is_some());
    let chain = document.pads[PAD].plugins.clone().unwrap();
    assert_eq!(chain.stages[0].plugin.id, PASSTHROUGH);
    assert_eq!(chain.stages[0].location_hint.as_deref(), installed.to_str());
    let sample = document.pads[PAD].sample.clone().unwrap();
    let retained = sample.retained.unwrap();

    // Remove the plugin and the cache, and move the project.
    fs::remove_dir_all(&plugins).unwrap();
    fs::remove_dir_all(&cache).unwrap();
    let moved = root.0.join("moved B");
    fs::rename(&saved_at, &moved).unwrap();

    let mut rt = runtime(&root.0.join("cache2"));
    open(&mut rt, &moved);
    assert_eq!(rt.state.takes.take(TakeId(41)).unwrap().notes().len(), 1);
    assert_eq!(
        ui::midi::mappings(&rt.state.midi).unwrap().entries().len(),
        1
    );
    open_plugins(&mut rt, PAD);
    assert_eq!(
        &*audible(&rt, PAD),
        &heard[..],
        "the reopened pad plays the saved render"
    );
    let text = status(&rt);
    assert!(
        text.starts_with("READY") && text.contains("PLUGIN MISSING"),
        "{text}"
    );
    assert!(exists(&rt, "plugin-problem"));
    assert!(exists(&rt, "pad-4-plug"));
    hit(&mut rt, PAD);
    assert!(
        is_current(&rt.state, PAD),
        "a missing plugin does not block saving"
    );

    // An edit cannot render without the plugin and keeps the saved sound.
    load(&mut rt, &fixture, PASSTHROUGH);
    let text = status(&rt);
    assert!(text.contains("is not installed here"), "{text}");
    assert_eq!(&*audible(&rt, PAD), &heard[..]);
    tap(&mut rt, "plugin-revert");
    assert!(is_current(&rt.state, PAD));

    // Relinking to an identical build needs no render.
    let before = audible(&rt, PAD);
    tap(&mut rt, "plugin-0");
    rt.state.pedals.plugins.path = TextEdit::default();
    flush(&mut rt);
    typed(&mut rt, "plugin-path", fixture.to_str().unwrap());
    tap(&mut rt, "plugin-relink");
    settle(&mut rt);
    assert!(
        rt.state
            .pedals
            .plugins
            .message
            .as_deref()
            .unwrap()
            .starts_with("Relinked"),
        "{:?}",
        rt.state.pedals.plugins.message
    );
    assert!(!status(&rt).contains("PLUGIN MISSING"), "{}", status(&rt));
    assert!(Arc::ptr_eq(&audible(&rt, PAD), &before));
    assert!(exists(&rt, "plugin-controls"));

    let resaved = root.0.join("C");
    save(&mut rt, &resaved);
    let again = saved_document(&resaved);
    assert_eq!(again.midi_mappings, document.midi_mappings);
    assert_eq!(again.takes, document.takes);
    assert_eq!(again.edits, document.edits);
    assert_eq!(again.instrument, document.instrument);
    let chain_again = again.pads[PAD].plugins.clone().unwrap();
    assert_eq!(chain_again.render, chain.render);
    assert_eq!(chain_again.stages[0].plugin, chain.stages[0].plugin);
    assert_eq!(
        chain_again.stages[0].location_hint.as_deref(),
        fixture.to_str()
    );
    assert_eq!(again.pads[PAD].sample, Some(sample.clone()));

    // #109 export: the dry source and the baked render, named honestly.
    let (takes, edits) = one_hit_takes();
    let bundle = root.0.join("bundle");
    let receipt = adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &BundleRequest {
            source_root: resaved.clone(),
            destination: bundle.clone(),
            origin: TimeOrigin { nanos: 0 },
            timing: Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        },
        &takes,
        &edits,
        &Default::default(),
    )
    .unwrap();
    let baked: Vec<&String> = receipt
        .manifest
        .losses
        .iter()
        .filter_map(|loss| match loss {
            LossRecord::BakedProcessing { asset, detail, .. }
                if asset.to_string() == retained.to_string() =>
            {
                Some(detail)
            }
            _ => None,
        })
        .collect();
    assert_eq!(baked.len(), 1, "{:?}", receipt.manifest.losses);
    assert!(baked[0].contains("CLAP plugin"), "{}", baked[0]);
    assert!(baked[0].contains(PASSTHROUGH), "{}", baked[0]);
    assert!(
        baked[0].contains("downmixed to the mono pad voice"),
        "{}",
        baked[0]
    );
    for id in [sample.source, retained] {
        let exported = fs::read(bundle.join(id.relative_path())).unwrap();
        let project = fs::read(resaved.join(id.relative_path())).unwrap();
        assert_eq!(exported, project, "{id} is exported byte for byte");
    }
}

// Airwindows Consolidated 1.2026.313, pinned by #96.
const EFFECT_ID: &str = "com.airwindows.consolidated";
const EFFECT_EXECUTABLE_SHA256: &str =
    "92152f95a7053ecd65c4e5f356e719c6b82a7dfe4083bc12848f5c755077ad4d";

#[test]
fn pinned_airwindows_effect_renders_edits_and_reopens_through_the_app() {
    let Some(_) = fixture() else {
        return;
    };
    let Some(bundle) = env_path("BROWNNOTE_CLAP_EFFECT", "BROWNNOTE_REQUIRE_CLAP_EFFECT") else {
        return;
    };
    let root = Root::new();
    let mut rt = runtime(&root.0.join("cache"));
    open_plugins(&mut rt, PAD);
    let started = Instant::now();
    load(&mut rt, &bundle, EFFECT_ID);
    println!("pinned effect load + render: {:?}", started.elapsed());
    let text = status(&rt);
    assert!(text.starts_with("READY"), "{text}");
    let stage = rt.state.pedals.plugins.pads[PAD].stages[0].clone();
    assert_eq!(
        stage.record.plugin.artifact.to_string(),
        EFFECT_EXECUTABLE_SHA256
    );
    let Status::Ready(report) = rt.state.pedals.plugins.pads[PAD].status.clone() else {
        unreachable!()
    };
    let source_frames = source(&rt, PAD).len();
    let wet = audible(&rt, PAD);
    println!(
        "pinned effect: {} source frames -> {} retained frames, tail {:?} s, capped {}",
        source_frames,
        wet.len(),
        report.tail_seconds,
        report.tail_capped
    );
    assert!(report.tail_seconds.unwrap() > 4.0, "{report:?}");
    assert!(!report.tail_capped);
    assert!(wet.len() > source_frames + 4 * RATE as usize);
    assert!(wet.iter().all(|s| s.is_finite()));
    let difference: f32 = wet
        .iter()
        .zip(source(&rt, PAD).iter())
        .map(|(a, b)| (a - b).abs())
        .sum();
    assert!(
        difference > 1.0,
        "the effect changed nothing ({difference})"
    );

    // A generic control: Dry/Wet, set from its slider.
    let dry_wet = stage
        .controls
        .unwrap()
        .into_iter()
        .find(|param| param.name == "Dry/Wet")
        .unwrap();
    tap(&mut rt, &format!("plugin-param-{}", dry_wet.id));
    settle(&mut rt);
    let edited = audible(&rt, PAD);
    let value = rt.state.pedals.plugins.pads[PAD].stages[0].record.params[0]
        .value
        .0;
    println!("Dry/Wet {value} from the slider");
    assert!((value - dry_wet.default).abs() > 0.1, "{value}");
    assert!(status(&rt).starts_with("READY"), "{}", status(&rt));
    assert_ne!(&edited[..source_frames], &wet[..source_frames]);

    // A hit while a different render runs keeps the previous slot alive: the
    // new bytes get a new slot and the playing voice is never cut off.
    tap(&mut rt, &format!("plugin-param-{}", dry_wet.id));
    press(&mut rt, Key::Home);
    hit(&mut rt, PAD);
    let playing_slot = playback_slot(&rt, PAD);
    settle(&mut rt);
    assert!(status(&rt).starts_with("READY"), "{}", status(&rt));
    assert_ne!(playback_slot(&rt, PAD), playing_slot);
    assert!(
        !rt.state.assets.slots().is_empty(playing_slot),
        "the playing voice lost its sample"
    );
    let edited = audible(&rt, PAD);

    // Save and reopen: the same samples, and the recipe still matches.
    let saved_at = root.0.join("A");
    save(&mut rt, &saved_at);
    let mut rt = runtime(&root.0.join("cache2"));
    open(&mut rt, &saved_at);
    open_plugins(&mut rt, PAD);
    assert_eq!(&*audible(&rt, PAD), &*edited);
    settle(&mut rt);
    let problems: Vec<_> = rt.state.pedals.plugins.pads[PAD]
        .stages
        .iter()
        .filter_map(|stage| stage.problem.clone())
        .collect();
    assert!(problems.is_empty(), "{problems:?}");
    assert!(exists(&rt, &format!("plugin-param-{}", dry_wet.id)));
}

// CAPS 0.9.26 Plate, pinned by #97 and provisioned by caps/provision.py.
const PLATE_ID: &str = "1779:Plate";

#[test]
fn pinned_ladspa_plate_renders_auditions_and_exports_through_the_app() {
    let Some(_) = env_path("BROWNNOTE_LADSPA_HELPER", "BROWNNOTE_REQUIRE_LADSPA_PLATE") else {
        return;
    };
    let Some(built) = env_path("BROWNNOTE_LADSPA_PLATE", "BROWNNOTE_REQUIRE_LADSPA_PLATE") else {
        return;
    };
    let root = Root::new();
    let cache = root.0.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let plugins = root.0.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    let installed = plugins.join("caps.so");
    fs::copy(&built, &installed).unwrap();

    let mut rt = runtime(&cache);
    open_plugins(&mut rt, PAD);
    load(&mut rt, &installed, PLATE_ID);
    let text = status(&rt);
    assert!(text.starts_with("READY"), "{text}");
    assert!(text.contains("TAIL"), "{text}");
    let stage = rt.state.pedals.plugins.pads[PAD].stages[0].clone();
    assert_eq!(stage.record.plugin.backend, Backend::Ladspa);
    assert_eq!(stage.record.plugin.id, PLATE_ID);
    let artifact = stage.record.plugin.artifact.to_string();
    let blend = stage
        .controls
        .clone()
        .unwrap()
        .into_iter()
        .find(|param| param.name == "blend")
        .unwrap();

    let dry = source(&rt, PAD);
    let wet = audible(&rt, PAD);
    assert!(wet.iter().all(|sample| sample.is_finite()));
    assert!(wet.len() > dry.len(), "the plate tail is retained");
    let difference: f32 = wet.iter().zip(dry.iter()).map(|(a, b)| (a - b).abs()).sum();
    assert!(difference > 1.0, "the plate changed nothing ({difference})");

    // A hit while an edit renders keeps the voice already playing on its slot.
    hit(&mut rt, PAD);
    let playing_slot = playback_slot(&rt, PAD);
    tap(&mut rt, &format!("plugin-param-{}", blend.id));
    hit(&mut rt, PAD);
    settle(&mut rt);
    assert!(status(&rt).starts_with("READY"), "{}", status(&rt));
    assert_ne!(playback_slot(&rt, PAD), playing_slot);
    assert!(
        !rt.state.assets.slots().is_empty(playing_slot),
        "the playing voice lost its sample"
    );
    let edited = audible(&rt, PAD);
    assert_ne!(&edited[..dry.len()], &wet[..dry.len()]);

    // Save, then reopen with the library and cache gone: the retained audio
    // still plays and the pad names the missing LADSPA plugin.
    let saved_at = root.0.join("A");
    save(&mut rt, &saved_at);
    let document = saved_document(&saved_at);
    let chain = document.pads[PAD].plugins.clone().unwrap();
    assert_eq!(chain.stages[0].plugin.backend, Backend::Ladspa);
    assert_eq!(chain.stages[0].plugin.id, PLATE_ID);
    assert_eq!(chain.stages[0].location_hint.as_deref(), installed.to_str());
    let sample = document.pads[PAD].sample.clone().unwrap();
    let retained = sample.retained.unwrap();

    fs::remove_dir_all(&plugins).unwrap();
    fs::remove_dir_all(&cache).unwrap();
    let moved = root.0.join("moved B");
    fs::rename(&saved_at, &moved).unwrap();

    let mut rt = runtime(&root.0.join("cache2"));
    open(&mut rt, &moved);
    open_plugins(&mut rt, PAD);
    assert_eq!(
        &*audible(&rt, PAD),
        &*edited,
        "the reopened pad plays the saved render"
    );
    let text = status(&rt);
    assert!(
        text.starts_with("READY") && text.contains("PLUGIN MISSING"),
        "{text}"
    );
    let problem = rt.state.pedals.plugins.pads[PAD].stages[0]
        .problem
        .clone()
        .unwrap();
    assert!(problem.contains("LADSPA 1779:Plate"), "{problem}");
    assert!(
        rt.elements().iter().any(|element| {
            element.key.as_deref() == Some("plugin-problem")
                && element
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("LADSPA 1779:Plate"))
        }),
        "the status does not name the missing LADSPA plugin"
    );
    hit(&mut rt, PAD);
    assert!(
        is_current(&rt.state, PAD),
        "a missing plugin does not block saving"
    );

    // Relinking to an identical build needs no render.
    let relinked = root.0.join("plugins2");
    fs::create_dir_all(&relinked).unwrap();
    let library = relinked.join("caps.so");
    fs::copy(&built, &library).unwrap();
    let before = audible(&rt, PAD);
    tap(&mut rt, "plugin-0");
    rt.state.pedals.plugins.path = TextEdit::default();
    flush(&mut rt);
    typed(&mut rt, "plugin-path", library.to_str().unwrap());
    tap(&mut rt, "plugin-relink");
    settle(&mut rt);
    assert!(
        rt.state
            .pedals
            .plugins
            .message
            .as_deref()
            .unwrap()
            .starts_with("Relinked"),
        "{:?}",
        rt.state.pedals.plugins.message
    );
    assert!(!status(&rt).contains("PLUGIN MISSING"), "{}", status(&rt));
    assert!(Arc::ptr_eq(&audible(&rt, PAD), &before));
    assert!(exists(&rt, "plugin-controls"));

    // #109 export: the dry source is byte-identical and the baked stage is
    // named by backend, ID and artifact.
    let resaved = root.0.join("C");
    save(&mut rt, &resaved);
    let (takes, edits) = one_hit_takes();
    let bundle = root.0.join("bundle");
    let receipt = adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &BundleRequest {
            source_root: resaved.clone(),
            destination: bundle.clone(),
            origin: TimeOrigin { nanos: 0 },
            timing: Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        },
        &takes,
        &edits,
        &Default::default(),
    )
    .unwrap();
    let baked: Vec<&String> = receipt
        .manifest
        .losses
        .iter()
        .filter_map(|loss| match loss {
            LossRecord::BakedProcessing { asset, detail, .. }
                if asset.to_string() == retained.to_string() =>
            {
                Some(detail)
            }
            _ => None,
        })
        .collect();
    assert_eq!(baked.len(), 1, "{:?}", receipt.manifest.losses);
    assert!(baked[0].contains("LADSPA plugin"), "{}", baked[0]);
    assert!(baked[0].contains(PLATE_ID), "{}", baked[0]);
    assert!(baked[0].contains(&artifact[..12]), "{}", baked[0]);
    assert!(
        baked[0].contains("downmixed to the mono pad voice"),
        "{}",
        baked[0]
    );
    for id in [sample.source, retained] {
        let exported = fs::read(bundle.join(id.relative_path())).unwrap();
        let project = fs::read(resaved.join(id.relative_path())).unwrap();
        assert_eq!(exported, project, "{id} is exported byte for byte");
    }
}
