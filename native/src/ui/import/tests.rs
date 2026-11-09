//! Keyboard behavior through the application view, key and poll entry points.
use super::*;
use crate::{ui, ui_test_support as fixture};
use brownnote_native::{engine::Command, import::ImportService, kit, project::AssetRef};
use rune::{Env, Mods, Runtime};
use std::{
    fs,
    io::Cursor,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed},
        mpsc,
    },
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "brownnote-import-controls-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn wav(&self, name: &str, frames: usize) -> PathBuf {
        let path = self.0.join(name);
        fs::write(
            &path,
            kit::encode_wav(&brownnote_native::engine::Sample {
                data: vec![0.4; frames].into(),
                rate: 8_000,
            })
            .unwrap(),
        )
        .unwrap();
        path
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runtime() -> Runtime<State> {
    let mut state = State::new(fixture::kit(0.1, 8_000, 80_000), true, true);
    state.pedals.auto_load = false;
    state.output.send(Command::SetChoke(false));
    for settings in &mut state.settings {
        settings.pitch = 0.0;
    }
    let mut rt = Runtime::new(state, ui::view, (820.0, 600.0), Env::new(true, 2.0));
    rt.set_full_keyboard_access(true);
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.01);
    rt
}

fn key(rt: &mut Runtime<State>, key: Key) {
    rt.key(key.into());
    rt.frame(0.0);
    rt.frame(0.001);
}

fn focus(rt: &mut Runtime<State>, target: &str) {
    for _ in 0..20 {
        if rt
            .elements()
            .iter()
            .any(|e| e.key.as_deref() == Some(target) && e.focused)
        {
            return;
        }
        key(rt, Key::Tab);
    }
    panic!("Tab did not reach {target}");
}

fn press(rt: &mut Runtime<State>, target: &str) {
    focus(rt, target);
    key(rt, Key::Text(" ".into()));
}

fn path(rt: &mut Runtime<State>, value: &Path) {
    focus(rt, "import-path");
    // Drive Rune's actual editor, including a complete paste-shaped Text event.
    key(rt, Key::Text(value.to_str().unwrap().into()));
}

fn finish(rt: &mut Runtime<State>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while rt.state.media.busy() && Instant::now() < deadline {
        rt.frame(0.001);
        std::thread::yield_now();
    }
    assert!(!rt.state.media.busy(), "import worker did not finish");
    rt.frame(0.01);
}

fn label(rt: &Runtime<State>, target: &str) -> String {
    rt.elements()
        .iter()
        .find(|e| e.key.as_deref() == Some(target))
        .unwrap()
        .label
        .clone()
        .unwrap_or_default()
}

#[test]
fn keyboard_import_targets_selected_pad_preserves_spaces_and_plays_real_stereo_float_media() {
    let root = Root::new();
    let file = root.0.join(" My stereo sound .wav");
    let mut encoded = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut encoded,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for _ in 0..48_000 {
            writer.write_sample(0.2_f32).unwrap();
            writer.write_sample(0.6_f32).unwrap();
        }
        writer.finalize().unwrap();
    }
    fs::write(&file, encoded.get_ref()).unwrap();
    let mut rt = runtime();
    key(&mut rt, Key::Text("q".into()));
    assert_eq!(rt.state.selected, 4);
    let untouched = rt.state.assets.pad(fixture::pad(0)).assignment.clone();
    rt.state.settings[4].pan = 0.25;
    press(&mut rt, "import-toggle");
    assert!(label(&rt, "import-target").contains("05"));
    path(&mut rt, &file);
    assert_eq!(rt.state.media.controls.path.text, file.to_str().unwrap());
    key(&mut rt, Key::Enter);
    finish(&mut rt);
    let asset = rt.state.assets.source(fixture::pad(4)).unwrap();
    assert_eq!(
        asset.imported.asset.reference,
        AssetRef::of(encoded.get_ref())
    );
    assert_eq!(asset.imported.sample.rate, 48_000);
    assert_eq!(asset.imported.sample.data.len(), 48_000);
    assert!((asset.imported.sample.data[0] - 0.4).abs() < 0.0001);
    assert_eq!(rt.state.settings[4].pan, 0.25);
    assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, untouched);
    assert!(label(&rt, "import-status").starts_with("IMPORTED"));
    assert!(rt.state.counts[4] > 0, "import stopped the old voice");
    let slot = fixture::slot(&rt.state, 4);
    press(&mut rt, "import-submit");
    finish(&mut rt);
    assert_eq!(
        fixture::slot(&rt.state, 4),
        slot,
        "identical reimport allocated another slot"
    );
    assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, untouched);
    key(&mut rt, Key::Escape);
    assert!(!rt.state.media.controls.visible);
    key(&mut rt, Key::Text("q".into()));
    assert!(rt.state.counts[4] >= 2, "next performance key did not play");
}

