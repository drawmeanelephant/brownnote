use super::{
    State, Tab, accent, button, caption,
    control::{Control, slider},
    graphics::{led, logo, meter, waveform},
    ink, pedals,
};
use brownnote_native::{
    engine::Command,
    kit::{FILES, PADS},
};
use rune::{Cx, Drag, El, Phase, TextStyle, col, el, row, text};
use std::time::Duration;

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
    // A pedal adds a tail, and the pad plays the render, so the display has to
    // follow the slot rather than the kit file.
    let (duration, rate) = s.pad_audio(s.selected);
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
                        rate as f32 / 1000.0
                    ))
                    .key("sample-length")
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
