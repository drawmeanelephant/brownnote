mod audio;
mod control;
pub(crate) mod devices;
mod edit;
mod graphics;
#[cfg(test)]
#[path = "ui_import_tests.rs"]
mod import_tests;
mod instrument;
mod live;
mod media;
mod midi;
pub(crate) mod pedals;
pub(crate) mod projects;
mod transport;
mod view;
use brownnote_native::{
    assets::AssetRegistry,
    engine::Command,
    kit::{DEFAULT_SAMPLES, Kit, PADS},
    output::Output,
    performance::{LoopBars, TakeId},
    takes::Takes,
};
use media::pad_id;
#[allow(unused_imports)]
pub use pedals::{CatalogState, Status, Tab};
use rune::{Color, El, Key, KeyEvent, TextStyle, Tx, el, text};
use std::{path::Path, sync::atomic::Ordering::Relaxed, time::Duration};
pub use view::view;

#[derive(Clone, Copy)]
pub struct Settings {
    pub gain: f32,
    pub pan: f32,
    pub pitch: f32,
}

pub struct State {
    pub output: Output,
    pub assets: AssetRegistry,
    pub media: media::Media,
    pub projects: projects::Projects,
    pub settings: [Settings; 16],
    pub selected: usize,
    pub master: f32,
    pub choke: bool,
    pub counts: [u32; 16],
    pub peak: f32,
    pub pedals: pedals::Pedals,
    pub devices: devices::Devices,
    pub midi: midi::Midi,
    /// View-side mirror of the MIDI panel visibility (the lane handle keeps its
    /// own flag for the header label and Escape handling).
    pub midi_panel: bool,
    pub edit: edit::Editor,
    pub takes: Takes,
    /// View-side gate for the take/event edit panel.
    pub edit_panel: bool,
    pub transport: transport::Transport,
    pub instrument: instrument::Instrument,
    pub live: live::Live,
    now: f64,
    flashes: [f64; 16],
    wake: Option<rune::Waker>,
}

impl State {
    pub fn new(kit: Kit, headless: bool, no_audio: bool) -> Self {
        Self::with_cache(
            kit,
            headless,
            no_audio,
            pedals::default_cache(),
            pedals::default_bin(),
        )
    }

    pub fn with_cache(
        kit: Kit,
        headless: bool,
        no_audio: bool,
        pedal_cache: std::path::PathBuf,
        brownnote: std::path::PathBuf,
    ) -> Self {
        let (assets, media) = media::bootstrap(&kit);
        let pedals = pedals::Pedals::with_bin(pedal_cache, brownnote);
        let devices = if headless || no_audio {
            devices::Devices::disabled()
        } else {
            let root = brownnote_native::paths::Resolver::current()
                .and_then(|paths| {
                    paths.resolve(brownnote_native::paths::Resource::Preferences, None)
                })
                .map_err(|error| error.to_string());
            devices::Devices::load(root)
        };
        let output = devices.initial_output(
            assets.slots().clone(),
            if headless {
                "Headless: no audio device".into()
            } else {
                "Audio off (--no-audio)".into()
            },
        );
        // Interactive startup mounts the real MIDI input (D8 machine preference
        // under Resource::Preferences); headless/test constructors stay safe.
        let midi = if headless {
            midi::Midi::disabled()
        } else {
            let root = brownnote_native::paths::Resolver::current()
                .and_then(|paths| {
                    paths.resolve(brownnote_native::paths::Resource::Preferences, None)
                })
                .map_err(|error| error.to_string());
            midi::Midi::load(root, !no_audio, brownnote_native::performance::TakeId(0))
        };
        let takes = fresh_takes();
        let live = if headless {
            live::Live::disabled()
        } else {
            let root = brownnote_native::paths::Resolver::current()
                .and_then(|paths| {
                    paths.resolve(brownnote_native::paths::Resource::Preferences, None)
                })
                .map_err(|error| error.to_string());
            live::Live::load(root)
        };
        Self {
            output,
            assets,
            media,
            projects: projects::Projects::default(),
            settings: std::array::from_fn(|i| Settings {
                gain: 1.0,
                pan: 0.0,
                pitch: PADS[i].semitones,
            }),
            selected: 0,
            master: 0.8,
            choke: true,
            counts: [0; 16],
            peak: 0.0,
            pedals,
            devices,
            midi,
            midi_panel: false,
            edit: edit::Editor::default(),
            takes,
            edit_panel: false,
            transport: transport::Transport::default(),
            instrument: instrument::Instrument::default(),
            live,
            now: 0.0,
            flashes: [0.0; 16],
            wake: None,
        }
    }

