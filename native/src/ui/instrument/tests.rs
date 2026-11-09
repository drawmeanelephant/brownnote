#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The sampled instrument through the real app entry points: the panel, the
//! MIDI fixture input, the transport, project save/open and the export
//! adapter. The MIT mechanics fixture proves mechanics only, not sound quality.
use super::*;
use crate::ui::{self, projects, transport};
use brownnote_native::performance::SourceId;
use brownnote_native::{
    export::{
        adapter,
        bundle::{BundleRequest, SystemWriter},
        manifest::{AudioRole, LossRecord, TimeOrigin, Timing},
    },
    project::{
        MediaKind,
        store::{MediaSource, Store},
    },
};
use rune::{Env, Runtime};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

const FIXTURE: &[u8] =
    include_bytes!("../../../docs/spikes/sampled-instrument/fixtures/mechanics.sf2");

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "brownnote-instrument-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runtime() -> Runtime<State> {
    let kit = crate::ui_test_support::kit(0.5, 44_100, 44_100);
    let mut state = State::new(kit, true, true);
    state.pedals.auto_load = false;
    let mut rt = Runtime::new(state, ui::view, (1120.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.0);
    rt
}

fn run(rt: &mut Runtime<State>, seconds: f64) {
    for _ in 0..(seconds / 0.01).round() as usize {
        rt.frame(0.01);
    }
}

/// Real worker threads: bounded by the wall clock while app time runs on.
fn wait(rt: &mut Runtime<State>, what: &str, done: impl Fn(&State) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done(&rt.state) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out: {what}: {}",
            status(&rt.state)
        );
        rt.frame(0.01);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn tap(rt: &mut Runtime<State>, target: &str) {
    let (x, y) = rt
        .find(target)
        .unwrap_or_else(|| panic!("missing {target}"));
    rt.pointer_down(x, y);
    rt.frame(0.0);
    rt.pointer_up(x, y);
    rt.frame(0.0);
}

/// Open or close the panel and let its transition settle, as a person would.
fn toggle(rt: &mut Runtime<State>) {
    tap(rt, "instrument-toggle");
    run(rt, 0.5);
}

fn show(rt: &mut Runtime<State>) {
    if rt.find("instrument-panel").is_none() {
        toggle(rt);
    }
}

fn label(rt: &Runtime<State>, target: &str) -> String {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(target))
        .unwrap_or_else(|| panic!("missing {target}"))
        .label
        .unwrap()
}

/// Loudest UI meter reading over `seconds` of app time.
fn loudest(rt: &mut Runtime<State>, seconds: f64) -> f32 {
    let mut peak = 0.0_f32;
    for _ in 0..(seconds / 0.01).round() as usize {
        rt.frame(0.01);
        peak = peak.max(rt.state.peak);
    }
    peak
}

fn load_fixture(rt: &mut Runtime<State>, root: &Root) {
    let path = root.0.join("mechanics.sf2");
    fs::write(&path, FIXTURE).unwrap();
    toggle(rt);
    assert!(rt.find("instrument-panel").is_some());
    assert!(label(rt, "instrument-status").starts_with("NO INSTRUMENT"));
    rt.state.instrument.path = TextEdit::new(path.to_string_lossy());
    tap(rt, "instrument-load");
    wait(rt, "instrument load", |s| status(s).contains("PLAYING AT"));
    let line = label(rt, "instrument-status");
    assert!(
        line.starts_with("mechanics · PRESET 000:000 · PLAYING AT"),
        "{line}"
    );
    assert!(rt.state.instrument.error.is_none());
}

const HELD: &str = r#"{"version":1,"mappings":[
    {"type":"note","channel":0,"pitch":60,"target":{"type":"held","instrument":"1"}}
]}"#;

