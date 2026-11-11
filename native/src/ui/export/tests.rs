#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The export action through the shipped view, `poll` and `on_key` paths.
//! Every test types a destination, presses the real buttons and reads the
//! produced bundle back with the existing manifest and WAV readers; the SMF
//! header is read directly because the writers publish standard MIDI files.
use super::*;
use crate::ui::{media::Media, projects, transport};
use brownnote_native::{
    engine::Sample,
    export::manifest::{
        AssetId, AudioRole, LossRecord, MANIFEST_NAME, MEDIA_DIR, Manifest, PartKind,
    },
    export::midi::MIDI_PATH,
    import::{ImportService, MediaPublisher},
    kit::{decode_wav_max, encode_wav},
    live_audio,
    pedals::{
        offline::{Backend, Hash256, Plugin, Stage as OfflineStage},
        render::Stage,
    },
    project::{
        AssetRef,
        live::{LiveInstrumentDocument, LiveTakeDocument},
    },
    takes::PassOptions,
};
use rune::{Env, Key, KeyEvent, Mods, Runtime};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        mpsc,
    },
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "brownnote-export-{}-{}",
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

fn state() -> State {
    let mut state = State::new(crate::ui_test_support::kit(0.5, 44_100, 44_100), true, true);
    state.pedals.auto_load = false;
    // Never touch the machine's real recovery directory.
    state.projects.recovery_base = Some(std::env::temp_dir().join(format!(
        "brownnote-export-recovery-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Relaxed)
    )));
    // A saved project in these tests must not also autosave during an export.
    state.projects.autosave_after = Duration::from_secs(3600);
    state
}

fn runtime() -> Runtime<State> {
    let mut rt = Runtime::new(
        state(),
        crate::ui::view,
        (1120.0, 800.0),
        Env::new(true, 2.0),
    );
    rt.set_full_keyboard_access(true);
    rt.on_key = Some(crate::ui::on_key);
    rt.poll = Some(crate::ui::poll);
    rt.frame(0.01);
    rt
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

fn label(rt: &Runtime<State>, target: &str) -> String {
    let element = rt
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some(target))
        .unwrap_or_else(|| panic!("missing {target}"));
    element.text.or(element.label).unwrap()
}

fn key(rt: &mut Runtime<State>, key: Key) {
    rt.key(key.into());
    rt.frame(0.0);
    rt.frame(0.001);
}

fn focus(rt: &mut Runtime<State>, target: &str) {
    for _ in 0..32 {
        if rt
            .elements()
            .iter()
            .any(|element| element.key.as_deref() == Some(target) && element.focused)
        {
            return;
        }
        key(rt, Key::Tab);
    }
    panic!("Tab did not reach {target}");
}

/// Type a destination into the shipped field: select all, then the path.
fn type_destination(rt: &mut Runtime<State>, path: &Path) {
    focus(rt, "export-destination");
    rt.key(KeyEvent {
        key: Key::Command('a'),
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
    });
    rt.frame(0.001);
    key(rt, Key::Text(path.to_string_lossy().into()));
    assert_eq!(rt.state.export.destination.text, path.to_string_lossy());
}

fn set_destination(rt: &mut Runtime<State>, path: &Path) {
    rt.state.export.destination = TextEdit::new(path.to_string_lossy());
}

fn finish(rt: &mut Runtime<State>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.export.busy() && Instant::now() < deadline {
        rt.frame(0.001);
        std::thread::yield_now();
    }
    assert!(!rt.state.export.busy(), "export worker timed out");
    rt.frame(0.0);
    rt.frame(0.01);
}

fn finish_project(rt: &mut Runtime<State>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.projects.busy() && Instant::now() < deadline {
        rt.frame(0.001);
        std::thread::yield_now();
    }
    assert!(!rt.state.projects.busy(), "project worker timed out");
    rt.frame(0.0);
}

/// Let the panel's enter transition settle, as a person would before clicking.
fn run(rt: &mut Runtime<State>, seconds: f64) {
    for _ in 0..(seconds / 0.01).round() as usize {
        rt.frame(0.01);
    }
}

