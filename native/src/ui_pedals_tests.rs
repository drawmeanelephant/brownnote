//! Runtime tests for the pedal strip. No processes: the catalog comes from
//! the fixture and the renderer is a fake that answers at once.
use crate::ui::{self, CatalogState, FIRST_RENDER_SLOT, State, Status, Tab};
use brownnote_native::{
    engine::{Command, SLOTS, Sample},
    kit::{DEFAULT_SAMPLES, Kit, PADS},
    pedals::{
        brownnote::CatalogError,
        catalog::Catalog,
        render::{Chain, RenderError, RenderJob, Rendered, Stage, argv},
    },
};
use rune::{Env, Key, KeyEvent, Mods, Runtime};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};

const FIXTURE: &str = include_str!("../tests/fixtures/bench-list.txt");

#[derive(Default)]
struct Recorder {
    jobs: Mutex<Vec<Vec<String>>>,
    calls: AtomicUsize,
    answer: Mutex<Option<String>>,
    /// When set, a render blocks until the sender drops. The debounce runs on
    /// real time, so a test that asserts what happens *before* an answer lands
    /// has to fence it off rather than hope the runner is fast.
    hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Recorder {
    /// Makes every later render wait until the returned sender is dropped.
    fn hold(&self) -> std::sync::mpsc::Sender<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        *self.hold.lock().unwrap() = Some(rx);
        tx
    }
}

fn rendered(source: &Sample) -> Rendered {
    let frames = (5.0 * f64::from(source.rate)) as usize;
    Rendered {
        sample: Sample {
            data: Arc::from(vec![0.25_f32; frames]),
            rate: source.rate,
        },
        cached: false,
        clipped: None,
    }
}

fn fake_state(recorder: &Arc<Recorder>) -> State {
    let mut kit = Kit::load(Path::new(DEFAULT_SAMPLES));
    kit.samples = (0..5)
        .map(|_| Sample {
            data: Arc::from(vec![0.5_f32; 44_100]),
            rate: 44_100,
        })
        .collect();
    let mut state = State::new(kit, true, true);
    state.pedals.auto_load = false;
    state.pedals.catalog = CatalogState::Ready(Arc::new(Catalog::parse(FIXTURE).unwrap()));
    let answer = recorder.clone();
    state.pedals.render_fn = Arc::new(move |job: &RenderJob| {
        answer.calls.fetch_add(1, SeqCst);
        answer.jobs.lock().unwrap().push(argv(&job.chain));
        if let Some(held) = answer.hold.lock().unwrap().as_ref() {
            let _ = held.recv();
        }
        match answer.answer.lock().unwrap().clone() {
            Some(message) => Err(RenderError::Invalid(message)),
            None => Ok(rendered(&job.source)),
        }
    });
    state
}

fn runtime(recorder: Arc<Recorder>) -> Runtime<State> {
    let state = fake_state(&recorder);
    let mut rt = Runtime::new(state, ui::view, (1120.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.0);
    rt
}

/// Two frames: the first commits the press, the second lets its appearance
/// animation finish so the element takes pointer input again.
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
    // No time passes, so a burst of presses lands inside one debounce window.
    rt.frame(0.0);
}

fn stacked(rt: &mut Runtime<State>) {
    rt.state
        .output
        .send(brownnote_native::engine::Command::SetChoke(false));
    rt.frame(0.0);
}

/// Wait out the 250 ms debounce and the worker, then land the answer. The
/// deadline is generous because the debounce runs on real time and CI runners
/// are slow, not because the wait is ever expected to last.
fn settle(rt: &mut Runtime<State>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
        rt.frame(0.01);
        if !matches!(
            rt.state.pedals.pads[rt.state.selected].status,
            Status::Rendering
        ) {
            break;
        }
    }
    rt.frame(0.0);
}

fn pedals_tab(rt: &mut Runtime<State>, pad: usize) {
    tap(rt, &format!("pad-{pad}"));
    tap(rt, "editor-tab-pedals");
}

fn add(rt: &mut Runtime<State>, name: &str) {
    tap(rt, "pedal-add");
    rt.resolve_popup(Some(name)).unwrap();
    flush(rt);
}