    pub fn trigger(&mut self, pad: usize, velocity: f32) {
        let Some((slot, group)) = self.assets.playback(pad_id(pad)) else {
            return;
        };
        let settings = self.settings[pad];
        self.output.send(Command::Trigger {
            pad,
            slot,
            group,
            velocity,
            gain: settings.gain,
            pan: settings.pan,
            semitones: settings.pitch,
        });
        transport::capture_trigger(self, pad, velocity);
        self.selected = pad;
        self.flashes[pad] = self.now + 0.120;
    }

    /// The length and rate of what this pad triggers now: its rendered sample
    /// when it has one, otherwise the kit file.
    pub fn pad_audio(&self, pad: usize) -> (f32, u32) {
        let Some(asset) = self.assets.audible(pad_id(pad)) else {
            return (0.0, 0);
        };
        let sample = &asset.imported.sample;
        (sample.data.len() as f32 / sample.rate as f32, sample.rate)
    }
}

/// An empty registry for a new or take-less project.
pub fn fresh_takes() -> Takes {
    // 8 bars is always a valid LoopBars (the accepted set is 1, 2, 4, 8);
    // TakeId(1) keeps this registry clear of the MIDI input's TakeId(0).
    let loop_bars =
        LoopBars::try_from(8).unwrap_or_else(|_| unreachable!("8 bars is a valid loop length"));
    Takes::new(loop_bars, TakeId(1))
}

pub fn init() -> State {
    State::new(Kit::load(Path::new(DEFAULT_SAMPLES)), true, true)
}

/// Lane accessors for the take/event edit panel (#147 wiring): the panel reads
/// through `edit_read` and acts through `edit_access`, both bound to the single
/// `State.takes` registry.
pub fn edit_read(s: &State) -> (&edit::Editor, &Takes) {
    (&s.edit, &s.takes)
}

pub fn edit_access(s: &mut State) -> (&mut edit::Editor, &mut Takes) {
    (&mut s.edit, &mut s.takes)
}

pub fn setup(s: &mut State, wake: rune::Waker) {
    s.output.set_waker(wake.clone());
    s.pedals.set_waker(wake.clone());
    s.wake = Some(wake);
    s.pedals.load();
}

pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if projects::on_key(s, event, tx) {
        return true;
    }
    if instrument::on_key(s, event, tx) {
        return true;
    }
    if live::on_key(s, event, tx) {
        return true;
    }
    if media::controls::on_key(s, event, tx) {
        return true;
    }
    let midi = s.midi.clone();
    if midi::on_key(s, &midi, event, tx) {
        s.midi_panel = false;
        return true;
    }
    if event.mods.cmd || event.mods.ctrl || event.mods.alt || event.mods.logo {
        return false;
    }
    let pad = match &event.key {
        Key::Text(key) => PADS
            .iter()
            .position(|p| key.eq_ignore_ascii_case(&p.key.to_string())),
        _ => None,
    };
    if pad.is_none() && event.key != Key::Escape {
        return false;
    }
    if tx.key_repeat() {
        return true;
    }
    if event.key == Key::Escape {
        if s.edit_panel {
            s.edit_panel = false;
        } else {
            transport::panic(s);
        }
    } else if let Some(pad) = pad {
        s.trigger(pad, 1.0);
    }
    tx.wake_after(Duration::from_millis(1));
    true
}

pub fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let projects_changed = projects::poll(s);
    if s.projects.busy() {
        tx.wake_after(Duration::from_millis(10));
    }
    let elapsed = (clock - s.now).max(0.0);
    let was_lit = s.flashes.map(|until| until > s.now);
    s.now = clock;
    let failed = s.recover_stream_error();
    let devices_changed = devices::poll(s);
    if s.devices.busy() {
        tx.wake_after(Duration::from_millis(1));
    }
    let instrument_changed = instrument::poll(s);
    if instrument::wants_wake(s) {
        tx.wake_after(Duration::from_millis(10));
    }
    let midi = s.midi.clone();
    let midi_changed = midi::poll(s, &midi);
    let live_changed = live::poll(s);
    if live::wants_wake(s) {
        tx.wake_after(Duration::from_millis(10));
    }
    let edit_changed = edit::poll(s, edit_access);
    s.output.advance(elapsed);
    let transport_changed = transport::poll(s);
    if s.transport.active() {
        tx.wake_after(Duration::from_millis(10));
    }
    let import_changed = media::controls::poll(s);
    let media_changed = media::poll(s);
    if s.media.busy() {
        tx.wake_after(Duration::from_millis(33));
    }
    let pedals_changed = pedals::poll(s);
    let counts = s.output.meters.counts();
    let fresh_peak = f32::from_bits(s.output.meters.peak.swap(0, Relaxed));
    let peak = fresh_peak.max(s.peak * (-elapsed as f32 * 7.0).exp());
    let peak = if peak < 0.001 { 0.0 } else { peak };
    let lit = s.flashes.map(|until| until > clock);
    let changed = projects_changed
        || devices_changed
        || midi_changed
        || live_changed
        || instrument_changed
        || edit_changed
        || transport_changed
        || failed
        || pedals_changed
        || import_changed
        || media_changed
        || counts != s.counts
        || peak != s.peak
        || was_lit != lit
        || (elapsed > 0.0 && was_lit.contains(&true));
    s.counts = counts;
    s.peak = peak;
    if lit.contains(&true)
        || counts.iter().any(|n| *n > 0)
        || peak > 0.0
        || s.assets.has_retirements()
    {
        tx.wake_after(Duration::from_millis(33));
    }
    changed
}

