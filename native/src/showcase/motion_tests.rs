//! Rune's motion gate on the real view and state: every look's hit, overlap and panic, recorded
//! on the virtual clock. It fails on motion a person would see go wrong (snaps, flicker, jumps)
//! and on a window that keeps drawing after the voices, meter, lights and ripples have settled.

use super::*;
use brownnote_native::{
    engine::Sample,
    kit::{DEFAULT_SAMPLES, Kit},
    look::theme::ThemeId,
    output::Output,
};
use rune::motion_gate::{self, AllowRow, Job, Recording, Run};
use rune::{Env, Key, Runtime};
use std::{path::Path, sync::Arc};

const HZ: f64 = 60.0;

const ALLOW: &[AllowRow] = &[AllowRow::new(
    "text-swap",
    "",
    "",
    "voice counts, the meter readout and the selected sample's name are live readouts: they change with the engine, not as motion",
)];

fn runtime(theme: ThemeId, reduced_motion: bool) -> Runtime<State> {
    let mut kit = Kit::load(Path::new(DEFAULT_SAMPLES));
    kit.samples = (0..5)
        .map(|i| Sample {
            data: Arc::from(
                (0..30_000)
                    .map(|n| ((n as f32) * 0.01 * (i + 1) as f32).sin() * 0.6)
                    .collect::<Vec<f32>>(),
            ),
            rate: 44_100,
        })
        .collect();
    let output = Output::virtual_driver(kit.samples.clone(), "Headless".into(), false);
    let options = Options {
        theme,
        reduced_motion,
        ..Options::default()
    };
    let mut rt = Runtime::new(
        State::new(kit, output, &options),
        view::view,
        (1120.0, 800.0),
        Env::new(true, 1.0),
    );
    rt.on_key = Some(state::on_key);
    rt.poll = Some(state::poll);
    rt
}

fn key(name: &str) -> impl FnOnce(&mut Runtime<State>) + '_ {
    move |rt| rt.key(Key::Text(name.into()).into())
}

/// Hits, then the voices end (0.68 s samples), the light and meter release, and the window
/// should stop asking for frames.
fn cell(theme: ThemeId, reduced_motion: bool, what: &'static str) -> Recording {
    let mut r = Run::new(runtime(theme, reduced_motion), HZ);
    r.idle(0.5);
    r.step();
    match what {
        "hit" => r.act("key 1", key("1")),
        "overlap" => {
            for name in ["1", "1", "2", "1"] {
                r.act(&format!("key {name}"), key(name));
                r.play(0.05);
            }
        }
        _ => {
            for name in ["1", "w", "d", "v"] {
                r.act(&format!("key {name}"), key(name));
            }
            r.play(0.2);
            r.act("escape", |rt| rt.key(Key::Escape.into()));
        }
    }
    r.play(3.0);
    r.quiet();
    r.play(2.0);
    r.rec
}

#[test]
fn motion_gate_every_look_hits_overlaps_panics_and_goes_quiet() {
    let mut cells: Vec<(String, Job)> = Vec::new();
    for theme in ThemeId::ALL {
        for reduced in [false, true] {
            for what in ["hit", "overlap", "panic"] {
                let name = format!(
                    "{}/{what}{}",
                    theme.slug(),
                    if reduced { "/reduced" } else { "" }
                );
                cells.push((name, Box::new(move || cell(theme, reduced, what)) as Job));
            }
        }
    }
    motion_gate::gate_rows(cells, ALLOW);
}