fn job_list(recorder: &Recorder) -> Vec<Vec<String>> {
    recorder.jobs.lock().unwrap().clone()
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

#[test]
fn the_pedal_tab_offers_only_routable_pedals() {
    let mut rt = runtime(Arc::new(Recorder::default()));
    pedals_tab(&mut rt, 0);
    assert_eq!(rt.state.pedals.tab, Tab::Pedals);
    tap(&mut rt, "pedal-add");
    let menu = rt.popup_menu().expect("the add menu");
    assert!(menu.find("Rat Sketch").is_some());
    assert!(menu.find("Analog Out Tone").is_some());
    assert!(
        menu.find("Signal Chain").is_none(),
        "a system block is offered"
    );
    rt.resolve_popup(None).unwrap();
    flush(&mut rt);
    assert_eq!(
        rt.state.pedals.tab,
        Tab::Pedals,
        "cancelling changes nothing"
    );
}

#[test]
fn adding_a_pedal_renders_once_after_the_debounce_with_only_the_moved_pot() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder.clone());
    pedals_tab(&mut rt, 4);
    // Fenced, so the answer cannot land early on a slow runner: the debounce
    // runs on real time, not on `rt.frame`'s clock.
    let held = recorder.hold();
    add(&mut rt, "Rat Sketch");
    assert_eq!(rt.state.pedals.pads[4].chain.stages.len(), 1);
    assert_eq!(rt.state.pedals.pads[4].status, Status::Rendering);
    assert_eq!(
        rt.state.pedals.pads[4].slot, 1,
        "the base slot before landing"
    );

    drop(held);
    settle(&mut rt);
    assert_eq!(recorder.calls.load(SeqCst), 1);
    assert_eq!(job_list(&recorder)[0], strings(&["--effect", "Rat Sketch"]));
    assert!(matches!(
        rt.state.pedals.pads[4].status,
        Status::Ready { .. }
    ));
    assert_eq!(rt.state.pedals.pads[4].slot, FIRST_RENDER_SLOT);

    // Moving one pot asks once, with just that pot, even across several steps.
    tap(&mut rt, "pot-distortion");
    press(&mut rt, Key::Home);
    press(&mut rt, Key::Right);
    settle(&mut rt);
    let jobs = job_list(&recorder);
    assert_eq!(
        jobs.last().unwrap(),
        &strings(&["--effect", "Rat Sketch", "--pot", "Rat Sketch:Distortion=1"])
    );
    assert_eq!(jobs.len(), 2, "one render per pause: {jobs:?}");
}

#[test]
fn a_hit_before_the_answer_uses_the_base_slot_and_after_uses_the_new_one() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder.clone());
    stacked(&mut rt);
    pedals_tab(&mut rt, 4);
    let held = recorder.hold();
    add(&mut rt, "Rat Sketch");
    tap(&mut rt, "pad-4");
    assert_eq!(rt.state.counts[4], 1, "the hit before landing plays");
    assert_eq!(rt.state.pedals.pads[4].slot, 1);

    drop(held);
    settle(&mut rt);
    assert_eq!(rt.state.pedals.pads[4].slot, FIRST_RENDER_SLOT);
    tap(&mut rt, "pad-4");
    assert_eq!(
        rt.state.counts[4], 2,
        "the hit after landing plays the render"
    );
}

#[test]
fn a_voice_keeps_its_sample_across_a_swap_and_its_slot_retires_after_it_ends() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder);
    pedals_tab(&mut rt, 4);
    add(&mut rt, "Rat Sketch");
    settle(&mut rt);
    assert_eq!(rt.state.pedals.pads[4].slot, FIRST_RENDER_SLOT);

    // Start a voice on the first render, then swap in a second one.
    tap(&mut rt, "pad-4");
    assert_eq!(rt.state.counts[4], 1);
    tap(&mut rt, "pot-distortion");
    press(&mut rt, Key::Home);
    press(&mut rt, Key::Right);
    settle(&mut rt);
    let second = FIRST_RENDER_SLOT + 1;
    assert_eq!(rt.state.pedals.pads[4].slot, second);
    assert_eq!(rt.state.counts[4], 1, "the old voice still plays");
    assert!(
        !rt.state.slots.is_empty(FIRST_RENDER_SLOT),
        "the old slot is still held by the playing voice"
    );

    rt.state.output.send(Command::Panic);
    settle(&mut rt);
    assert_eq!(rt.state.counts[4], 0);
    assert!(
        rt.state.slots.is_empty(FIRST_RENDER_SLOT),
        "the retired slot came back once the voice ended"
    );
}

