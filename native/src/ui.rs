mod control;
mod graphics;
pub(crate) mod pedals;
use brownnote_native::{
    engine::{Command, Slots},
    kit::{DEFAULT_SAMPLES, FILES, Kit, PADS},
    output::Output,
};
use control::{Control, slider};
use graphics::{Envelope, envelope, led, logo, meter, waveform};
#[allow(unused_imports)]
pub use pedals::{CatalogState, FIRST_RENDER_SLOT, Status, Tab};
use rune::{Color, Cx, Drag, El, Key, KeyEvent, Phase, TextStyle, Tx, col, el, row, text};
use std::{path::Path, sync::atomic::Ordering::Relaxed, time::Duration};

#[derive(Clone, Copy)]
pub struct Settings {
    pub gain: f32,
    pub pan: f32,
    pub pitch: f32,
}

pub struct State {
    pub kit: Kit,
    pub output: Output,
    /// What each sample slot holds, kept so a fresh `Output` can be rebuilt
    /// after a device failure.
    pub slots: Slots,
    pub settings: [Settings; 16],
    pub selected: usize,
    pub master: f32,
    pub choke: bool,
    pub counts: [u32; 16],
    pub peak: f32,
    pub pedals: pedals::Pedals,
    /// The waveform of each pad's rendered sample, if it has one.
    pub render_waves: [Option<graphics::Envelope>; 16],
    now: f64,
    flashes: [f64; 16],
    wake: Option<rune::Waker>,
    waveforms: [Envelope; 5],
}

impl State {
    pub fn new(kit: Kit, headless: bool, no_audio: bool) -> Self {
        Self::with_cache(kit, headless, no_audio, pedals::default_cache())
    }

    pub fn with_cache(
        kit: Kit,
        headless: bool,
        no_audio: bool,
        pedal_cache: std::path::PathBuf,
    ) -> Self {
        let waveforms = std::array::from_fn(|i| envelope(&kit.samples[i]));
        let pedals = pedals::Pedals::new(pedal_cache);
        let slots = Slots::from(kit.samples.clone());
        let output = if headless || no_audio {
            Output::virtual_driver(
                slots.clone(),
                if headless {
                    "Headless: no audio device".into()
                } else {
                    "Audio off (--no-audio)".into()
                },
                false,
            )
        } else {
            Output::open(slots.clone())
                .unwrap_or_else(|e| Output::virtual_driver(slots.clone(), e.to_string(), true))
        };
        Self {
            kit,
            output,
            slots,
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
            render_waves: std::array::from_fn(|_| None),
            now: 0.0,
            flashes: [0.0; 16],
            wake: None,
            waveforms,
        }
    }

    pub fn trigger(&mut self, pad: usize, velocity: f32) {
        let spec = &PADS[pad];
        if self.kit.errors[spec.sample].is_some() {
            return;
        }
        let settings = self.settings[pad];
        self.output.send(Command::Trigger {
            pad,
            slot: self.pedals.pads[pad].slot,
            group: spec.sample,
            velocity,
            gain: settings.gain,
            pan: settings.pan,
            semitones: settings.pitch,
        });
        self.selected = pad;
        self.flashes[pad] = self.now + 0.120;
    }

    fn retry(&mut self) {
        self.output = Output::open(self.slots.clone())
            .unwrap_or_else(|e| Output::virtual_driver(self.slots.clone(), e.to_string(), true));
        self.output.send(Command::SetMaster(self.master));
        self.output.send(Command::SetChoke(self.choke));
        if let Some(wake) = &self.wake {
            self.output.set_waker(wake.clone());
        }
    }
}

pub fn init() -> State {
    State::new(Kit::load(Path::new(DEFAULT_SAMPLES)), true, true)
}

pub fn setup(s: &mut State, wake: rune::Waker) {
    s.output.set_waker(wake.clone());
    s.pedals.set_waker(wake.clone());
    s.wake = Some(wake);
    s.pedals.load();
}

pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
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
        s.output.send(Command::Panic);
    } else if let Some(pad) = pad {
        s.trigger(pad, 1.0);
    }
    tx.wake_after(Duration::from_millis(1));
    true
}