fn show(rt: &mut Runtime<State>) {
    if rt.find("export-panel").is_none() {
        tap(rt, "export-toggle");
    }
    run(rt, 0.5);
}

const FIXTURE: &[u8] =
    include_bytes!("../../../docs/spikes/sampled-instrument/fixtures/mechanics.sf2");

/// Load the mechanics fixture through the shipped instrument panel.
fn load_instrument(rt: &mut Runtime<State>, root: &Root) {
    let path = root.0.join("mechanics.sf2");
    fs::write(&path, FIXTURE).unwrap();
    tap(rt, "instrument-toggle");
    run(rt, 0.5);
    rt.state.instrument.path = TextEdit::new(path.to_string_lossy());
    tap(rt, "instrument-load");
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.instrument.busy() && Instant::now() < deadline {
        rt.frame(0.01);
        std::thread::yield_now();
    }
    assert!(!rt.state.instrument.busy(), "instrument load timed out");
    assert!(
        rt.state.instrument.error.is_none(),
        "{:?}",
        rt.state.instrument.error
    );
    assert!(
        label(rt, "instrument-status").contains("PLAYING AT"),
        "{}",
        label(rt, "instrument-status")
    );
    // Back to pads so the export panel can own the slot.
    key(rt, Key::Escape);
    run(rt, 0.3);
}

/// One held instrument pass plus two pad hits, through the shipped MIDI fixture.
fn record_instrument_pass(rt: &mut Runtime<State>) {
    transport::scene_midi(&mut rt.state);
    rt.frame(0.0);
    let deadline = Instant::now() + Duration::from_secs(20);
    while rt.state.takes.has_pass() && Instant::now() < deadline {
        rt.frame(0.01);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!rt.state.takes.has_pass(), "pass did not commit");
    assert!(rt.state.takes.takes().next().is_some());
}

/// One committed pad take, recorded through the shipped transport.
fn record_take(rt: &mut Runtime<State>, pad: usize) {
    transport::record(&mut rt.state);
    rt.frame(0.0);
    assert!(rt.state.takes.has_pass(), "pass was not armed");
    rt.state.trigger(pad, 1.0);
    rt.frame(0.05);
    transport::stop(&mut rt.state, "Stopped");
    rt.frame(0.0);
    assert!(!rt.state.takes.has_pass(), "pass did not commit");
}

/// Open the panel, type a destination and press EXPORT.
fn export_to(rt: &mut Runtime<State>, destination: &Path) {
    show(rt);
    type_destination(rt, destination);
    tap(rt, "export-start");
}

fn read_manifest(destination: &Path) -> Manifest {
    Manifest::decode(&fs::read(destination.join(MANIFEST_NAME)).unwrap()).unwrap()
}

fn saved(rt: &mut Runtime<State>, project: &Path) {
    rt.state.projects.path = TextEdit::new(project.to_string_lossy());
    projects::save(&mut rt.state);
    finish_project(rt);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );
}