#[test]
fn duplicates_are_disabled_and_a_fifth_stage_cannot_be_added() {
    let mut rt = runtime(Arc::new(Recorder::default()));
    pedals_tab(&mut rt, 0);
    for name in ["Rat Sketch", "Tape Echo", "Reverb", "Flanger"] {
        add(&mut rt, name);
    }
    assert_eq!(rt.state.pedals.pads[0].chain.stages.len(), 4);
    tap(&mut rt, "pedal-add");
    assert!(
        rt.resolve_popup(Some("Reverb")).is_err(),
        "a duplicate is disabled in the menu"
    );
    rt.resolve_popup(None).unwrap();
    flush(&mut rt);
    assert_eq!(rt.state.pedals.pads[0].chain.stages.len(), 4);
    let add = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("pedal-add"))
        .unwrap();
    assert!(!add.focusable, "the add button is disabled at 4 stages");
}

#[test]
fn removing_the_last_stage_points_the_pad_back_at_its_base_slot() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder);
    pedals_tab(&mut rt, 3);
    add(&mut rt, "Reverb");
    settle(&mut rt);
    assert_eq!(rt.state.pedals.pads[3].slot, FIRST_RENDER_SLOT);
    tap(&mut rt, "pedal-0-remove");
    flush(&mut rt);
    assert_eq!(
        rt.state.pedals.pads[3].slot, 0,
        "back to the base file slot"
    );
    assert_eq!(rt.state.pedals.pads[3].rendered_slot, None);
    assert_eq!(rt.state.pedals.pads[3].status, Status::Base);
}

#[test]
fn reordering_stages_changes_the_requested_order() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder.clone());
    pedals_tab(&mut rt, 0);
    add(&mut rt, "Rat Sketch");
    add(&mut rt, "Tape Echo");
    settle(&mut rt);
    tap(&mut rt, "pedal-1-left");
    settle(&mut rt);
    assert_eq!(
        job_list(&recorder).last().unwrap(),
        &strings(&["--effect", "Tape Echo", "--effect", "Rat Sketch"])
    );
    assert_eq!(rt.state.pedals.pads[0].focus, Some(0));
}

#[test]
fn a_render_error_shows_its_first_line_and_keeps_the_previous_sound() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder.clone());
    pedals_tab(&mut rt, 2);
    add(&mut rt, "Reverb");
    settle(&mut rt);
    let good = rt.state.pedals.pads[2].slot;
    assert_eq!(good, FIRST_RENDER_SLOT);
    *recorder.answer.lock().unwrap() =
        Some("Rat Sketch / Distortion takes 0..120, not 200\nsecond line".into());
    add(&mut rt, "Tape Echo");
    settle(&mut rt);
    assert_eq!(rt.state.pedals.pads[2].slot, good, "the old sound stayed");
    assert_eq!(
        ui::pedals::status_text(&rt.state.pedals.pads[2].status),
        "ERROR · Rat Sketch / Distortion takes 0..120, not 200"
    );
    let tag = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("pad-2-fx"))
        .expect("the FX tag");
    assert_eq!(tag.text.as_deref(), Some("FX 2"));
}

#[test]
fn an_unavailable_catalog_explains_what_to_build_and_leaves_play_alone() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder);
    rt.update(|s, _| {
        s.pedals.catalog =
            CatalogState::Unavailable(CatalogError::NoBench(String::new()).to_string());
    });
    tap(&mut rt, "editor-tab-pedals");
    let message = rt
        .elements()
        .into_iter()
        .filter(|e| e.key.as_deref() == Some("pedal-status"))
        .find_map(|e| e.text)
        .unwrap_or_default();
    assert!(
        message.contains("make -C vendor/GuitarPedal/Validation bench"),
        "{message}"
    );
    tap(&mut rt, "pad-0");
    assert_eq!(rt.state.counts[0], 1, "pads still play");
    tap(&mut rt, "choke");
    assert!(!rt.state.choke, "choke still works");
    press(&mut rt, Key::Escape);
    for _ in 0..60 {
        rt.frame(0.01);
    }
    assert_eq!(rt.state.counts, [0; 16], "panic still works");
}

