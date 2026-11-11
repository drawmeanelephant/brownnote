use crate::ui::{self, State};
use brownnote_native::engine::Command;
use rune::{Color, Env, Key, KeyEvent, Mods, Runtime, Tx};
use std::sync::atomic::Ordering::Relaxed;

fn runtime() -> Runtime<State> {
    let kit = crate::ui_test_support::kit(0.5, 44_100, 44_100);
    let state = State::new(kit, true, true);
    let mut rt = Runtime::new(state, ui::view, (960.0, 680.0), Env::new(true, 2.0));
    rt.on_key = Some(ui::on_key);
    rt.poll = Some(ui::poll);
    rt.frame(0.0);
    rt
}

fn flush(rt: &mut Runtime<State>) {
    rt.frame(0.0);
    rt.frame(0.01);
}

fn key(rt: &mut Runtime<State>, key: Key) {
    rt.key(key.into());
    flush(rt);
}

fn tap(rt: &mut Runtime<State>, name: &str) {
    let (x, y) = rt.find(name).unwrap();
    rt.pointer_down(x, y);
    rt.frame(0.0);
    rt.pointer_up(x, y);
    flush(rt);
}

fn fill(rt: &Runtime<State>, key: &str) -> Color {
    rt.elements()
        .iter()
        .find(|element| element.key.as_deref() == Some(key))
        .unwrap()
        .fill
}

fn key_label(rt: &Runtime<State>, key: &str) -> Option<String> {
    rt.elements()
        .iter()
        .find(|element| element.key.as_deref() == Some(key))
        .unwrap()
        .label
        .clone()
}

fn stacking(rt: &mut Runtime<State>) {
    rt.state.output.send(Command::SetChoke(false));
    rt.frame(0.0);
}

#[test]
fn pointer_down_is_immediate_once_up_does_not_trigger_and_pads_cannot_focus() {
    let mut rt = runtime();
    stacking(&mut rt);
    let (x, y) = rt.find("pad-0").unwrap();
    rt.pointer_down(x, y);
    flush(&mut rt);
    assert_eq!(rt.state.counts[0], 1);
    rt.pointer_up(x, y);
    flush(&mut rt);
    assert_eq!(rt.state.counts[0], 1);
    assert!(
        !rt.elements()
            .iter()
            .find(|e| e.key.as_deref() == Some("pad-0"))
            .unwrap()
            .focusable
    );
    key(&mut rt, Key::Text(" ".into()));
    assert_eq!(rt.state.counts[0], 1);
}

#[test]
fn click_height_sets_top_and_bottom_velocity() {
    for (relative, low, high) in [(0.02, 0.95, 1.0), (0.98, 0.4, 0.45)] {
        let mut rt = runtime();
        let ([x, y, w, h], _) = rt.presented("pad-0").unwrap();
        rt.pointer_down(x + w * 0.5, y + h * relative);
        flush(&mut rt);
        let velocity = rt.state.peak / (0.5 * 0.8 * std::f32::consts::FRAC_1_SQRT_2);
        assert!((low..=high).contains(&velocity), "{velocity}");
    }
}

#[test]
fn upper_and_lower_q_repeat_and_modified_keys() {
    let mut rt = runtime();
    stacking(&mut rt);
    key(&mut rt, Key::Text("q".into()));
    key(&mut rt, Key::Text("Q".into()));
    assert_eq!(rt.state.counts[4], 2);
    rt.repeat_key(Key::Text("q".into()).into());
    flush(&mut rt);
    assert_eq!(rt.state.counts[4], 2);
    for mods in [
        Mods {
            cmd: true,
            ..Mods::default()
        },
        Mods {
            ctrl: true,
            ..Mods::default()
        },
        Mods {
            alt: true,
            ..Mods::default()
        },
    ] {
        let event = KeyEvent {
            key: Key::Text("q".into()),
            mods,
        };
        assert!(!ui::on_key(&mut rt.state, &event, &mut Tx::default()));
        rt.key(event);
        flush(&mut rt);
    }
    let command_q = KeyEvent {
        key: Key::Command('q'),
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
    };
    assert!(!ui::on_key(&mut rt.state, &command_q, &mut Tx::default()));
    assert_eq!(rt.state.counts[4], 2);
}

