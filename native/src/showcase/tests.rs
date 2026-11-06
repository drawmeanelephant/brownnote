use super::*;
use crate::params::Param;
use brownnote_native::{
    engine::{Command, Sample},
    kit::{DEFAULT_SAMPLES, Kit, PADS},
    look::{fx::Settling, motion::RIPPLE_BUDGET, theme::ThemeId},
    output::Output,
};
use rune::{ElementInfo, Env, Key, KeyEvent, Mods, Runtime, Tx};
use std::{path::Path, sync::Arc};

const FRAME: f64 = 1.0 / 60.0;

fn runtime_with(samples: Option<&str>, options: Options, size: (f32, f32)) -> Runtime<State> {
    let mut kit = Kit::load(Path::new(samples.unwrap_or(DEFAULT_SAMPLES)));
    if samples.is_none() {
        kit.samples = (0..5)
            .map(|_| Sample {
                data: Arc::from(vec![0.5; 44_100]),
                rate: 44_100,
            })
            .collect();
    }
    let output = Output::virtual_driver(kit.samples.clone(), "Headless".into(), false);
    let mut rt = Runtime::new(
        State::new(kit, output, &options),
        view::view,
        size,
        Env::new(true, 2.0),
    );
    rt.on_key = Some(state::on_key);
    rt.poll = Some(state::poll);
    rt.frame(0.0);
    rt
}

fn runtime() -> Runtime<State> {
    runtime_with(None, Options::default(), (1120.0, 800.0))
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
    let Some((x, y)) = rt.find(name) else {
        panic!("missing {name}");
    };
    rt.pointer_down(x, y);
    rt.frame(0.0);
    rt.pointer_up(x, y);
    flush(rt);
}

fn run(rt: &mut Runtime<State>, seconds: f64) {
    for _ in 0..(seconds / FRAME).ceil() as usize {
        rt.frame(FRAME);
    }
}

/// A pad's light at the peak of its latest hit.
fn peak(s: &State, pad: usize) -> f32 {
    let m = &s.theme().motion;
    s.lights[pad].level(s.played[pad] + f64::from(m.attack), m)
}

fn element(rt: &Runtime<State>, key: &str) -> ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key) && !e.exiting)
        .unwrap_or_else(|| panic!("missing {key}"))
}

#[test]
fn flags_pick_look_effects_mask_and_view() {
    let Ok(a) = args(
        [
            "--look",
            "space",
            "--fx",
            "light",
            "--only",
            "glow,ripple",
            "--reduced-motion",
            "--lab",
        ]
        .map(String::from)
        .to_vec(),
    ) else {
        panic!("valid flags");
    };
    assert_eq!(a.options.theme, ThemeId::SpaceProgram);
    assert_eq!(a.options.fx, FxLevel::Light);
    assert_eq!(a.options.mask, 1 << 2 | 1 << 3);
    assert!(a.options.reduced_motion);
    assert_eq!(a.options.view, View::Lab);
    for bad in [
        &["--look", "neon"][..],
        &["--only", "lasers"],
        &["--fx"],
        &["--x"],
    ] {
        assert!(args(bad.iter().map(|s| s.to_string()).collect()).is_err());
    }
}

#[test]
fn pointer_hits_keep_the_velocity_curve_and_light_scales_with_it() {
    let mut rt = runtime();
    for (pad, relative, low, high) in [(0, 0.02, 0.95, 1.0), (1, 0.98, 0.4, 0.45)] {
        let Some(([x, y, w, h], _)) = rt.presented(&format!("pad-{pad}")) else {
            panic!("pad");
        };
        rt.pointer_down(x + w * 0.5, y + h * relative);
        flush(&mut rt);
        assert_eq!(rt.state.counts[pad], 1);
        let light = peak(&rt.state, pad);
        assert!((low..=high).contains(&light), "pad {pad}: {light}");
        let shot = rt.state.ripples.live(rt.state.now, 1.0, pad).last();
        assert!(shot.is_some_and(|s| (s.at[1] - relative).abs() < 0.02));
        rt.pointer_up(x + w * 0.5, y + h * relative);
        flush(&mut rt);
        assert_eq!(rt.state.counts[pad], 1, "release never triggers");
    }
    assert!(!element(&rt, "pad-0").focusable);
}

#[test]
fn keys_ignore_repeat_and_modifiers_and_escape_panics_and_releases_light() {
    let mut rt = runtime();
    rt.state.output.send(Command::SetChoke(false));
    key(&mut rt, Key::Text("q".into()));
    key(&mut rt, Key::Text("Q".into()));
    assert_eq!(rt.state.counts[4], 2);
    rt.repeat_key(Key::Text("q".into()).into());
    flush(&mut rt);
    assert_eq!(rt.state.counts[4], 2);
    let cmd = KeyEvent {
        key: Key::Text("q".into()),
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
    };
    assert!(!state::on_key(&mut rt.state, &cmd, &mut Tx::default()));
    assert!(
        (peak(&rt.state, 4) - 1.0).abs() < 0.02,
        "keys hit at full velocity"
    );
    run(&mut rt, 0.5);
    let m = rt.state.theme().motion;
    assert!(rt.state.lights[4].level(rt.state.now, &m) >= m.sustain * 0.99);
    key(&mut rt, Key::Escape);
    assert_eq!(rt.state.counts, [0; 16]);
    run(&mut rt, 3.0);
    assert!(!rt.state.settling(rt.state.now));
}