#[test]
fn a_four_stage_chain_with_ten_pots_fits_the_minimum_window_and_tabs_through() {
    let recorder = Arc::new(Recorder::default());
    let mut state = fake_state(&recorder);
    state.pedals.pads[0].chain = Chain {
        stages: ["Rat Sketch", "Tape Echo", "Reverb", "Parametric EQ"]
            .iter()
            .map(|name| Stage {
                effect: (*name).into(),
                pots: Vec::new(),
                mix: None,
            })
            .collect(),
    };
    state.pedals.pads[0].focus = Some(3);
    state.pedals.tab = Tab::Pedals;
    let mut rt = Runtime::new(state, ui::view, (820.0, 600.0), Env::new(true, 2.0));
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.0);

    let rect = |rt: &Runtime<State>, key: &str| rt.presented(key).unwrap().0;
    let [_, editor_y, _, _] = rect(&rt, "editor");
    for pad in 0..16 {
        let [x, y, w, h] = rect(&rt, &format!("pad-{pad}"));
        assert!(x >= 0.0 && x + w <= 820.1 && y + h <= 600.1);
        assert!(h >= 60.0, "pad {pad} is {h} tall");
        assert!(y + h <= editor_y, "pad {pad} overlaps the editor");
    }
    for key in [
        "pedal-chain",
        "pedal-0",
        "pedal-3",
        "pedal-add",
        "pedal-pots",
        "pedal-status",
    ] {
        let [x, y, w, h] = rect(&rt, key);
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= 820.1 && y + h <= 600.1,
            "{key} overflows"
        );
        assert!(w > 0.0 && h > 0.0, "{key} collapsed");
    }

    // Tab reaches every pedal control, and each shows a focus ring.
    rt.set_full_keyboard_access(true);
    let mut seen = Vec::new();
    for _ in 0..80 {
        press(&mut rt, Key::Tab);
        if let Some(focused) = rt
            .elements()
            .into_iter()
            .find(|e| e.focused)
            .and_then(|e| e.key)
            && !seen.contains(&focused)
        {
            seen.push(focused);
        }
    }
    for key in ["pedal-0", "pedal-0-remove", "pot-ls-freq", "pedal-mix"] {
        assert!(seen.iter().any(|k| k == key), "Tab never reached {key}");
    }
}

/// The text of an element, for the display checks.
fn text_of(rt: &Runtime<State>, key: &str) -> String {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .and_then(|e| e.text)
        .unwrap_or_else(|| panic!("missing text for {key}"))
}

/// Drags an element to `x` and lets the value land, without releasing. It
/// presses on the knob, as a hand does, rather than on the track.
fn drag(rt: &mut Runtime<State>, key: &str, x: f32) {
    let (ex, ey) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
    rt.pointer_down(ex - 40.0, ey);
    rt.frame(0.0);
    rt.pointer_move(x, ey);
    rt.frame(0.0);
}

#[test]
fn cancelling_a_mix_drag_restores_default_and_asks_for_nothing() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder.clone());
    pedals_tab(&mut rt, 4);
    add(&mut rt, "Rat Sketch");
    settle(&mut rt);
    assert_eq!(rt.state.pedals.pads[4].chain.stages[0].mix, None);
    let jobs = job_list(&recorder).len();

    let (x, _) = rt.find("pedal-mix").unwrap();
    drag(&mut rt, "pedal-mix", x + 80.0);
    assert_eq!(
        rt.state.pedals.pads[4].chain.stages[0].mix,
        Some(120),
        "the drag did not move the mix"
    );

    // Rune consumes Escape to cancel a held pointer before window keys, so this
    // is the path the review used.
    press(&mut rt, Key::Escape);
    flush(&mut rt);
    assert_eq!(
        rt.state.pedals.pads[4].chain.stages[0].mix, None,
        "cancelling kept the dragged mix"
    );
    assert!(
        matches!(rt.state.pedals.pads[4].status, Status::Ready { .. }),
        "a cancelled drag left the strip mid-render: {:?}",
        rt.state.pedals.pads[4].status
    );
    assert_eq!(
        job_list(&recorder).len(),
        jobs,
        "a cancelled drag asked for another render"
    );
}