pub fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let elapsed = (clock - s.now).max(0.0);
    let was_lit = s.flashes.map(|until| until > s.now);
    s.now = clock;
    let error = s.output.take_error();
    let failed = error.is_some();
    if let Some(error) = error {
        s.output = Output::virtual_driver(s.slots.clone(), format!("Audio error: {error}"), true);
        s.output.send(Command::SetMaster(s.master));
        s.output.send(Command::SetChoke(s.choke));
    }
    s.output.advance(elapsed);
    let pedals_changed = pedals::poll(s);
    let counts = s.output.meters.counts();
    let fresh_peak = f32::from_bits(s.output.meters.peak.swap(0, Relaxed));
    let peak = fresh_peak.max(s.peak * (-elapsed as f32 * 7.0).exp());
    let peak = if peak < 0.001 { 0.0 } else { peak };
    let lit = s.flashes.map(|until| until > clock);
    let changed = failed
        || pedals_changed
        || counts != s.counts
        || peak != s.peak
        || was_lit != lit
        || (elapsed > 0.0 && was_lit.contains(&true));
    s.counts = counts;
    s.peak = peak;
    if lit.contains(&true) || counts.iter().any(|n| *n > 0) || peak > 0.0 {
        tx.wake_after(Duration::from_millis(33));
    }
    changed
}