type Scene = (&'static str, fn(&mut State));
pub const SCENES: &[Scene] = &[
    ("project-path", |s| {
        s.projects.visible = true;
        s.projects.path = rune::TextEdit::new("/kits/Portable kit");
    }),
    ("project-error", |s| {
        s.projects.visible = true;
        s.projects.error = Some("Project could not be opened. Active kit unchanged.".into());
    }),
    ("import-path", |s| {
        media::controls::scene(s, media::controls::SceneKind::Path)
    }),
    ("import-busy", |s| {
        media::controls::scene(s, media::controls::SceneKind::Busy)
    }),
    ("import-error", |s| {
        media::controls::scene(s, media::controls::SceneKind::Error)
    }),
    ("import-success", |s| {
        media::controls::scene(s, media::controls::SceneKind::Success)
    }),
    ("output-selection", |s| {
        devices::scene(s, devices::SceneKind::Selection)
    }),
    ("output-missing", |s| {
        devices::scene(s, devices::SceneKind::Missing)
    }),
    ("output-invalid", |s| {
        devices::scene(s, devices::SceneKind::Invalid)
    }),
    ("midi", |s| {
        s.midi_panel = true;
        let midi = s.midi.clone();
        midi::scene(s, &midi, midi::SceneKind::Connected);
    }),
    ("edit-empty", |s| {
        s.edit_panel = true;
        let _ = edit::scene(&mut s.edit, &mut s.takes, edit::SceneKind::Empty);
    }),
    ("edit-selected", |s| {
        s.edit_panel = true;
        let _ = edit::scene(&mut s.edit, &mut s.takes, edit::SceneKind::Selected);
    }),
    ("edit-muted", |s| {
        s.edit_panel = true;
        let _ = edit::scene(&mut s.edit, &mut s.takes, edit::SceneKind::Muted);
    }),
    ("edit-removed", |s| {
        s.edit_panel = true;
        let _ = edit::scene(&mut s.edit, &mut s.takes, edit::SceneKind::Removed);
    }),
    ("edit-error", |s| {
        s.edit_panel = true;
        let _ = edit::scene(&mut s.edit, &mut s.takes, edit::SceneKind::Error);
    }),
    ("transport-midi", transport::scene_midi),
    ("playing", |s| {
        for pad in [0, 4, 10] {
            s.trigger(pad, 1.0);
        }
    }),
    ("missing-samples", |s| {
        *s = State::new(
            Kit::load(Path::new("/nonexistent/brownnote-samples")),
            true,
            true,
        );
    }),
    ("pedals", |s| pedals::scene(s, pedals::SceneKind::Ready)),
    ("pedal-error", |s| {
        pedals::scene(s, pedals::SceneKind::Error)
    }),
    ("pedals-unavailable", |s| {
        pedals::scene(s, pedals::SceneKind::Unavailable)
    }),
    ("no-audio", |s| {
        s.install_output(Output::virtual_driver(
            s.assets.slots().clone(),
            "Audio off (--no-audio)".into(),
            false,
        ));
    }),
];

fn ink(hex: &str) -> Color {
    rune::color::hex(hex).unwrap_or(Color::WHITE)
}

fn caption(value: impl Into<String>) -> El {
    text(value.into())
        .font(TextStyle::new(10.0, 550.0))
        .color(ink("#8c9e98"))
}

fn accent(sample: usize) -> Color {
    ink(["#c8f77b", "#ffac7e", "#73d9c1", "#b5a3f5", "#f097bf"][sample])
}

fn button(key: &str, title: &str) -> El {
    el().key(key)
        .role("button")
        .label(title)
        .focusable(true)
        .h(32.0)
        .px(12.0)
        .center()
        .radius(6.0)
        .gradient(ink("#303b37"), ink("#202a26"))
        .border_gradient(1.0, ink("#526059"), ink("#2b3631"))
        .shadow(2.0, 4.0, ink("#080e0b"))
        .hover(ink("#425249"))
        .child(
            text(title)
                .font(TextStyle::new(11.0, 650.0))
                .color(ink("#eeeeed")),
        )
}