#[test]
fn cancelling_a_mix_drag_restores_a_moved_value() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder.clone());
    pedals_tab(&mut rt, 4);
    add(&mut rt, "Rat Sketch");
    settle(&mut rt);
    let (x, _) = rt.find("pedal-mix").unwrap();
    drag(&mut rt, "pedal-mix", x + 50.0);
    press(&mut rt, Key::Escape);
    flush(&mut rt);
    let original = rt.state.pedals.pads[4].chain.stages[0].mix;
    let jobs = job_list(&recorder).len();

    // From a real value rather than DEFAULT, cancelling still restores it.
    let (x, _) = rt.find("pedal-mix").unwrap();
    drag(&mut rt, "pedal-mix", x + 90.0);
    assert_ne!(
        rt.state.pedals.pads[4].chain.stages[0].mix, original,
        "the second drag did not move the mix"
    );
    press(&mut rt, Key::Escape);
    flush(&mut rt);
    assert_eq!(rt.state.pedals.pads[4].chain.stages[0].mix, original);
    assert_eq!(job_list(&recorder).len(), jobs);
}

#[test]
fn a_load_the_queue_refused_is_landed_once_it_drains() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder);
    pedals_tab(&mut rt, 4);
    add(&mut rt, "Rat Sketch");

    // Let the answer land in `Latest` without running a frame, which is the
    // state a slow worker leaves the strip in when the queue is full.
    let deadline = Instant::now() + Duration::from_secs(10);
    while rt.state.pedals.pads[4].render.is_pending() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!rt.state.pedals.pads[4].render.is_pending());

    let mut filled = 0;
    while rt.state.output.send(Command::SetMaster(0.5)) {
        filled += 1;
        assert!(filled < 1024, "the queue never filled");
    }
    ui::pedals::poll(&mut rt.state);
    assert_eq!(
        rt.state.pedals.pads[4].slot, 1,
        "the pad was pointed at a slot the mixer never got"
    );
    assert!(
        rt.state.pedals.pads[4].rendered_slot.is_none(),
        "the slot was reserved without a load"
    );
    assert!(
        rt.state.pedals.pads[4].pending.is_some(),
        "the answer was dropped instead of kept"
    );

    // A frame drains the queue, and the next poll lands what it kept.
    rt.frame(0.0);
    assert_eq!(rt.state.pedals.pads[4].slot, FIRST_RENDER_SLOT);
    assert!(matches!(
        rt.state.pedals.pads[4].status,
        Status::Ready { .. }
    ));
    tap(&mut rt, "pad-4");
    assert_eq!(rt.state.counts[4], 1, "the landed slot plays");
}

#[test]
fn an_edit_that_changes_nothing_keeps_ready_and_asks_for_nothing() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder.clone());
    pedals_tab(&mut rt, 4);
    add(&mut rt, "Rat Sketch");
    settle(&mut rt);
    tap(&mut rt, "pot-distortion");
    press(&mut rt, Key::End);
    settle(&mut rt);
    assert!(matches!(
        rt.state.pedals.pads[4].status,
        Status::Ready { .. }
    ));
    let jobs = job_list(&recorder).len();

    // Already at the maximum, so the value does not move.
    press(&mut rt, Key::End);
    flush(&mut rt);
    assert!(
        matches!(rt.state.pedals.pads[4].status, Status::Ready { .. }),
        "an unchanged edit stuck on RENDERING: {:?}",
        rt.state.pedals.pads[4].status
    );
    assert_eq!(
        job_list(&recorder).len(),
        jobs,
        "an unchanged edit rendered"
    );
}