#[test]
fn escape_after_click_and_panic_button_clear_real_voices() {
    let mut rt = runtime();
    for panic in ["escape", "button"] {
        tap(&mut rt, "pad-0");
        key(&mut rt, Key::Text("q".into()));
        assert_eq!(rt.state.counts[0], 1);
        assert_eq!(rt.state.counts[4], 1);
        if panic == "escape" {
            key(&mut rt, Key::Escape);
        } else {
            tap(&mut rt, "panic");
        }
        assert_eq!(rt.state.counts, [0; 16]);
        key(&mut rt, Key::Text("q".into()));
        assert_eq!(rt.state.counts[4], 1);
        key(&mut rt, Key::Escape);
    }
}

#[test]
fn escape_also_panics_while_pointer_is_still_held_on_a_pad() {
    let mut rt = runtime();
    let (x, y) = rt.find("pad-0").unwrap();
    rt.pointer_down(x, y);
    flush(&mut rt);
    assert_eq!(rt.state.counts[0], 1);
    key(&mut rt, Key::Escape);
    assert_eq!(rt.state.counts, [0; 16]);
    rt.pointer_up(x, y);
    flush(&mut rt);
    assert_eq!(rt.state.counts, [0; 16]);
}

#[test]
fn slider_keyboard_steps_limits_and_next_hit_settings() {
    let mut rt = runtime();
    tap(&mut rt, "pad-4");
    key(&mut rt, Key::Escape);
    for (name, expected) in [
        ("gain", 0.05),
        ("pan", -0.95),
        ("pitch", -11.0),
        ("master", 0.01),
    ] {
        tap(&mut rt, name);
        key(&mut rt, Key::Home);
        key(&mut rt, Key::Left);
        key(&mut rt, Key::Right);
        let value = match name {
            "gain" => rt.state.settings[4].gain,
            "pan" => rt.state.settings[4].pan,
            "pitch" => rt.state.settings[4].pitch,
            _ => rt.state.master,
        };
        assert!((value - expected).abs() < 1e-5, "{name}: {value}");
        key(&mut rt, Key::End);
        key(&mut rt, Key::Right);
        let expected = match name {
            "gain" => 1.5,
            "pan" => 1.0,
            "pitch" => 12.0,
            _ => 1.0,
        };
        let value = match name {
            "gain" => rt.state.settings[4].gain,
            "pan" => rt.state.settings[4].pan,
            "pitch" => rt.state.settings[4].pitch,
            _ => rt.state.master,
        };
        assert!((value - expected).abs() < 1e-5, "{name}: {value}");
    }
    assert_eq!(rt.state.settings[0].gain, 1.0);
    rt.state.peak = 0.0;
    key(&mut rt, Key::Text("q".into()));
    assert!((rt.state.peak - 0.75).abs() < 1e-6);
    for _ in 0..52 {
        rt.frame(0.01);
    }
    assert_eq!(rt.state.counts[4], 0, "octave-up sample ends at 0.5 s");
}

#[test]
fn slider_drag_cancel_restores_original_value() {
    let mut rt = runtime();
    let original = rt.state.settings[0].gain;
    let ([x, y, w, h], _) = rt.presented("gain").unwrap();
    rt.pointer_down(x + w - 3.0, y + h / 2.0);
    flush(&mut rt);
    assert!(rt.state.settings[0].gain > original);
    key(&mut rt, Key::Escape);
    assert_eq!(rt.state.settings[0].gain, original);
    rt.pointer_up(x + w - 3.0, y + h / 2.0);
    flush(&mut rt);
    assert_eq!(rt.state.settings[0].gain, original);
}