#[test]
fn export_controls_export_a_complete_bundle_and_report_its_receipt() {
    let root = Root::new();
    let mut rt = runtime();
    record_take(&mut rt, 2);
    let before = (
        rt.state.takes.takes().count(),
        rt.state.master,
        rt.state.takes.tempo().milli_bpm(),
    );
    let destination = root.0.join("bundle");
    // The registry the pads play, held by owned sample buffers.
    let registry: Vec<_> = (0..16)
        .filter_map(|index| rt.state.assets.audible(crate::ui_test_support::pad(index)))
        .map(|asset| asset.imported.sample.data.clone())
        .collect();

    // Keyboard reachable: Tab reaches the field, text entry does not play pads.
    show(&mut rt);
    assert!(rt.find("export-panel").is_some());
    let voices = rt.state.output.meters.counts();
    type_destination(&mut rt, &destination);
    assert_eq!(rt.state.output.meters.counts(), voices);
    tap(&mut rt, "export-start");
    finish(&mut rt);
    assert!(
        rt.state.export.error.is_none(),
        "{:?}",
        rt.state.export.error
    );

    // The bundle is complete: manifest v3, notes.mid, media/.
    let manifest = read_manifest(&destination);
    assert_eq!(manifest.version, 3);
    assert_eq!(manifest.timing.tempo_milli_bpm, before.2);
    assert!(destination.join(MIDI_PATH).is_file());
    assert!(destination.join(MEDIA_DIR).is_dir());
    assert!(
        !manifest.events.is_empty(),
        "the recorded take exported no event"
    );
    assert!(
        manifest.events.iter().any(|event| event.pad.is_some()),
        "pad hits lost their pad identity: {:?}",
        manifest.events
    );
    assert!(!manifest.parts.is_empty());

    // The receipt names the manifest id and the counts.
    let status = label(&rt, "export-status");
    assert!(status.contains("Manifest 3"), "{status}");
    assert!(
        status.contains(&manifest.events.len().to_string()),
        "{status}"
    );
    assert!(
        status.contains(&manifest.assets.len().to_string()),
        "{status}"
    );
    let manifest_id = AssetId::of(&fs::read(destination.join(MANIFEST_NAME)).unwrap());
    assert!(status.contains(&manifest_id.to_string()), "{status}");

    // Existing readers read it back: every asset hashes to its name and
    // decodes as the WAV the manifest describes.
    for asset in &manifest.assets {
        let path = destination.join(&asset.path);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, asset.bytes, "{path:?}");
        assert_eq!(
            brownnote_native::export::manifest::AssetId::of(&bytes),
            asset.id,
            "{path:?}"
        );
        decode_wav_max(bytes.as_slice(), &path, |rate| u64::from(rate) * 60).unwrap();
    }
    // Standard MIDI file: format 1, 960 PPQ, at least the conductor.
    let smf = fs::read(destination.join(MIDI_PATH)).unwrap();
    assert_eq!(&smf[..4], b"MThd");
    assert_eq!(u16::from_be_bytes([smf[8], smf[9]]), 1);
    assert_eq!(u16::from_be_bytes([smf[12], smf[13]]), 960);
    assert!(manifest.midi.as_ref().is_some_and(|midi| midi.ppq == 960));

    // A successful export leaves the project, registry, takes, edits and output
    // alone: the pad registry still holds the same owned sample buffers.
    assert_eq!(
        (
            rt.state.takes.takes().count(),
            rt.state.master,
            rt.state.takes.tempo().milli_bpm(),
        ),
        before
    );
    let after: Vec<_> = (0..16)
        .filter_map(|index| rt.state.assets.audible(crate::ui_test_support::pad(index)))
        .map(|asset| asset.imported.sample.data.clone())
        .collect();
    assert_eq!(registry.len(), after.len());
    assert!(
        registry
            .iter()
            .zip(&after)
            .all(|(was, now)| Arc::ptr_eq(was, now))
    );
}

#[test]
fn export_controls_refuse_existing_relative_and_inside_source_destinations() {
    let root = Root::new();
    let mut rt = runtime();
    show(&mut rt);

    // Relative paths are typed-path refusals (D7): nothing is written.
    type_destination(&mut rt, Path::new("relative/bundle"));
    tap(&mut rt, "export-start");
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("absolute"), "{error}");
    assert!(!rt.state.export.busy());
    assert!(!Path::new("relative/bundle").exists());

    // An existing destination is never overwritten.
    let existing = root.0.join("existing");
    fs::create_dir(&existing).unwrap();
    type_destination(&mut rt, &existing);
    tap(&mut rt, "export-start");
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(fs::read_dir(&existing).unwrap().count(), 0);
    assert!(!rt.state.export.busy());

    // A destination inside the saved project is refused too.
    let project = root.0.join("project");
    saved(&mut rt, &project);
    rt.state.export.error = None;
    show(&mut rt);
    let inside = project.join("bundle");
    type_destination(&mut rt, &inside);
    tap(&mut rt, "export-start");
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("outside the project"), "{error}");
    assert!(!inside.exists());
    assert!(!rt.state.export.busy());

    // The project itself is untouched.
    assert!(project.join("project.json").is_file());
}