#[test]
fn audio_pending_for_an_abandoned_chain_is_dropped() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder);
    pedals_tab(&mut rt, 4);
    add(&mut rt, "Rat Sketch");
    settle(&mut rt);
    assert_eq!(rt.state.pedals.pads[4].slot, FIRST_RENDER_SLOT);

    // Every slot is taken, so the next answer has nowhere to go.
    let sample = rt.state.kit.samples[PADS[4].sample].clone();
    for slot in FIRST_RENDER_SLOT..SLOTS {
        rt.state.slots.set(slot, Some(sample.clone()));
    }
    tap(&mut rt, "pot-distortion");
    press(&mut rt, Key::Home);
    press(&mut rt, Key::Right);
    settle(&mut rt);
    assert!(
        rt.state.pedals.pads[4].pending.is_some(),
        "the answer did not wait for a slot"
    );

    // The user edits again, and a slot frees up. The older answer must not
    // play, because it belongs to the chain that was abandoned.
    press(&mut rt, Key::Right);
    rt.state.slots.set(FIRST_RENDER_SLOT + 1, None);
    rt.frame(0.0);
    assert!(
        rt.state.pedals.pads[4].pending.is_none(),
        "the abandoned answer is still waiting"
    );
    assert_eq!(
        rt.state.pedals.pads[4].slot, FIRST_RENDER_SLOT,
        "the abandoned answer was loaded"
    );
    assert!(
        matches!(rt.state.pedals.pads[4].status, Status::Rendering),
        "the strip reported the abandoned answer as ready"
    );

    rt.state.slots.set(FIRST_RENDER_SLOT + 1, None);
    settle(&mut rt);
    assert_eq!(
        rt.state.pedals.pads[4].slot,
        FIRST_RENDER_SLOT + 1,
        "the newest answer never landed"
    );
}

#[test]
fn retry_audio_drops_slots_no_pad_owns() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder);
    pedals_tab(&mut rt, 4);
    add(&mut rt, "Rat Sketch");
    settle(&mut rt);
    let owned = rt.state.pedals.pads[4].rendered_slot.unwrap();

    // A superseded slot still in the table with no pad pointing at it, and an
    // unload the old mixer never handled.
    let sample = rt.state.kit.samples[PADS[4].sample].clone();
    rt.state.slots.set(9, Some(sample.clone()));
    rt.state.slots.set(20, Some(sample));
    rt.state.pedals.unloads.push(20);
    rt.state.output.failed = true;
    // A rebuild has to happen before the button exists, and `poll` only asks for
    // one when visible state changed.
    tap(&mut rt, "choke");

    tap(&mut rt, "retry-audio");
    assert!(
        rt.state.slots.get(owned).is_some(),
        "the slot the pad plays was dropped"
    );
    assert!(
        rt.state.slots.get(9).is_none(),
        "a slot no pad owns survived the new mixer"
    );
    assert!(rt.state.slots.get(20).is_none());
    assert!(
        rt.state.pedals.unloads.is_empty(),
        "an unload for a slot the new mixer never held is still queued"
    );
}

#[test]
fn the_catalog_loader_runs_the_brownnote_the_state_was_given() {
    let missing = PathBuf::from("/nowhere/brownnote-for-this-test");
    let mut kit = Kit::load(Path::new(DEFAULT_SAMPLES));
    kit.samples = (0..5)
        .map(|_| Sample {
            data: Arc::from(vec![0.5_f32; 44_100]),
            rate: 44_100,
        })
        .collect();
    let mut state = State::with_cache(
        kit,
        true,
        true,
        PathBuf::from("/nowhere/pedal-cache"),
        missing,
    );
    state.pedals.load();

    let deadline = Instant::now() + Duration::from_secs(10);
    while matches!(state.pedals.catalog, CatalogState::Loading) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        ui::pedals::poll(&mut state);
    }
    match &state.pedals.catalog {
        CatalogState::Unavailable(message) => assert!(
            message.contains("/nowhere/brownnote-for-this-test"),
            "the loader ran another binary: {message}"
        ),
        _ => panic!("the default binary answered instead of the one given"),
    }
}

#[test]
fn the_display_length_follows_the_render() {
    let recorder = Arc::new(Recorder::default());
    let mut rt = runtime(recorder);
    pedals_tab(&mut rt, 4);
    assert_eq!(text_of(&rt, "sample-length"), "1.00 s   /   44.1 kHz");
    add(&mut rt, "Rat Sketch");
    settle(&mut rt);
    assert_eq!(
        text_of(&rt, "sample-length"),
        "5.00 s   /   44.1 kHz",
        "the display kept the kit file's length"
    );
}