#[test]
fn muted_master_still_shows_every_hit() {
    let mut rt = runtime();
    rt.update(|s, _| Param::Master.set(s, 0.0));
    run(&mut rt, 0.1);
    key(&mut rt, Key::Text("1".into()));
    run(&mut rt, 0.05);
    assert_eq!(rt.state.counts[0], 1, "the engine plays");
    assert_eq!(rt.state.meter.level, 0.0, "nothing reaches the output");
    assert!(peak(&rt.state, 0) > 0.98);
    assert!(rt.state.lights[0].level(rt.state.now, &rt.state.theme().motion) > 0.3);
    assert_eq!(rt.state.ripples.live(rt.state.now, 1.0, 0).count(), 1);
    assert_eq!(
        element(&rt, "pad-0-voices").text.as_deref(),
        Some("▶ 1"),
        "the pad says it plays"
    );
}

#[test]
fn overlapping_hits_stay_bounded_and_everything_settles_then_stops_waking() {
    let mut rt = runtime();
    rt.state.output.send(Command::SetChoke(false));
    let Some((_, storm)) = state::SCENES.iter().find(|(name, _)| *name == "storm") else {
        panic!("storm scene");
    };
    storm(&mut rt.state);
    let life = rt.state.theme().motion.ripple_life;
    let mut most = 0;
    for _ in 0..(2.4 / FRAME) as usize {
        rt.frame(FRAME);
        most = most.max(rt.state.ripples.drawn(rt.state.now, life));
    }
    assert!(most > RIPPLE_BUDGET, "the storm overlaps: {most}");
    assert!(most <= 2 * RIPPLE_BUDGET, "{most}");
    assert!(rt.state.counts.iter().sum::<u32>() <= 32);
    let mut t = 0.0;
    while rt.state.settling(rt.state.now) {
        rt.frame(FRAME);
        t += FRAME;
        assert!(t < 4.0, "settles once the last voice and light end");
    }
    let mut tx = Tx::default();
    let now = rt.state.now + 1.0;
    assert!(
        !state::poll(&mut rt.state, now, &mut tx),
        "an idle window draws nothing"
    );
}

fn inside(e: &ElementInfo, [w, h]: [f32; 2]) -> bool {
    let [x, y, ew, eh] = e.rect;
    ew > 0.0 && eh > 0.0 && x >= -0.5 && y >= -0.5 && x + ew <= w + 0.5 && y + eh <= h + 0.5
}

fn within(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] >= b[0] - 0.5
        && a[1] >= b[1] - 0.5
        && a[0] + a[2] <= b[0] + b[2] + 0.5
        && a[1] + a[3] <= b[1] + b[3] + 0.5
}

/// cpal's message for a disconnected device, as `poll` reports it.
const LONG_AUDIO_ERROR: &str =
    "Audio error: The requested audio device is not available. It may have been disconnected.";

fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[0] + b[2] - 0.5
        && b[0] < a[0] + a[2] - 0.5
        && a[1] < b[1] + b[3] - 0.5
        && b[1] < a[1] + a[3] - 0.5
}