#[test]
fn export_controls_refuse_a_second_export_keep_audio_running_and_cancel() {
    let root = Root::new();
    let mut rt = runtime();
    record_take(&mut rt, 3);
    let hold = Arc::new(Hold::new());
    rt.state.export.hold = Some(hold.clone());
    let destination = root.0.join("held");
    export_to(&mut rt, &destination);
    assert!(rt.state.export.busy());

    // A second request is refused, never queued. The panel's button is
    // disabled while busy, and the shipped action refuses as well.
    assert!(
        rt.elements()
            .iter()
            .any(|element| element.key.as_deref() == Some("export-start") && !element.focusable)
    );
    start(&mut rt.state);
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("already running"), "{error}");
    assert_eq!(
        rt.state.export.pending.as_ref().map(|job| job.generation),
        Some(rt.state.export.generation)
    );

    // Pads, transport and meters keep working while the worker runs.
    transport::play(&mut rt.state);
    rt.state.trigger(1, 1.0);
    rt.frame(0.02);
    assert!(rt.state.output.meters.counts()[1] > 0);
    assert!(rt.state.transport.playing());
    transport::stop(&mut rt.state, "Stopped");

    // Escape drops the request before publication: no destination appears and
    // the late completion cannot replace the notice or the session state.
    let takes = rt.state.takes.takes().count();
    let master = rt.state.master;
    key(&mut rt, Key::Escape);
    assert!(!rt.state.export.busy());
    assert!(
        rt.state.export.visible,
        "Escape cancelled instead of closing?"
    );
    assert!(rt.state.export.notice.contains("cancelled"));
    hold.release();
    for _ in 0..20 {
        rt.frame(0.001);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !destination.exists(),
        "a cancelled export published a bundle"
    );
    assert!(rt.state.export.notice.contains("cancelled"));
    assert!(rt.state.export.error.is_none());
    assert_eq!(rt.state.takes.takes().count(), takes);
    assert_eq!(rt.state.master, master);

    // Closing the panel drops a pending request the same way.
    let hold = Arc::new(Hold::new());
    rt.state.export.hold = Some(hold.clone());
    let closed = root.0.join("closed");
    export_to(&mut rt, &closed);
    assert!(rt.state.export.busy());
    close(&mut rt.state);
    assert!(!rt.state.export.busy());
    assert!(!rt.state.export.visible);
    hold.release();
    for _ in 0..20 {
        rt.frame(0.001);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!closed.exists());
}

#[test]
fn export_controls_fail_honestly_when_captured_media_is_missing_or_corrupt() {
    let root = Root::new();
    let mut rt = runtime();
    record_take(&mut rt, 4);
    let seen = Arc::new(Mutex::new(None::<PathBuf>));
    let record = seen.clone();
    rt.state.export.after_stage = Some(Arc::new(move |staging: &Path| {
        *record.lock().unwrap() = Some(staging.to_path_buf());
        let media = staging.join(MEDIA_DIR);
        for entry in fs::read_dir(&media).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
    }));
    let destination = root.0.join("missing");
    export_to(&mut rt, &destination);
    finish(&mut rt);
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("missing"), "{error}");
    assert!(!destination.exists());
    let staging = seen.lock().unwrap().clone().unwrap();
    assert!(!staging.exists(), "staging root was not cleaned up");

    // Truncated bytes are a size mismatch, never a regeneration.
    let truncate = Arc::new(move |staging: &Path| {
        let media = staging.join(MEDIA_DIR);
        let entry = fs::read_dir(&media).unwrap().next().unwrap().unwrap();
        let bytes = fs::read(entry.path()).unwrap();
        fs::write(entry.path(), &bytes[..bytes.len() - 1]).unwrap();
    });
    rt.state.export.after_stage = Some(truncate);
    let destination = root.0.join("corrupt");
    set_destination(&mut rt, &destination);
    tap(&mut rt, "export-start");
    finish(&mut rt);
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("bytes but the document says"), "{error}");
    assert!(!destination.exists());
    // The kit is unchanged by either refusal.
    assert_eq!(rt.state.takes.takes().count(), 1);
}

