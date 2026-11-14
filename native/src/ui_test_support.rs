//! Encoded fixtures for the mounted application, not fabricated persistent IDs.
use crate::ui::State;
use brownnote_native::{
    assets::ChokeIdentity,
    engine::{Command, SLOTS, Sample},
    import::{Imported, MediaPublisher, retain},
    kit::{Kit, encode_wav},
    project::PadId,
};

pub fn kit(value: f32, rate: u32, frames: usize) -> Kit {
    Kit::from_encoded(std::array::from_fn(|index| {
        let mut bytes = encode_wav(&Sample {
            data: vec![value; frames].into(),
            rate,
        })
        .unwrap();
        if index > 0 {
            // Distinct valid encoded files, with identical PCM as the original
            // fixtures. IDs describe bytes, not an invented hash of floats.
            bytes.extend_from_slice(b"JUNK");
            bytes.extend_from_slice(&4_u32.to_le_bytes());
            bytes.extend_from_slice(&(index as u32).to_le_bytes());
            let riff_bytes = (bytes.len() - 8) as u32;
            bytes[4..8].copy_from_slice(&riff_bytes.to_le_bytes());
        }
        Ok(bytes)
    }))
}

pub fn pad(index: usize) -> PadId {
    PadId::try_from(index as u8).unwrap()
}

pub fn slot(state: &State, index: usize) -> usize {
    state.assets.playback(pad(index)).unwrap().0
}

pub fn retained(state: &State, index: usize) -> Option<usize> {
    let id = state.assets.pad(pad(index)).assignment.as_ref()?.retained?;
    Some(state.assets.asset(id)?.slot)
}

pub fn source_slot(state: &State, index: usize) -> usize {
    state.assets.source(pad(index)).unwrap().slot
}

pub fn source(value: f32, frames: usize) -> Imported {
    kit(value, 8_000, frames).media[0].take().unwrap().imported
}

pub fn replace(state: &mut State, index: usize, imported: &Imported) {
    let bytes = encode_wav(&imported.sample).unwrap();
    state
        .media
        .owned
        .publish(&imported.asset.reference, &bytes)
        .unwrap();
    let ticket = state.assets.begin(pad(index));
    state
        .assets
        .replace_source(
            ticket,
            imported,
            ChokeIdentity::Asset(imported.asset.reference.id),
            |command| state.output.send(command),
        )
        .unwrap();
}

pub fn render(state: &mut State, index: usize, value: f32) {
    let imported = retain(
        Sample {
            data: vec![value; 8_000 * 30].into(),
            rate: 8_000,
        },
        "retained.wav".into(),
        state.media.owned.as_ref(),
    )
    .unwrap();
    let ticket = state.assets.begin(pad(index));
    state
        .assets
        .replace_retained(ticket, &imported, |command| state.output.send(command))
        .unwrap();
}

/// Occupy all slots through actual Load/Trigger/Unload admission. Live
/// assignments and long voices hold them, not fabricated slot-table entries.
pub fn fill_slots(state: &mut State, keep: usize) {
    let selected = state.selected;
    state.choke = false;
    state.output.send(Command::SetChoke(false));
    for index in 0..16 {
        if index != keep {
            replace(
                state,
                index,
                &source(0.01 + index as f32 * 0.001, 8_000 * 30),
            );
            render(state, index, 0.1 + index as f32 * 0.001);
            state.output.advance(0.001);
            state.poll_assets();
        }
    }
    let mut count = 0;
    while (0..SLOTS).any(|slot| state.assets.slots().is_empty(slot)) {
        state.trigger(15, 1.0);
        render(state, 15, 0.2 + count as f32 * 0.001);
        state.output.advance(0.001);
        state.poll_assets();
        count += 1;
        assert!(count < SLOTS, "could not hold the slot pool");
    }
    state.selected = selected;
}

/// One shared bounds assertion for the mounted view (#239): the header, the
/// transport row, the pad grid and every panel must stay inside the window at
/// both the minimum (820×600) and default (1120×800) sizes, with the macOS
/// titlebar inset and the widest header state mounted.
///
/// The in-process `Runtime` reads drawn rects only; duplicate-key layout
/// warnings are exposed by Rune's `--tree` JSON instead, so
/// `native/tests/headless.rs` asserts `warnings == []` on the same states.
pub mod layout {
    use crate::ui::{self, State};
    use brownnote_native::kit::Kit;
    use brownnote_native::output::Output;
    use rune::{Env, Runtime};

    /// Every header control; `retry-audio` is mounted with a failed output.
    pub const HEADER: &[&str] = &[
        "home",
        "voice-count",
        "choke",
        "panic",
        "output-toggle",
        "midi-toggle",
        "edit-toggle",
        "instrument-toggle",
        "live-toggle",
        "retry-audio",
    ];
    /// The second-row strip itself (bounds only: it contains the pills).
    pub const TRANSPORT_STRIP: &str = "transport";
    /// Every interactive second-row control.
    pub const TRANSPORT: &[&str] = &[
        "transport-record",
        "transport-play",
        "transport-loop",
        "transport-mode",
        "transport-count-in",
        "projects-toggle",
        "export-toggle",
    ];
    /// Each panel the header or the strip can open, with the scene that mounts it.
    pub const PANELS: &[(&str, &str)] = &[
        ("output-panel", "output-selection"),
        ("midi-panel", "midi"),
        ("edit-panel", "edit-selected"),
        ("instrument-panel", "instrument-panel"),
        ("live-panel", "live-panel"),
        ("import-panel", "import-path"),
        ("projects-panel", "project-path"),
        ("export-panel", "export-panel"),
    ];
    pub const SIZES: [(f32, f32); 2] = [(820.0, 600.0), (1120.0, 800.0)];