#[test]
fn invalid_paths_and_decoder_limits_report_errors_without_changing_playable_assignments() {
    let root = Root::new();
    let empty = root.0.join("empty.wav");
    fs::write(&empty, []).unwrap();
    let corrupt = root.0.join("corrupt.wav");
    fs::write(&corrupt, b"not a WAV").unwrap();
    let oversized = root.wav("too long.wav", 30 * 8_000 + 1);
    let unsupported = root.0.join("pcm8.wav");
    {
        let mut writer = hound::WavWriter::create(
            &unsupported,
            hound::WavSpec {
                channels: 1,
                sample_rate: 8_000,
                bits_per_sample: 8,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample(1_i8).unwrap();
        writer.finalize().unwrap();
    }
    for file in [
        root.0.join("missing.wav"),
        empty,
        corrupt,
        oversized,
        unsupported,
    ] {
        let mut rt = runtime();
        let before = rt.state.assets.pad(fixture::pad(0)).assignment.clone();
        let other = rt.state.assets.pad(fixture::pad(4)).assignment.clone();
        press(&mut rt, "import-toggle");
        path(&mut rt, &file);
        press(&mut rt, "import-submit");
        finish(&mut rt);
        assert!(label(&rt, "import-status").starts_with("IMPORT FAILED"));
        assert!(label(&rt, "import-status").contains(file.file_name().unwrap().to_str().unwrap()));
        assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, before);
        assert_eq!(rt.state.assets.pad(fixture::pad(4)).assignment, other);
        press(&mut rt, "import-cancel");
        key(&mut rt, Key::Text("1".into()));
        key(&mut rt, Key::Text("q".into()));
        assert_eq!(rt.state.counts[0], 1);
        assert_eq!(rt.state.counts[4], 1);
    }
}

struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}

fn blocked(rt: &mut Runtime<State>) -> (mpsc::Receiver<()>, Release, Arc<AtomicUsize>) {
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let ui_thread = std::thread::current().id();
    let reader = Arc::new(
        move |path: &Path, _: &brownnote_native::import::Cancellation| {
            assert_ne!(std::thread::current().id(), ui_thread);
            if reads.fetch_add(1, Relaxed) == 0 {
                entered.send(()).unwrap();
                gate.lock().unwrap().recv().unwrap();
            }
            fs::read(path)
        },
    );
    let service = ImportService::with_reader(rt.state.media.owned.clone(), reader).unwrap();
    rt.state.media = ui::media::Media::with_service(rt.state.media.owned.clone(), Ok(service));
    (waiting, Release(Some(release)), calls)
}

#[test]
fn busy_repeat_cancel_and_later_target_cannot_land_a_stale_completion() {
    let root = Root::new();
    let file = root.wav("slow sound.wav", 8_000);
    let mut rt = runtime();
    let (entered, release, reads) = blocked(&mut rt);
    let original = rt.state.assets.pad(fixture::pad(0)).assignment.clone();
    press(&mut rt, "import-toggle");
    path(&mut rt, &file);
    key(&mut rt, Key::Enter);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(label(&rt, "import-status").starts_with("IMPORTING"));
    assert!(
        !rt.elements()
            .iter()
            .find(|e| e.key.as_deref() == Some("import-submit"))
            .unwrap()
            .focusable
    );
    rt.repeat_key(Key::Enter.into());
    rt.frame(0.0);
    assert_eq!(reads.load(Relaxed), 1);
    press(&mut rt, "import-next");
    assert_eq!(rt.state.selected, 1);
    assert!(!rt.state.media.importing(0));
    assert_eq!(rt.state.media.import_status(0), &ImportStatus::Cancelled);
    // The draft path is retained literally, but this new submit targets pad 02.
    press(&mut rt, "import-submit");
    drop(release);
    finish(&mut rt);
    assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, original);
    assert_eq!(rt.state.pad_name(1), "slow sound.wav");
    assert_eq!(reads.load(Relaxed), 2);
    assert!(label(&rt, "import-status").contains("pad 02"));
}

#[test]
fn cancelling_in_flight_then_reopening_keeps_old_sound_and_draft_path() {
    let root = Root::new();
    let file = root.wav("cancel me.wav", 8_000);
    let mut rt = runtime();
    let (entered, release, _) = blocked(&mut rt);
    let original = rt.state.assets.pad(fixture::pad(0)).assignment.clone();
    press(&mut rt, "import-toggle");
    path(&mut rt, &file);
    key(&mut rt, Key::Enter);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    press(&mut rt, "import-cancel");
    assert!(!rt.state.media.controls.visible);
    drop(release);
    // A later successful job fences completion of the cancelled worker read.
    rt.state.import_sample(4, Some(file.clone())).unwrap();
    finish(&mut rt);
    assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, original);
    press(&mut rt, "import-toggle");
    assert_eq!(rt.state.media.controls.path.text, file.to_str().unwrap());
    assert!(label(&rt, "import-status").starts_with("CANCELLED"));
    key(&mut rt, Key::Escape);
    focus(&mut rt, "import-toggle");
}