#[test]
fn export_controls_refuse_armed_passes_pending_work_and_busy_projects() {
    let root = Root::new();
    let mut rt = runtime();
    show(&mut rt);
    let destination = root.0.join("bundle");
    set_destination(&mut rt, &destination);

    // An armed/recording pass is never exported.
    let pass = rt.state.takes.arm(PassOptions::default()).unwrap();
    tap(&mut rt, "export-start");
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("recording"), "{error}");
    assert!(!rt.state.export.busy());
    rt.state.takes.cancel(pass).unwrap();

    // A changed chain that has not rendered refuses with an actionable message.
    rt.state.pedals.pads[0].chain.stages.push(Stage {
        effect: "drive".into(),
        pots: Vec::new(),
        mix: None,
    });
    rt.state.export.error = None;
    tap(&mut rt, "export-start");
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("Finish or cancel"), "{error}");
    assert!(!destination.exists());
    rt.state.pedals.pads[0].chain.stages.clear();

    // A project transaction in the project worker is refused, not interleaved.
    let project = root.0.join("project");
    saved(&mut rt, &project);
    rt.state.projects.path = TextEdit::new(project.to_string_lossy());
    projects::open(&mut rt.state);
    assert!(rt.state.projects.busy());
    rt.state.export.error = None;
    tap(&mut rt, "export-start");
    let error = rt.state.export.error.clone().unwrap();
    assert!(error.contains("save, open or recovery"), "{error}");
    finish_project(&mut rt);
    assert!(!destination.exists());
    assert!(
        rt.state
            .export
            .error
            .as_deref()
            .is_some_and(|error| error.contains("save, open or recovery")),
        "{:?}",
        rt.state.export.error
    );
    assert!(!rt.state.export.busy());
}

/// Releases the worker a [`gate`] is holding. Dropping it is the release, so
/// an unwinding test never leaves a thread parked forever.
struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}

/// A channel pair that parks one worker at a known point: the worker reports
/// entry on the receiver and then blocks until the `Release` drops. Neither
/// case sleeps or races the disk, so both refusals are deterministic.
fn gate() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>, Release) {
    let (entered, waiting) = mpsc::channel();
    let (release, open) = mpsc::channel::<()>();
    let open = Mutex::new(open);
    let hold: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = entered.send(());
        if let Ok(open) = open.lock() {
            let _ = open.recv();
        }
    });
    (hold, waiting, Release(Some(release)))
}

/// Replace the sample importer with one whose reader parks on a [`gate`].
fn blocked_import(rt: &mut Runtime<State>) -> (mpsc::Receiver<()>, Release) {
    let (hold, waiting, release) = gate();
    let ui_thread = std::thread::current().id();
    let reader = Arc::new(
        move |path: &Path, _: &brownnote_native::import::Cancellation| {
            assert_ne!(std::thread::current().id(), ui_thread);
            hold();
            fs::read(path)
        },
    );
    let service = ImportService::with_reader(rt.state.media.owned.clone(), reader).unwrap();
    rt.state.media = Media::with_service(rt.state.media.owned.clone(), Ok(service));
    (waiting, release)
}

/// Poll the shipped frame loop until `done`, with a wall-clock bound for the
/// real worker threads the gates release.
fn wait_until(rt: &mut Runtime<State>, what: &str, done: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(&rt.state) {
        assert!(Instant::now() < deadline, "{what} timed out");
        rt.frame(0.001);
        std::thread::yield_now();
    }
    rt.frame(0.0);
}