#[test]
fn panel_loads_sf2_auditions_and_live_midi_notes_sound_then_release_fifo() {
    let root = Root::new();
    let mut rt = runtime();
    // A relative path is refused without touching the disk.
    toggle(&mut rt);
    rt.state.instrument.path = TextEdit::new("mechanics.sf2");
    tap(&mut rt, "instrument-load");
    assert!(label(&rt, "instrument-error").contains("absolute path"));
    // A file that is not an SF2 is reported and nothing mounts.
    let junk = root.0.join("junk.sf2");
    fs::write(&junk, b"RIFF\x04\x00\x00\x00sfbk").unwrap();
    rt.state.instrument.path = TextEdit::new(junk.to_string_lossy());
    tap(&mut rt, "instrument-load");
    wait(&mut rt, "junk refusal", |s| !s.instrument.busy());
    assert!(rt.state.instrument.error.is_some());
    assert!(rt.state.instrument.font.is_none());
    toggle(&mut rt);
    assert!(rt.find("instrument-panel").is_none());

    load_fixture(&mut rt, &root);
    assert_eq!(loudest(&mut rt, 0.2), 0.0, "loading alone is silent");
    tap(&mut rt, "instrument-audition");
    assert!(loudest(&mut rt, 0.3) > 0.01, "audition sounds");
    run(&mut rt, 1.0);
    assert_eq!(loudest(&mut rt, 0.2), 0.0, "the audition releases itself");

    // Two overlapping same-key presses, then two releases: FIFO identity
    // (D14) must end both, or a stuck voice keeps the meter up.
    let midi = rt.state.midi.clone();
    ui::midi::fixture(
        &midi,
        SourceId(77),
        HELD,
        &[
            (0, &[0x90, 60, 100]),
            (100_000, &[0x90, 60, 90]),
            (500_000, &[0x80, 60, 0]),
        ],
    );
    let sounding = loudest(&mut rt, 0.3);
    assert!(sounding > 0.01, "live held notes sound: {sounding}");
    assert_eq!(rt.state.instrument.live, 2);
    assert!(label(&rt, "instrument-status").contains("2 LIVE"));
    ui::midi::request_disconnect(&midi);
    rt.frame(0.0);
    run(&mut rt, 1.0);
    assert_eq!(
        loudest(&mut rt, 0.2),
        0.0,
        "disconnect releases the held note"
    );

    // Panic silences a held note and the next one still plays.
    ui::midi::fixture(&midi, SourceId(78), HELD, &[(0, &[0x90, 60, 110])]);
    assert!(loudest(&mut rt, 0.2) > 0.01);
    tap(&mut rt, "panic");
    run(&mut rt, 0.5);
    assert_eq!(loudest(&mut rt, 0.2), 0.0, "panic silences the instrument");
    ui::midi::request_disconnect(&midi);
    rt.frame(0.0);
    show(&mut rt);
    tap(&mut rt, "instrument-audition");
    assert!(
        loudest(&mut rt, 0.3) > 0.01,
        "the instrument survives panic"
    );
}

fn request(project: &Path, destination: &Path, s: &State) -> BundleRequest {
    BundleRequest {
        source_root: project.to_path_buf(),
        destination: destination.to_path_buf(),
        origin: TimeOrigin {
            nanos: 1_000_000_000,
        },
        timing: Timing {
            tempo_milli_bpm: s.takes.tempo().milli_bpm(),
            sample_rate_hz: 48_000,
        },
    }
}

fn save(rt: &mut Runtime<State>, project: &Path) {
    rt.state.projects.path = TextEdit::new(project.to_string_lossy());
    projects::save(&mut rt.state);
    wait(rt, "save", |s| !s.projects.busy());
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
}

fn open(rt: &mut Runtime<State>, project: &Path) {
    rt.state.projects.path = TextEdit::new(project.to_string_lossy());
    projects::open(&mut rt.state);
    wait(rt, "open", |s| !s.projects.busy());
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
}