#[test]
fn missing_pad_does_nothing_while_good_pads_still_play() {
    let mut rt = runtime();
    let mut kit = crate::ui_test_support::kit(0.5, 44_100, 44_100);
    kit.media[0] = None;
    kit.errors[0] = Some("fart_dry.wav: missing".into());
    rt.state = State::new(kit, true, true);
    rt.frame(0.0);
    tap(&mut rt, "pad-0");
    key(&mut rt, Key::Text("1".into()));
    assert_eq!(rt.state.counts[0], 0);
    assert!(rt.find("asset-errors").is_some());
    key(&mut rt, Key::Text("q".into()));
    assert_eq!(rt.state.counts[4], 1);
}

#[test]
fn idle_poll_returns_false_and_virtual_voices_expire() {
    let mut rt = runtime();
    assert!(!ui::poll(&mut rt.state, 0.0, &mut Tx::default()));
    key(&mut rt, Key::Text("q".into()));
    for _ in 0..90 {
        rt.frame(0.033);
    }
    assert_eq!(rt.state.counts, [0; 16]);
    assert_eq!(rt.state.peak, 0.0);
}

#[test]
fn all_pads_and_controls_fit_minimum_window_even_with_long_asset_errors() {
    for missing in [false, true] {
        let mut state = ui::init();
        if missing {
            let error = format!("{}fart_dry.wav: missing file", "/long/path/".repeat(100));
            state = State::new(
                brownnote_native::kit::Kit::from_encoded(std::array::from_fn(|_| {
                    Err(error.clone())
                })),
                true,
                true,
            );
        }
        let mut rt = Runtime::new(state, ui::view, (820.0, 600.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let mut widths = Vec::new();
        let ([_, editor_y, _, _], _) = rt.presented("editor").unwrap();
        for index in 0..16 {
            let ([x, y, w, h], _) = rt.presented(&format!("pad-{index}")).unwrap();
            assert!(
                x >= 0.0 && y >= 0.0 && x + w <= 820.1 && y + h <= 600.1,
                "pad {index} missing={missing}: {x} {y} {w} {h}"
            );
            assert!(h >= 60.0);
            assert!(y + h <= editor_y, "pad {index} overlaps editor");
            if index < 12 {
                let ([_, next_y, _, _], _) = rt.presented(&format!("pad-{}", index + 4)).unwrap();
                assert!(y + h <= next_y, "pad {index} overlaps next row");
            }
            widths.push(w);
        }
        assert!(widths.iter().all(|w| (w - widths[0]).abs() < 0.1));
        for key in [
            "import-toggle",
            "gain",
            "pan",
            "pitch",
            "master",
            "hint",
            "pad-15",
            "sample-display",
            "sample-waveform",
            "output-meter",
            "audio-status",
        ] {
            let ([x, y, w, h], _) = rt.presented(key).unwrap();
            assert!(x + w <= 820.1 && y + h <= 600.1, "{key} overflows");
            assert!(w > 0.0 && h > 0.0, "{key} collapsed");
        }
    }
}

#[test]
fn display_tracks_selection_and_meter_reports_real_signal_then_silence() {
    let mut rt = runtime();
    let label = |rt: &Runtime<State>, key: &str| {
        rt.elements()
            .iter()
            .find(|e| e.key.as_deref() == Some(key))
            .unwrap()
            .label
            .clone()
    };
    let original = label(&rt, "sample-waveform");
    tap(&mut rt, "pad-10");
    assert_ne!(label(&rt, "sample-waveform"), original);
    assert!(
        label(&rt, "sample-waveform")
            .unwrap()
            .contains("fart_chorus.wav")
    );
    assert!(rt.state.peak > 0.0);
    assert_eq!(rt.state.counts[10], 1);
    key(&mut rt, Key::Escape);
    for _ in 0..90 {
        rt.frame(0.033);
    }
    assert_eq!(rt.state.peak, 0.0);
    assert_eq!(rt.state.counts, [0; 16]);
}

#[test]
fn muted_hits_still_animate_input_feedback_and_stop_redrawing_at_rest() {
    let mut rt = runtime();
    rt.state.settings[0].gain = 0.0;
    rt.state.trigger(0, 1.0);
    assert!(ui::poll(&mut rt.state, 0.02, &mut Tx::default()));
    assert_eq!(rt.state.peak, 0.0);
    assert!(ui::poll(&mut rt.state, 0.04, &mut Tx::default()));
    for i in 1..=12 {
        ui::poll(
            &mut rt.state,
            0.04 + f64::from(i) * 0.25,
            &mut Tx::default(),
        );
    }
    assert!(!ui::poll(&mut rt.state, 3.05, &mut Tx::default()));
}

/// Drive the virtual mixer past full scale with two overlapping voices.
fn clipped_overload(rt: &mut Runtime<State>) {
    rt.state.choke = false;
    rt.state.output.send(Command::SetChoke(false));
    rt.state.master = 1.0;
    rt.state.output.send(Command::SetMaster(1.0));
    rt.state.settings[0].pan = -1.0;
    rt.state.settings[0].gain = 2.0;
    rt.frame(0.02);
    rt.state.trigger(0, 1.0);
    rt.state.trigger(0, 1.0);
    rt.frame(0.02);
}

#[test]
fn an_overload_lights_the_clip_lamp_and_every_poll_exchanges_the_engine_flag() {
    let mut rt = runtime();
    let unlit = fill(&rt, "clip");
    clipped_overload(&mut rt);
    assert!(
        rt.state.peak > 1.5,
        "the meter keeps the pre-clamp peak: {}",
        rt.state.peak
    );
    assert!(rt.state.meter.lit());
    assert!(
        !rt.state.output.meters.clipped.load(Relaxed),
        "the poll exchanges the latched engine flag for false"
    );
    assert_ne!(fill(&rt, "clip"), unlit, "the lamp visibly changes");
    let elements = rt.elements();
    let find = |key: &str| {
        elements
            .iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap()
    };
    assert_eq!(
        find("output-meter").label.as_deref(),
        Some("Output peak, clipping"),
        "the meter names the clip state"
    );
    assert_eq!(
        find("clip-label").text.as_deref(),
        Some("CLIP"),
        "clip is not colour alone"
    );
    let lamp = find("clip").rect;
    let db = find("meter").rect;
    assert!(
        lamp[0] + lamp[2] <= db[0] + 0.01,
        "the clip lamp clears the dB readout: {lamp:?} {db:?}"
    );

    rt.state.output.meters.clipped.store(true, Relaxed);
    rt.frame(0.02);
    assert!(
        !rt.state.output.meters.clipped.load(Relaxed),
        "a later poll exchanges it too, so no stale true survives"
    );
    assert!(rt.state.meter.lit());
}

#[test]
fn the_clip_latch_holds_for_1_5_seconds_then_clears_and_relights_on_a_new_overload() {
    let mut rt = runtime();
    clipped_overload(&mut rt);
    assert!(rt.state.meter.lit());
    key(&mut rt, Key::Escape);
    for _ in 0..4 {
        rt.frame(0.1);
    }
    assert_eq!(rt.state.counts, [0; 16]);

    rt.state.output.meters.clipped.store(true, Relaxed);
    rt.frame(0.02);
    assert!(rt.state.meter.lit(), "a fresh overload lights it");
    assert!(!rt.state.output.meters.clipped.load(Relaxed));
    for _ in 0..14 {
        rt.frame(0.1);
    }
    assert!(
        rt.state.meter.lit(),
        "still lit 1.4 s after the last overload"
    );
    rt.frame(0.11);
    assert!(
        !rt.state.meter.lit(),
        "silence clears the latch without a new overload"
    );

    rt.frame(0.02);
    rt.state.output.meters.clipped.store(true, Relaxed);
    rt.frame(0.02);
    assert!(rt.state.meter.lit(), "a second overload lights it again");
    assert!(
        !rt.state.output.meters.clipped.load(Relaxed),
        "the exchanged flag cannot stay true"
    );
}

#[test]
fn a_clean_full_scale_peak_never_lights_the_clip_lamp() {
    let mut rt = runtime();
    rt.state.choke = false;
    rt.state.output.send(Command::SetChoke(false));
    rt.state.master = 1.0;
    rt.state.output.send(Command::SetMaster(1.0));
    rt.state.settings[0].pan = -1.0;
    // The fixture is 0.5 full scale, so 2.0 gain is exactly 1.0, not over.
    rt.state.settings[0].gain = 2.0;
    rt.frame(0.02);
    rt.state.trigger(0, 1.0);
    rt.frame(0.02);
    assert!((rt.state.peak - 1.0).abs() < 1e-6, "peak {}", rt.state.peak);
    assert!(!rt.state.output.meters.clipped.load(Relaxed));
    assert!(!rt.state.meter.lit());
    assert_eq!(
        key_label(&rt, "output-meter").as_deref(),
        Some("Output peak")
    );
}

#[test]
fn silence_zero_gain_and_a_stopped_voice_light_nothing() {
    let mut rt = runtime();
    rt.frame(0.1);
    assert!(!rt.state.meter.lit(), "a silent mixer lights nothing");
    rt.state.settings[0].gain = 0.0;
    rt.state.trigger(0, 1.0);
    rt.frame(0.02);
    assert_eq!(rt.state.peak, 0.0);
    assert!(!rt.state.meter.lit(), "zero pad gain lights nothing");
    key(&mut rt, Key::Escape);
    for _ in 0..4 {
        rt.frame(0.1);
    }
    assert_eq!(rt.state.counts, [0; 16]);
    assert_eq!(rt.state.peak, 0.0);
    assert!(!rt.state.meter.lit(), "a stopped voice lights nothing");
}

#[test]
fn the_wordmark_button_closes_every_panel_and_returns_to_pads() {
    let mut rt = runtime();
    for toggle in [
        "output-toggle",
        "instrument-toggle",
        "live-toggle",
        "projects-toggle",
        "export-toggle",
    ] {
        tap(&mut rt, toggle);
    }
    // The import, take-edit and MIDI panels are plain view flags.
    rt.state.media.controls.visible = true;
    rt.state.edit_panel = true;
    rt.state.midi_panel = true;
    rt.frame(0.0);
    assert!(
        rt.find("pads").is_none(),
        "a panel is still replacing the pad grid"
    );

    {
        let elements = rt.elements();
        let home = elements
            .iter()
            .find(|element| element.key.as_deref() == Some("home"))
            .expect("the wordmark is not a control");
        assert_eq!(home.role, "button", "the wordmark is not a button");
        assert!(home.focusable, "the wordmark cannot be reached by keyboard");
    }

    tap(&mut rt, "home");

    assert!(!rt.state.projects.visible, "the projects panel stayed open");
    assert!(!rt.state.export.visible, "the export panel stayed open");
    assert!(!rt.state.devices.visible, "the output panel stayed open");
    assert!(
        !rt.state.instrument.visible,
        "the instrument panel stayed open"
    );
    assert!(!rt.state.live.visible, "the live panel stayed open");
    assert!(
        !rt.state.media.controls.visible,
        "the import panel stayed open"
    );
    assert!(!rt.state.edit_panel, "the take edit panel stayed open");
    assert!(!rt.state.midi_panel, "the MIDI panel stayed open");
    assert!(rt.find("pads").is_some(), "the pad grid did not come back");
}