/// The busy-work refusal as the mounted panel reports and shows it: the typed
/// copy, no destination directory and no export worker left running.
fn assert_work_pending_refusal(rt: &Runtime<State>, destination: &Path) {
    let error = rt.state.export.error.clone().unwrap_or_default();
    assert!(error.contains("Finish or cancel"), "{error}");
    assert!(error.contains("Nothing was written."), "{error}");
    let shown = label(rt, "export-error");
    assert!(shown.contains("Finish or cancel"), "{shown}");
    assert!(
        !destination.exists(),
        "the refusal created {}",
        destination.display()
    );
    assert!(
        !rt.state.export.busy(),
        "the refusal left an export running"
    );
}

/// A retried export after the held work finished writes the complete bundle.
fn assert_export_succeeds(rt: &mut Runtime<State>, destination: &Path) {
    tap(rt, "export-start");
    finish(rt);
    assert!(
        rt.state.export.error.is_none(),
        "{:?}",
        rt.state.export.error
    );
    assert_eq!(read_manifest(destination).version, 3);
    assert!(destination.join(MIDI_PATH).exists(), "notes.mid is missing");
}

#[test]
fn export_controls_refuse_an_in_flight_sample_import_and_export_once_it_finishes() {
    let root = Root::new();
    let mut rt = runtime();
    record_take(&mut rt, 2);
    let sample = root.0.join("busy sound.wav");
    let wav = encode_wav(&Sample {
        data: vec![0.4; 8_000].into(),
        rate: 44_100,
    })
    .unwrap();
    fs::write(&sample, wav).unwrap();

    // The importer's reader parks on the gate, so media.busy() cannot clear
    // between the pointer press and the export action's capture.
    let (entered, release) = blocked_import(&mut rt);
    rt.state.import_sample(0, Some(sample)).unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(rt.state.media.busy(), "the import was not in flight");

    show(&mut rt);
    let destination = root.0.join("while-importing");
    set_destination(&mut rt, &destination);
    tap(&mut rt, "export-start");
    assert_work_pending_refusal(&rt, &destination);
    assert!(rt.state.media.busy(), "the refusal disturbed the import");

    drop(release);
    wait_until(&mut rt, "sample import", |s| !s.media.busy());
    assert_eq!(rt.state.pad_name(0), "busy sound.wav");
    assert_export_succeeds(&mut rt, &destination);
}

#[test]
fn export_controls_refuse_an_in_flight_instrument_load_and_export_once_it_finishes() {
    let root = Root::new();
    let mut rt = runtime();
    record_take(&mut rt, 3);
    let path = root.0.join("mechanics.sf2");
    fs::write(&path, FIXTURE).unwrap();

    // LOAD SF2 spawns the real loader; its hook parks before the file read.
    tap(&mut rt, "instrument-toggle");
    run(&mut rt, 0.5);
    rt.state.instrument.path = TextEdit::new(path.to_string_lossy());
    let (hold, entered, release) = gate();
    rt.state.instrument.load_hook = Some(hold);
    tap(&mut rt, "instrument-load");
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(rt.state.instrument.busy(), "the load was not in flight");

    // Back to the pads slot with the load still running, as a person can.
    key(&mut rt, Key::Escape);
    run(&mut rt, 0.3);
    show(&mut rt);
    let destination = root.0.join("while-loading");
    set_destination(&mut rt, &destination);
    tap(&mut rt, "export-start");
    assert_work_pending_refusal(&rt, &destination);
    assert!(
        rt.state.instrument.busy(),
        "the refusal disturbed the instrument load"
    );

    drop(release);
    wait_until(&mut rt, "instrument load", |s| !s.instrument.busy());
    assert!(
        rt.state.instrument.error.is_none(),
        "{:?}",
        rt.state.instrument.error
    );
    let status = crate::ui::instrument::status(&rt.state);
    assert!(
        status.contains("PLAYING AT"),
        "the font did not mount: {status}"
    );
    assert_export_succeeds(&mut rt, &destination);
}