type Scene = (&'static str, fn(&mut State));
pub const SCENES: &[Scene] = &[
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
        s.output = Output::virtual_driver(
            s.kit.samples.clone(),
            "Audio off (--no-audio)".into(),
            false,
        );
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

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let tall = cx.layout_size("window").is_some_and(|[_, h]| h > 720.0);
    let voices: u32 = s.counts.iter().sum();
    let mut controls = row()
        .gap(8.0)
        .center_y()
        .child(
            row()
                .gap(6.0)
                .center_y()
                .px(8.0)
                .child(led(voices > 0, ink("#c8f77b")))
                .child(caption(format!("{voices:02} / 32 VOICES")).key("voice-count")),
        )
        .child(
            button("choke", if s.choke { "CHOKE  ON" } else { "CHOKE  OFF" })
                .role("switch")
                .checked(s.choke)
                .on_press(|s: &mut State| {
                    s.choke = !s.choke;
                    s.output.send(Command::SetChoke(s.choke));
                }),
        )
        .child(
            button("panic", "PANIC  /  ESC")
                .gradient(ink("#56382e"), ink("#34251f"))
                .border_gradient(1.0, ink("#92604d"), ink("#4b342b"))
                .on_press_tx(|s: &mut State, tx| {
                    s.output.send(Command::Panic);
                    tx.wake_after(Duration::from_millis(1));
                }),
        );
    if s.output.failed {
        controls = controls
            .child(button("retry-audio", "RETRY AUDIO").on_press(|s: &mut State| s.retry()));
    }
    let mut grid = col().key("pads").grow(1.0).min_h(0.0).gap(8.0);
    for r in 0..4 {
        let mut line = row().key(format!("row-{r}")).grow(1.0).gap(8.0);
        for c in 0..4 {
            line = line.child(pad(s, r * 4 + c));
        }
        grid = grid.child(line);
    }
    let mut root = col()
        .key("window")
        .role("window")
        .gradient(ink("#202a26"), ink("#101713"))
        .pt(20.0)
        .px(20.0)
        .pb(12.0)
        .gap(10.0)
        .child(
            row()
                .h(42.0)
                .justify_between()
                .center_y()
                .child(
                    row().gap(12.0).center_y().child(logo()).child(
                        col()
                            .gap(3.0)
                            .child(
                                text("brownnote")
                                    .font(TextStyle::new(27.0, 800.0))
                                    .color(ink("#eef1dd")),
                            )
                            .child(
                                caption("FLATULENCE SYNTHESIZER     /     BN–01")
                                    .color(ink("#b9c99e")),
                            ),
                    ),
                )
                .child(controls),
        )
        .child(display(s, tall))
        .child(
            row()
                .h(14.0)
                .justify_between()
                .center_y()
                .child(caption("01—16    /    PRESSURE PADS").color(ink("#c0cbbf")))
                .child(caption("HIT HIGHER. GO LOUDER.")),
        )
        .child(grid)
        .child(editor(s))
        .child(
            row()
                .h(14.0)
                .gap(16.0)
                .center_y()
                .child(caption("1–4 / QWER / ASDF / ZXCV     ·     ESC TO VENT").key("hint"))
                .child(el().grow(1.0))
                .child(
                    caption(&s.output.status)
                        .key("audio-status")
                        .min_w(0.0)
                        .truncate(),
                ),
        );
    if !s.kit.banner.is_empty() {
        root = root.child(
            caption(&s.kit.banner)
                .key("asset-errors")
                .color(ink("#ffad92"))
                .label(&s.kit.banner)
                .truncate(),
        );
    }
    if s.output.dropped > 0 {
        root = root.child(caption(format!(
            "Command queue full: {} dropped",
            s.output.dropped
        )));
    }
    root
}

fn display(s: &State, tall: bool) -> El {
    let spec = &PADS[s.selected];
    let sample = &s.kit.samples[spec.sample];
    let duration = sample.data.len() as f32 / sample.rate as f32;
    let color = accent(spec.sample);
    row()
        .key("sample-display")
        .h(if tall { 116.0 } else { 76.0 })
        .gap(20.0)
        .padding(12.0)
        .radius(10.0)
        .gradient(ink("#101b16"), ink("#0b120e"))
        .border_gradient(1.0, ink("#090f0c"), ink("#3c4940"))
        .child(
            col()
                .w(135.0)
                .gap(5.0)
                .center_y()
                .child(caption(format!("SAMPLE {:02} / SELECTED", s.selected + 1)).color(color))
                .child(
                    text(spec.label)
                        .key("selected-name")
                        .truncate()
                        .font(TextStyle::new(22.0, 750.0))
                        .color(ink("#edf4dc")),
                )
                .children(tall.then(|| {
                    caption(format!(
                        "{duration:.2} s   /   {:.1} kHz",
                        sample.rate as f32 / 1000.0
                    ))
                })),
        )
        .child(
            col()
                .key("sample-waveform")
                .grow(1.0)
                .min_w(0.0)
                .gap(3.0)
                .role("img")
                .label(format!(
                    "Normalized sample waveform: {}",
                    FILES[spec.sample]
                ))
                .child(waveform(s.pad_wave(s.selected), color, true))
                .child(
                    row()
                        .justify_between()
                        .child(caption("0.00").color(ink("#617c68")))
                        .child(caption("SAMPLE WAVEFORM").color(ink("#617c68")))
                        .child(caption(format!("{duration:.2} s")).color(ink("#617c68"))),
                ),
        )
        .child(col().w(175.0).center_y().child(meter(s.peak)))
}

fn editor(s: &State) -> El {
    let spec = &PADS[s.selected];
    let pedals_tab = s.pedals.tab == Tab::Pedals;
    let tab = |key: &'static str, label: &'static str, tab: Tab| {
        let active = s.pedals.tab == tab;
        el().key(key)
            .role("tab")
            .label(label)
            .focusable(true)
            .checked(active)
            .h(16.0)
            .px(7.0)
            .center()
            .radius(3.0)
            .fill(if active {
                ink("#2f3d34")
            } else {
                ink("#1b251f")
            })
            .border(
                1.0,
                if active {
                    ink("#c8f77b")
                } else {
                    ink("#3b4c42")
                },
            )
            .child(
                text(label)
                    .font(TextStyle::new(9.5, 700.0))
                    .color(if active {
                        ink("#eef4d8")
                    } else {
                        ink("#8c9e98")
                    }),
            )
            .on_press(move |s: &mut State| s.pedals.tab = tab)
    };
    let header = if pedals_tab {
        pedals::header(s)
    } else {
        row()
            .justify_between()
            .child(
                caption(format!("SHAPE {:02}  /  {}", s.selected + 1, spec.label))
                    .color(accent(spec.sample)),
            )
            .child(caption("PAD SETTINGS APPLY TO THE NEXT HIT"))
    };
    let body = if pedals_tab {
        pedals::strip(s)
    } else {
        row()
            .gap(22.0)
            .child(slider(s, Control::Gain))
            .child(slider(s, Control::Pan))
            .child(slider(s, Control::Pitch))
            .child(slider(s, Control::Master))
    };
    col()
        .key("editor")
        .radius(9.0)
        .px(10.0)
        .py(8.0)
        .gap(4.0)
        .gradient(ink("#28332d"), ink("#1d2721"))
        .border_gradient(1.0, ink("#424e44"), ink("#1b251f"))
        .child(
            row().justify_between().center_y().child(header).child(
                row()
                    .gap(4.0)
                    .center_y()
                    .child(tab("editor-tab-shape", "SHAPE", Tab::Shape))
                    .child(tab("editor-tab-pedals", "PEDALS", Tab::Pedals))
                    .child(if pedals_tab {
                        caption("UP TO 4 · NEXT HIT").color(ink("#8c9e98"))
                    } else {
                        caption("NEXT HIT").color(ink("#8c9e98"))
                    }),
            ),
        )
        .child(body)
}

fn pad(s: &State, index: usize) -> El {
    let spec = &PADS[index];
    let color = accent(spec.sample);
    let missing = s.kit.errors[spec.sample].as_deref();
    let flash = ((s.flashes[index] - s.now) / 0.120).clamp(0.0, 1.0) as f32;
    let playing = s.counts[index] > 0;
    let selected = s.selected == index;
    let base = ink("#25312b");
    let tint = if missing.is_some() {
        ink("#7b877f")
    } else {
        color
    };
    let glow = if playing { 0.16 } else { 0.0 } + flash * 0.30;
    let mut pad = col()
        .key(format!("pad-{index}"))
        .role("button")
        .focusable(false)
        .label(format!(
            "{} · {}{}",
            spec.label,
            FILES[spec.sample],
            missing.map_or(String::new(), |e| format!(" · MISSING: {e}"))
        ))
        .grow(1.0)
        .min_w(0.0)
        .padding(8.0)
        .gap(3.0)
        .radius(9.0)
        .gradient(base.mix(tint, glow), ink("#1a231e").mix(tint, glow * 0.6))
        .border_gradient(
            if selected { 1.5 } else { 1.0 },
            if selected || playing {
                tint
            } else {
                ink("#49584d")
            },
            if selected {
                tint.mix(base, 0.4)
            } else {
                ink("#111b14")
            },
        )
        .shadow(3.0, 5.0, ink("#080e0a"))
        .shadow(
            0.0,
            if playing { 15.0 } else { 0.0 },
            tint.mix(rune::Color::TRANSPARENT, 0.90),
        )
        .hover(base.mix(tint, 0.12))
        .child(
            row()
                .justify_between()
                .center_y()
                .child(
                    row()
                        .gap(5.0)
                        .center_y()
                        .child(led(playing || flash > 0.0, tint))
                        .child(caption(format!("{:02}", index + 1)).color(ink("#9bada0")))
                        .child(
                            text(spec.label)
                                .font(TextStyle::new(14.0, 750.0))
                                .color(tint),
                        )
                        .children(pedals::tag(s, index)),
                )
                .child(
                    el().size(19.0, 17.0)
                        .center()
                        .radius(3.0)
                        .fill(ink("#111b15"))
                        .border(1.0, ink("#3f5043"))
                        .child(
                            caption(spec.key.to_ascii_uppercase().to_string())
                                .color(ink("#cbd7c7")),
                        ),
                ),
        )
        .child(waveform(
            s.pad_wave(index),
            tint.mix(base, if playing { 0.05 } else { 0.45 }),
            false,
        ))
        .child(
            row()
                .justify_between()
                .child(
                    caption(if let Some(reason) = missing {
                        let short = reason.split_once(": ").map_or(reason, |(_, reason)| reason);
                        format!("MISSING · {:+.0} st · {short}", s.settings[index].pitch)
                    } else {
                        format!("{:+.0} ST", s.settings[index].pitch)
                    })
                    .truncate(),
                )
                .child(
                    caption(if playing {
                        format!("▶ {}", s.counts[index])
                    } else {
                        String::new()
                    })
                    .key(format!("pad-{index}-voices"))
                    .color(tint),
                ),
        );
    if let Some(reason) = missing {
        pad = pad.help(reason).disabled(true);
    } else {
        pad = pad.on_drag(move |s: &mut State, d: &mut Drag, tx| {
            if d.phase == Phase::Down {
                s.trigger(index, 1.0 - 0.6 * (d.y / d.h).clamp(0.0, 1.0));
                tx.wake_after(Duration::from_millis(1));
            } else if d.phase == Phase::Up && d.cancelled() {
                // Rune consumes Escape to cancel a held pointer before window keys.
                s.output.send(Command::Panic);
                tx.wake_after(Duration::from_millis(1));
            }
        });
    }
    pad
}