    /// The pads state, optionally with every pad reporting the same long error.
    pub fn state(broken: bool) -> State {
        let kit = if broken {
            Kit::from_encoded(std::array::from_fn(|_| {
                Err(format!(
                    "{}fart_dry.wav: missing file",
                    "/long/path/".repeat(100)
                ))
            }))
        } else {
            super::kit(0.5, 44_100, 44_100)
        };
        State::new(kit, true, true)
    }

    /// Apply one of the binary's deterministic scenes (`--scene`).
    pub fn scene(state: &mut State, name: &str) {
        let (_, apply) = ui::SCENES
            .iter()
            .find(|(key, _)| *key == name)
            .unwrap_or_else(|| panic!("missing scene {name}"));
        apply(state);
    }

    /// Mount the failed-output state that shows RETRY AUDIO in the header.
    pub fn failed(state: &mut State) {
        state.output = Output::virtual_driver(
            state.assets.slots().clone(),
            "Audio output failed".into(),
            true,
        );
    }

    /// A mounted app at `size` with the `--chrome` titlebar inset applied.
    pub fn runtime(state: State, size: (f32, f32)) -> Runtime<State> {
        let mut rt = Runtime::new(state, ui::view, size, Env::new(true, 2.0));
        rt.on_key = Some(ui::on_key);
        rt.poll = Some(ui::poll);
        rt.set_titlebar_inset(rune::Chrome::DEFAULT.titlebar_inset());
        rt.frame(0.0);
        rt.frame(0.0);
        rt
    }

    /// Drawn rect of one mounted key.
    pub fn rect(rt: &Runtime<State>, key: &str, case: &str) -> [f32; 4] {
        rt.presented(key)
            .unwrap_or_else(|| panic!("{case}: missing {key}"))
            .0
    }

    fn inside([x, y, w, h]: [f32; 4], (width, height): (f32, f32), key: &str, case: &str) {
        assert!(
            x >= 0.0
                && y >= 0.0
                && w > 0.0
                && h > 0.0
                && x + w <= width + 0.1
                && y + h <= height + 0.1,
            "{case}: {key} outside {width}x{height}: {x} {y} {w} {h}"
        );
    }

    /// Assert the row `keys` fit and no two siblings share a box.
    fn row(rt: &Runtime<State>, keys: &[&str], size: (f32, f32), case: &str) {
        let rects: Vec<(&str, [f32; 4])> = keys
            .iter()
            .map(|key| {
                let rect = rect(rt, key, case);
                inside(rect, size, key, case);
                (*key, rect)
            })
            .collect();
        for (index, (a, first)) in rects.iter().enumerate() {
            for (b, second) in &rects[index + 1..] {
                let across = first[0] < second[0] + second[2] && second[0] < first[0] + first[2];
                let down = first[1] < second[1] + second[3] && second[1] < first[1] + first[3];
                assert!(
                    !(across && down),
                    "{case}: {a} overlaps {b}: {first:?} {second:?}"
                );
            }
        }
    }

    /// Assert pads keep their 140×60 floor, stay inside the window and never
    /// reach into the editor or the error banner below it.
    fn pads(rt: &Runtime<State>, size: (f32, f32), case: &str) {
        let (_, height) = size;
        let editor = rect(rt, "editor", case);
        let mut widths = Vec::new();
        for index in 0..16 {
            let [x, y, w, h] = rect(rt, &format!("pad-{index}"), case);
            assert!(
                w >= 140.0 && h >= 60.0,
                "{case}: pad-{index} collapsed to {w}x{h}"
            );
            assert!(
                x + w <= size.0 + 0.1 && y + h <= height + 0.1,
                "{case}: pad-{index} outside the window"
            );
            assert!(y + h <= editor[1], "{case}: pad-{index} overlaps editor");
            if index < 12 {
                let below = rect(rt, &format!("pad-{}", index + 4), case)[1];
                assert!(y + h <= below, "{case}: pad-{index} overlaps its row");
            }
            widths.push(w);
        }
        assert!(
            widths.iter().all(|w| (w - widths[0]).abs() < 0.1),
            "{case}: pad columns differ: {widths:?}"
        );
        let grid = rect(rt, "grid", case);
        assert!(
            grid[1] + grid[3] <= editor[1] + 0.1,
            "{case}: grid {grid:?} reaches the editor {editor:?}"
        );
    }

    /// The whole-window assertion: header and transport rows always, the pads
    /// view or the one open panel, at one size.
    pub fn assert_layout(
        rt: &Runtime<State>,
        size: (f32, f32),
        case: &str,
        panel: Option<&str>,
        cancelling: bool,
    ) {
        inside(rect(rt, "header", case), size, "header", case);
        row(rt, HEADER, size, case);
        inside(rect(rt, TRANSPORT_STRIP, case), size, TRANSPORT_STRIP, case);
        let mut transport = TRANSPORT.to_vec();
        if cancelling {
            transport.push("transport-cancel");
        } else {
            assert!(
                rt.presented("transport-cancel").is_none(),
                "{case}: DISCARD mounted with no pass"
            );
        }
        row(rt, &transport, size, case);
        match panel {
            Some(key) => inside(rect(rt, key, case), size, key, case),
            None => pads(rt, size, case),
        }
        inside(rect(rt, "hint", case), size, "hint", case);
        inside(rect(rt, "audio-status", case), size, "audio-status", case);
    }
}

pub fn retire_one(state: &mut State, index: usize) -> usize {
    render(state, index, 0.17);
    let previous = slot(state, index);
    state.trigger(index, 1.0);
    state.output.advance(0.001);
    render(state, index, 0.19);
    state.output.advance(0.001);
    state.poll_assets();
    assert!(!state.assets.slots().is_empty(previous));
    previous
}