#[test]
fn recorded_held_take_replays_saves_frozen_audio_reopens_and_exports() {
    let root = Root::new();
    let mut rt = runtime();
    load_fixture(&mut rt, &root);
    // Two held notes (0→500 ms, 100→900 ms) and two pad hits in one pass.
    transport::scene_midi(&mut rt.state);
    rt.frame(0.0);
    wait(&mut rt, "pass commit", |s| !s.takes.has_pass());
    let take = rt.state.takes.take(TakeId(1)).unwrap();
    assert_eq!(take.notes().len(), 4);
    let line = label(&rt, "transport-status");
    assert!(!line.contains("NOT SOUNDED"), "{line}");

    // Replay: the next cycle schedules the held notes on and off.
    let before = rt.state.output.schedule_counters().dispatched;
    run(&mut rt, 2.5);
    let dispatched = rt.state.output.schedule_counters().dispatched - before;
    assert!(
        dispatched >= 6,
        "2 held on + 2 held off + 2 pads: {dispatched}"
    );
    assert_eq!(rt.state.transport.counters.schedule_lost, 0);
    transport::stop(&mut rt.state, "Stopped");
    run(&mut rt, 1.5);

    // Saving freezes each distinct held note to project audio.
    let project = root.0.join("project");
    save(&mut rt, &project);
    let saved = Store::new(project.clone())
        .inspect_committed()
        .unwrap()
        .unwrap()
        .document;
    let instrument = saved.instrument.clone().expect("instrument saved");
    assert_eq!(instrument.renders.len(), 2);
    assert_eq!(instrument.source, AssetRef::of(FIXTURE).id);
    let kind = |id| {
        saved
            .assets
            .iter()
            .find(|a| a.reference.id == id)
            .map(|a| a.kind)
    };
    assert_eq!(kind(instrument.source), Some(MediaKind::InstrumentSource));
    for render in &instrument.renders {
        assert_eq!(kind(render.asset), Some(MediaKind::InstrumentRender));
        assert!(project.join(render.asset.relative_path()).is_file());
    }
    assert_eq!(rt.state.instrument.renders.lock().unwrap().len(), 2);
    // A second save reuses the renders instead of rendering again.
    save(&mut rt, &project);
    let again = Store::new(project.clone())
        .inspect_committed()
        .unwrap()
        .unwrap()
        .document;
    assert_eq!(again.instrument, Some(instrument.clone()));

    // Export (#109): the notes plus each note's retained audio, with no
    // "editable MIDI only" loss for the instrument.
    let bundle = root.0.join("bundle");
    let receipt = adapter::write_saved_project_bundle(
        &mut SystemWriter,
        &request(&project, &bundle, &rt.state),
        &rt.state.takes,
        &rt.state.edit.model,
        &Default::default(),
    )
    .unwrap();
    let held: Vec<_> = take_notes(&rt.state)
        .into_iter()
        .filter(|(target, _)| *target == Target::Instrument(INSTRUMENT))
        .map(|(_, id)| id)
        .collect();
    assert_eq!(held.len(), 2);
    for id in &held {
        let event = brownnote_native::export::manifest::EventId {
            take: id.take.0,
            sequence: id.sequence,
        };
        assert!(
            receipt
                .manifest
                .audio
                .iter()
                .any(|a| a.event == Some(event) && a.role == AudioRole::HitWet && a.frames > 0),
            "no frozen audio for {id:?}: {:?}",
            receipt.manifest.audio
        );
        assert!(!receipt.manifest.losses.iter().any(|loss| matches!(
            loss,
            LossRecord::ConversionLoss { event: e, detail } if *e == event && detail.contains("MIDI only")
        )));
    }

    // Reopen with the source intact: the instrument mounts and plays again.
    rt.state.instrument = Instrument::default();
    open(&mut rt, &project);
    wait(&mut rt, "remount", |s| status(s).contains("PLAYING AT"));
    assert!(status(&rt.state).contains("2 FROZEN"));
    show(&mut rt);
    tap(&mut rt, "instrument-audition");
    assert!(loudest(&mut rt, 0.3) > 0.01);

    // A saved preset this build cannot find: the project still opens, the
    // instrument says it is silent, and notes plus renders are kept.
    let moved = root.0.join("moved");
    fs::create_dir(&moved).unwrap();
    let mut document = saved.clone();
    let sources: Vec<MediaSource> = document
        .assets
        .iter()
        .map(|asset| {
            let path = moved.join(format!("{}", asset.reference.id));
            fs::copy(project.join(asset.reference.id.relative_path()), &path).unwrap();
            MediaSource {
                id: asset.reference.id,
                path,
            }
        })
        .collect();
    document.instrument.as_mut().unwrap().program = 9;
    let missing = root.0.join("missing-preset");
    Store::new(missing.clone())
        .save(&document, &sources)
        .unwrap();
    open(&mut rt, &missing);
    let line = label(&rt, "instrument-status");
    assert!(line.contains("SILENT"), "{line}");
    assert!(line.contains("2 FROZEN"), "{line}");
    assert!(rt.state.instrument.font.is_none());
    assert!(rt.find("instrument-audition").is_some());
    tap(&mut rt, "instrument-audition");
    assert_eq!(loudest(&mut rt, 0.3), 0.0, "nothing pretends to sound");
    // Held notes on the unavailable instrument are reported, not faked.
    transport::play(&mut rt.state);
    run(&mut rt, 0.2);
    let line = label(&rt, "transport-status");
    assert!(line.contains("2 HELD NOT SOUNDED"), "{line}");
    transport::stop(&mut rt.state, "Stopped");
    // Saving again keeps the source and both renders untouched.
    let resaved = root.0.join("resaved");
    save(&mut rt, &resaved);
    let kept = Store::new(resaved)
        .inspect_committed()
        .unwrap()
        .unwrap()
        .document;
    assert_eq!(kept.instrument, document.instrument);
}

fn take_notes(s: &State) -> Vec<(Target, EventId)> {
    s.takes
        .takes()
        .flat_map(|take| take.notes().iter().map(|n| (n.on().target, n.on().id)))
        .collect()
}
