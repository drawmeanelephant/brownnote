use crate::ui::{self, State};
use brownnote_native::{
    engine::{Command, Sample},
    kit::{DEFAULT_SAMPLES, Kit},
};
use rune::{Env, Key, KeyEvent, Mods, Runtime, Tx};
use std::{path::Path, sync::Arc};

fn runtime() -> Runtime<State> {
    let mut kit = Kit::load(Path::new(DEFAULT_SAMPLES));
    kit.samples = (0..5)
        .map(|_| Sample {
            data: Arc::from(vec![0.5; 44_100]),
            rate: 44_100,
        })
        .collect();
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
    rt.state.kit.errors[0] = Some("fart_dry.wav: missing".into());
    rt.state.kit.banner = "fart_dry.wav: missing".into();
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
            state.kit.errors = std::array::from_fn(|_| Some(error.clone()));
            state.kit.banner = error;
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