#[test]
fn every_look_fits_default_and_minimum_windows_in_every_state() {
    let controls = [
        "lab",
        "choke",
        "panic",
        "gain",
        "pan",
        "pitch",
        "master",
        "output-meter",
        "sample-waveform",
        "audio-status",
        "look-bunker",
        "look-space",
        "look-afterhours",
        "fx-full",
        "fx-light",
        "fx-off",
        "reduced-motion",
    ];
    for size in [(1120.0, 800.0), (820.0, 600.0)] {
        for theme in ThemeId::ALL {
            for fx in FxLevel::ALL {
                for missing in [false, true] {
                    let options = Options {
                        theme,
                        fx,
                        ..Options::default()
                    };
                    let mut rt = runtime_with(missing.then_some("/nonexistent"), options, size);
                    if !missing {
                        rt.update(|s, _| {
                            s.output = Output::virtual_driver(
                                s.kit.samples.clone(),
                                LONG_AUDIO_ERROR.into(),
                                true,
                            );
                        });
                        run(&mut rt, 0.6);
                    }
                    let window = [size.0, size.1];
                    let what = format!("{theme:?} {fx:?} missing={missing} {size:?}");
                    for key in controls
                        .iter()
                        .copied()
                        .chain((!missing).then_some("retry-audio"))
                        .chain(missing.then_some("asset-errors"))
                    {
                        let e = element(&rt, key);
                        assert!(inside(&e, window), "{what}: {key} {:?}", e.rect);
                    }
                    let display = element(&rt, "sample-display").rect;
                    let wave = element(&rt, "sample-waveform").rect;
                    for key in ["audio-status", "voice-count"] {
                        let e = element(&rt, key).rect;
                        assert!(within(e, display), "{what}: {key} {e:?} leaves {display:?}");
                        assert!(!overlaps(e, wave), "{what}: {key} covers the waveform");
                    }
                    let voices = element(&rt, "voice-count");
                    let style = theme.theme().type_.caption;
                    let (need, _) =
                        rune::text::measure(voices.text.as_deref().unwrap(), &[], style, None);
                    assert!(
                        voices.rect[2] >= need,
                        "{what}: voice count cut to {:?}",
                        voices.rect
                    );
                    let pads: Vec<_> = (0..16)
                        .map(|i| element(&rt, &format!("pad-{i}")).rect)
                        .collect();
                    for (i, a) in pads.iter().enumerate() {
                        assert!(a[2] >= 100.0 && a[3] >= 70.0, "{what}: pad {i} {a:?}");
                        for b in &pads[i + 1..] {
                            assert!(!overlaps(*a, *b), "{what}: pads overlap");
                        }
                        for key in ["editor", "sample-display"] {
                            assert!(!overlaps(*a, element(&rt, key).rect), "{what}: {key}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn tab_reaches_every_control_with_a_visible_ring_and_steps_change_values() {
    let mut rt = runtime();
    let mut seen = Vec::new();
    for _ in 0..20 {
        key(&mut rt, Key::Tab);
        let Some(focused) = rt.elements().into_iter().find(|e| e.focused) else {
            panic!("tab focuses something");
        };
        assert!(focused.focus_visible, "{:?}", focused.key);
        seen.extend(focused.key);
    }
    for key in [
        "lab",
        "choke",
        "panic",
        "gain",
        "pan",
        "pitch",
        "master",
        "look-space",
    ] {
        assert!(
            seen.iter().any(|k| k == key),
            "{key} not reachable: {seen:?}"
        );
    }
    while element(&rt, "gain").focused.then_some(()).is_none() {
        key(&mut rt, Key::Tab);
    }
    let before = rt.state.settings[0].gain;
    key(&mut rt, Key::Down);
    assert!((rt.state.settings[0].gain - (before - 0.05)).abs() < 1e-4);
    assert_eq!(element(&rt, "gain-value").text.as_deref(), Some("95%"));
}

#[test]
fn missing_samples_disable_pads_and_audio_failure_offers_retry() {
    let mut rt = runtime_with(Some("/nonexistent"), Options::default(), (1120.0, 800.0));
    let ([x, y, w, h], _) = rt.presented("pad-0").unwrap_or_default();
    rt.pointer_down(x + w / 2.0, y + h / 2.0);
    flush(&mut rt);
    key(&mut rt, Key::Text("1".into()));
    assert_eq!(rt.state.counts[0], 0);
    assert_eq!(peak(&rt.state, 0), 0.0);
    assert!(
        element(&rt, "pad-0")
            .label
            .is_some_and(|l| l.contains("MISSING"))
    );
    assert!(rt.find("asset-errors").is_some());
    let mut rt = runtime();
    assert!(rt.find("retry-audio").is_none());
    rt.update(|s, _| {
        s.output = Output::virtual_driver(s.kit.samples.clone(), "gone".into(), true);
    });
    flush(&mut rt);
    assert!(rt.find("retry-audio").is_some());
    assert_eq!(element(&rt, "audio-status").label.as_deref(), Some("gone"));
}

#[test]
fn look_switches_and_reduced_motion_change_the_skin_not_the_sound() {
    let mut rt = runtime();
    tap(&mut rt, "look-afterhours");
    tap(&mut rt, "fx-light");
    assert_eq!(rt.state.theme, ThemeId::Afterhours);
    assert_eq!(rt.state.fx, FxLevel::Light);
    tap(&mut rt, "reduced-motion");
    assert!(rt.state.reduced_motion && !rt.state.skin(false).uses(1 << 0 | 3));
    let Some(([x, y, w, h], _)) = rt.presented("pad-2") else {
        panic!("pad");
    };
    rt.pointer_down(x + w / 2.0, y + h / 2.0);
    run(&mut rt, 0.3);
    assert_eq!(rt.state.counts[2], 1);
    assert_eq!(
        element(&rt, "pad-2").scale,
        1.0,
        "no press squash under reduced motion"
    );
    rt.pointer_up(x + w / 2.0, y + h / 2.0);
    tap(&mut rt, "reduced-motion");
    rt.pointer_down(x + w / 2.0, y + h / 2.0);
    run(&mut rt, 0.3);
    assert!(element(&rt, "pad-2").scale < 0.99);
    rt.pointer_up(x + w / 2.0, y + h / 2.0);
    tap(&mut rt, "lab");
    assert_eq!(rt.state.view, View::Lab);
    run(&mut rt, 0.5);
    tap(&mut rt, "pad-5");
    assert_eq!(rt.state.counts[5], 1, "lab pads play the instrument");
    assert_eq!(PADS[5].key, 'w');
}