#[test]
fn external_selection_cancels_before_completion_and_worker_busy_refusal_is_retryable() {
    let root = Root::new();
    let file = root.wav("blocked.wav", 8_000);
    let mut rt = runtime();
    let (entered, release, _) = blocked(&mut rt);
    let original = rt.state.assets.pad(fixture::pad(0)).assignment.clone();
    press(&mut rt, "import-toggle");
    path(&mut rt, &file);
    key(&mut rt, Key::Enter);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    // Fill the service's single waiting position independently of the panel.
    rt.state.import_sample(4, Some(file.clone())).unwrap();
    rt.state.selected = 1;
    rt.frame(0.01);
    assert!(!rt.state.media.importing(0));
    assert!(label(&rt, "import-target").contains("02"));
    let target = rt.state.assets.pad(fixture::pad(1)).assignment.clone();
    press(&mut rt, "import-submit");
    assert!(label(&rt, "import-status").contains("worker busy"));
    assert_eq!(rt.state.assets.pad(fixture::pad(1)).assignment, target);
    drop(release);
    finish(&mut rt);
    press(&mut rt, "import-submit");
    finish(&mut rt);
    assert_eq!(rt.state.pad_name(1), "blocked.wav");
    assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, original);
}

#[test]
fn unavailable_worker_is_an_accessible_error_not_a_lost_assignment() {
    let mut rt = runtime();
    rt.state.media = ui::media::Media::with_service(
        rt.state.media.owned.clone(),
        Err("fixture startup failure".into()),
    );
    let original = rt.state.assets.pad(fixture::pad(0)).assignment.clone();
    press(&mut rt, "import-toggle");
    path(&mut rt, Path::new("/unread/sample.wav"));
    press(&mut rt, "import-submit");
    assert!(label(&rt, "import-status").contains("worker unavailable"));
    assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, original);
    assert!(!rt.state.media.busy());
}

#[test]
fn prepared_media_waits_for_admission_then_reports_success_without_early_assignment() {
    let root = Root::new();
    let file = root.wav("wait for audio.wav", 8_000);
    let mut rt = runtime();
    let (entered, release, _) = blocked(&mut rt);
    press(&mut rt, "import-toggle");
    path(&mut rt, &file);
    key(&mut rt, Key::Enter);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let original = rt.state.assets.pad(fixture::pad(0)).assignment.clone();
    while rt.state.output.send(Command::SetChoke(false)) {}
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(10);
    // The real poll, without advancing the mixer, proves queue refusal rather
    // than manufacturing a Waiting presentation state.
    while rt.state.media.import_status(0) != &ImportStatus::Waiting && Instant::now() < deadline {
        ui::media::poll(&mut rt.state);
        std::thread::yield_now();
    }
    assert_eq!(rt.state.media.import_status(0), &ImportStatus::Waiting);
    assert_eq!(rt.state.assets.pad(fixture::pad(0)).assignment, original);
    assert!(feedback(&rt.state).0.starts_with("WAITING"));
    finish(&mut rt);
    assert_eq!(rt.state.pad_name(0), "wait for audio.wav");
    assert!(label(&rt, "import-status").starts_with("IMPORTED"));
}

#[test]
fn blank_enter_shortcut_and_external_selection_use_real_key_and_poll_routes() {
    let mut rt = runtime();
    rt.key(KeyEvent {
        key: Key::Command('i'),
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
    });
    rt.frame(0.0);
    focus(&mut rt, "import-path");
    key(&mut rt, Key::Enter);
    assert!(label(&rt, "import-status").contains("Enter a local WAV path"));
    assert!(!rt.state.media.busy());
    rt.state.selected = 15;
    rt.frame(0.01);
    assert!(label(&rt, "import-target").contains("16"));
    press(&mut rt, "import-next");
    assert_eq!(rt.state.selected, 0);
    press(&mut rt, "import-previous");
    assert_eq!(rt.state.selected, 15);
    key(&mut rt, Key::Escape);
}

#[test]
fn scenes_expose_accessible_status_and_minimum_size_keyboard_controls_without_io() {
    for name in [
        "import-path",
        "import-busy",
        "import-error",
        "import-success",
    ] {
        let mut rt = runtime();
        let (_, scene) = ui::SCENES.iter().find(|(key, _)| *key == name).unwrap();
        scene(&mut rt.state);
        rt.frame(0.0);
        assert!(!rt.state.media.busy(), "scene must not start worker IO");
        for target in ["import-previous", "import-next", "import-cancel"] {
            focus(&mut rt, target);
        }
        for target in [
            "import-panel",
            "import-target",
            "import-previous",
            "import-next",
            "import-path",
            "import-submit",
            "import-cancel",
            "import-status",
        ] {
            let ([x, y, w, h], _) = rt.presented(target).unwrap();
            assert!(
                x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0 && x + w <= 820.1 && y + h <= 600.1,
                "{target}: {x}, {y}, {w}, {h}"
            );
        }
        let elements = rt.elements();
        assert_eq!(
            elements
                .iter()
                .find(|e| e.key.as_deref() == Some("import-path"))
                .unwrap()
                .role,
            "textbox"
        );
        assert_eq!(
            elements
                .iter()
                .find(|e| e.key.as_deref() == Some("import-panel"))
                .unwrap()
                .role,
            "dialog"
        );
        assert!(
            ["status", "alert"].contains(
                &elements
                    .iter()
                    .find(|e| e.key.as_deref() == Some("import-status"))
                    .unwrap()
                    .role
            )
        );
    }
}