#[test]
fn export_controls_use_one_captured_snapshot_and_honour_captured_edits() {
    let root = Root::new();
    let mut rt = runtime();
    record_take(&mut rt, 5);
    let take = rt.state.takes.takes().next().unwrap().id();
    let captured = rt.state.takes.takes().next().unwrap().notes().len();
    assert!(captured > 0);

    // Hold the worker after capture, then mute the take: the bundle must
    // describe the kit as it was when EXPORT was pressed, not as it is now.
    let hold = Arc::new(Hold::new());
    rt.state.export.hold = Some(hold.clone());
    let destination = root.0.join("snapshot");
    export_to(&mut rt, &destination);
    assert!(rt.state.export.busy());
    {
        let State { edit, takes, .. } = &mut rt.state;
        edit.model.set_muted(takes, take, true).unwrap();
    }
    rt.state.master = 0.25;
    hold.release();
    finish(&mut rt);
    assert!(
        rt.state.export.error.is_none(),
        "{:?}",
        rt.state.export.error
    );
    let manifest = read_manifest(&destination);
    assert_eq!(
        manifest.events.len(),
        captured,
        "an edit made during the export leaked into the bundle"
    );
    assert!(manifest.events.iter().all(|event| event.id.take == take.0));

    // The mute belongs to the captured state now: exporting again writes no
    // part, event or audio for that take (D4/D12).
    rt.state.export.hold = None;
    let muted = root.0.join("muted");
    set_destination(&mut rt, &muted);
    tap(&mut rt, "export-start");
    finish(&mut rt);
    assert!(
        rt.state.export.error.is_none(),
        "{:?}",
        rt.state.export.error
    );
    let manifest = read_manifest(&muted);
    assert!(manifest.events.is_empty(), "{:?}", manifest.events);
    assert!(
        manifest
            .audio
            .iter()
            .all(|entry| entry.role == AudioRole::Reference),
        "a muted take contributed audio: {:?}",
        manifest.audio
    );
    assert_eq!(manifest.version, 3);
}

#[test]
fn export_controls_freeze_instrument_renders_on_the_worker() {
    let root = Root::new();
    let mut rt = runtime();
    load_instrument(&mut rt, &root);
    record_instrument_pass(&mut rt);

    let destination = root.0.join("instrument");
    export_to(&mut rt, &destination);
    finish(&mut rt);
    assert!(
        rt.state.export.error.is_none(),
        "{:?}",
        rt.state.export.error
    );
    let manifest = read_manifest(&destination);
    let instrument: Vec<_> = manifest
        .parts
        .iter()
        .filter(|part| matches!(part.kind, PartKind::InstrumentNotes { .. }))
        .map(|part| part.id)
        .collect();
    assert!(!instrument.is_empty(), "{:?}", manifest.parts);
    let events: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| instrument.contains(&event.part))
        .collect();
    assert!(!events.is_empty());
    for event in &events {
        assert!(
            event.asset.is_some(),
            "instrument note {event:?} has no frozen audio fallback"
        );
        assert!(
            manifest
                .audio
                .iter()
                .any(|audio| audio.event == Some(event.id) && audio.role == AudioRole::HitWet),
            "no hit audio for {event:?}"
        );
    }
    assert!(
        !manifest.losses.iter().any(|loss| matches!(
            loss,
            LossRecord::UnsupportedMapping { reason, .. } if reason.contains("MIDI only")
        )),
        "frozen instrument notes were still reported as MIDI only"
    );
}

#[test]
fn export_controls_report_instrument_notes_without_a_frozen_render_as_a_loss() {
    let root = Root::new();
    let mut rt = runtime();
    load_instrument(&mut rt, &root);
    record_instrument_pass(&mut rt);
    // Save the performance, then reopen it on a build that cannot prepare the
    // preset and has no retained renders: the notes must still be reported.
    let project = root.0.join("project");
    saved(&mut rt, &project);
    let path = project.join("project.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let instrument = document
        .get_mut("instrument")
        .expect("the saved project has an instrument");
    instrument["program"] = serde_json::Value::from(9);
    instrument["renders"] = serde_json::Value::Array(Vec::new());
    fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

    rt.state.projects.path = TextEdit::new(project.to_string_lossy());
    projects::open(&mut rt.state);
    finish_project(&mut rt);
    assert!(
        rt.state.projects.error.is_none(),
        "{:?}",
        rt.state.projects.error
    );

    let destination = root.0.join("no-render");
    export_to(&mut rt, &destination);
    finish(&mut rt);
    assert!(
        rt.state.export.error.is_none(),
        "{:?}",
        rt.state.export.error
    );
    let manifest = read_manifest(&destination);
    let instrument: Vec<_> = manifest
        .parts
        .iter()
        .filter(|part| matches!(part.kind, PartKind::InstrumentNotes { .. }))
        .map(|part| part.id)
        .collect();
    assert!(!instrument.is_empty(), "{:?}", manifest.parts);
    let events: Vec<_> = manifest
        .events
        .iter()
        .filter(|event| instrument.contains(&event.part))
        .collect();
    assert!(!events.is_empty());
    for event in &events {
        assert!(
            event.asset.is_none(),
            "an unfrozen note claimed audio: {event:?}"
        );
        assert!(
            !manifest
                .audio
                .iter()
                .any(|audio| audio.event == Some(event.id)),
            "an unfrozen note produced audio: {event:?}"
        );
        assert!(
            manifest.losses.iter().any(|loss| matches!(
                loss,
                LossRecord::UnsupportedMapping { event: lost, reason }
                    if *lost == event.id && reason.contains("MIDI only")
            )),
            "an unfrozen instrument note was silently muted: {event:?}"
        );
    }
    // The pad hits in the same pass still export their audio.
    assert!(
        manifest
            .audio
            .iter()
            .any(|entry| entry.role == AudioRole::HitWet)
    );
}

#[test]
fn export_controls_write_a_retained_live_take_as_its_sample_clips_part() {
    let root = Root::new();
    let mut rt = runtime();
    // A retained live take is project media (D4/D21): the bundle must carry
    // those exact bytes as the take's sample_clips part.
    const RATE: u32 = 48_000;
    let samples: Vec<f32> = (0..2_400)
        .flat_map(|index| {
            let value = if index % 2 == 0 { 0.25 } else { -0.25 };
            [value, value]
        })
        .collect();
    let bytes = live_audio::encode(RATE, &samples).unwrap();
    let reference = AssetRef::of(&bytes);
    let id = AssetId::of(&bytes);
    rt.state.media.owned.publish(&reference, &bytes).unwrap();
    let stage = OfflineStage {
        plugin: Plugin {
            backend: Backend::Clap,
            id: "org.brownnote.fixture.instrument".into(),
            artifact: Hash256::of(b"fixture bundle bytes"),
            abi: "clap-1.2".into(),
            version: "1.0.0".into(),
        },
        location: PathBuf::from("/tmp/brownnote-fixture.clap"),
        params: Vec::new(),
        state: None,
    };
    let mut document = LiveInstrumentDocument::from_stage(&stage, "Fixture".into(), false);
    document.takes = vec![LiveTakeDocument {
        take: "5".into(),
        asset: reference.id,
        rate: RATE,
        frames: 2_400,
    }];
    rt.state
        .live
        .install_project(Some(document), BTreeMap::new());

    let destination = root.0.join("live");
    export_to(&mut rt, &destination);
    finish(&mut rt);
    assert!(
        rt.state.export.error.is_none(),
        "{:?}",
        rt.state.export.error
    );
    let manifest = read_manifest(&destination);
    assert_eq!(manifest.version, 3, "a live take note part requires v3");
    assert!(
        manifest.parts.iter().any(|part| matches!(
            &part.kind,
            PartKind::SampleClips { source } if *source == id
        )),
        "the retained take is not its own sample_clips part: {:?}",
        manifest.parts
    );
    assert_eq!(
        fs::read(destination.join(id.relative_path())).unwrap(),
        bytes,
        "the retained take audio was re-rendered instead of copied"
    );
    assert!(
        manifest
            .audio
            .iter()
            .any(|entry| entry.role == AudioRole::HitWet),
        "the take clip has no hit audio: {:?}",
        manifest.audio
    );
}
