//! The pedal strip, the editor header and the pad-face tag.
use super::{
    CatalogState, Status, add_stage, effect, move_stage, remove_stage, set_mix, set_pot,
    status_text,
};
use crate::ui::{State, accent, button, caption, ink};
use brownnote_native::{
    kit::PADS,
    pedals::{catalog::MixLaw, render::Stage},
};
use rune::{Color, Drag, El, Phase, TextStyle, Tx, col, el, row, text};

fn status_color(status: &Status) -> Color {
    match status {
        Status::Error(_) => ink("#ffad92"),
        Status::Ready { .. } => ink("#c8f77b"),
        _ => ink("#8c9e98"),
    }
}

fn chip(s: &State, pad: usize, index: usize, stage: &Stage) -> El {
    let focused = s.pedals.pads[pad].focus == Some(index);
    let count = s.pedals.pads[pad].chain.stages.len();
    let color = accent(PADS[pad].sample);
    let mut chip = row()
        .key(format!("pedal-{index}"))
        .role("button")
        .label(format!("Pedal {index}: {}", stage.effect))
        .focusable(true)
        .h(18.0)
        .px(6.0)
        .gap(4.0)
        .center_y()
        .radius(3.0)
        .fill(if focused {
            ink("#2c3a32")
        } else {
            ink("#20302a")
        })
        .border(1.0, if focused { color } else { ink("#3b4c42") })
        .on_press(move |s: &mut State| s.pedals.pads[pad].focus = Some(index))
        .child(
            text(stage.effect.clone())
                .font(TextStyle::new(9.5, 650.0))
                .color(if focused { color } else { ink("#cbd7c7") }),
        );
    for (kind, label, enabled) in [("left", "‹", index > 0), ("right", "›", index + 1 < count)]
    {
        chip = chip.child(
            el().key(format!("pedal-{index}-{kind}"))
                .role("button")
                .label(if kind == "left" {
                    "Move earlier".to_string()
                } else {
                    "Move later".to_string()
                })
                .focusable(enabled)
                .disabled(!enabled)
                .size(11.0, 11.0)
                .center()
                .radius(2.0)
                .fill(ink("#101a15"))
                .child(caption(label).color(if enabled {
                    ink("#cbd7c7")
                } else {
                    ink("#4c5a51")
                }))
                .on_press(move |s: &mut State| {
                    let to = if kind == "left" { index - 1 } else { index + 1 };
                    move_stage(s, pad, index, to);
                }),
        );
    }
    chip.child(
        el().key(format!("pedal-{index}-remove"))
            .role("button")
            .label(format!("Remove {}", stage.effect))
            .focusable(true)
            .size(11.0, 11.0)
            .center()
            .radius(2.0)
            .fill(ink("#101a15"))
            .child(caption("×").color(ink("#ffad92")))
            .on_press(move |s: &mut State| remove_stage(s, pad, index)),
    )
}

fn menu_button(s: &mut State, tx: &mut Tx) {
    let Some(catalog) = s.pedals.ready() else {
        return;
    };
    let pad = s.selected;
    let chain = s.pedals.pads[pad].chain.clone();
    let mut menu = rune::PopupMenu::new();
    for effect in catalog.routable() {
        let taken = chain.stages.iter().any(|stage| stage.effect == effect.name);
        let mut item = rune::PopupItem::new(effect.name.clone(), effect.name.clone());
        if taken {
            item = item.enabled(false);
        }
        menu = menu.item(item);
    }
    tx.popup_menu(menu, "pedal-add", move |s: &mut State, picked, _| {
        if let Some(name) = picked {
            add_stage(s, pad, name);
        }
    });
}

fn add_button(s: &State, pad: usize) -> El {
    let full =
        s.pedals.pads[pad].chain.stages.len() >= brownnote_native::pedals::render::MAX_STAGES;
    let mut button = button("pedal-add", "+ PEDAL");
    button = button
        .w(70.0)
        .h(18.0)
        .px(8.0)
        .focusable(!full)
        .disabled(full);
    if full {
        button = button.child(
            text("+ PEDAL")
                .font(TextStyle::new(9.5, 650.0))
                .color(ink("#4c5a51")),
        );
    }
    if rune::popup_menu_supported() {
        button.on_press_tx(menu_button)
    } else {
        button.on_press(move |s: &mut State| s.pedals.add_open = !s.pedals.add_open)
    }
}

fn inline_menu(s: &State) -> El {
    let pad = s.selected;
    let Some(catalog) = s.pedals.ready() else {
        return row();
    };
    let chain = s.pedals.pads[pad].chain.clone();
    let mut list = row().key("pedal-add-list").gap(4.0).h(26.0).scroll_x();
    for effect in catalog.routable() {
        let taken = chain.stages.iter().any(|stage| stage.effect == effect.name);
        let name = effect.name.clone();
        list = list.child(
            el().key(format!("pedal-add-{name}"))
                .role("button")
                .label(name.clone())
                .focusable(!taken)
                .disabled(taken)
                .h(16.0)
                .px(5.0)
                .center()
                .radius(3.0)
                .fill(ink("#20302a"))
                .border(1.0, ink("#3b4c42"))
                .child(caption(name.clone()).color(if taken {
                    ink("#4c5a51")
                } else {
                    ink("#cbd7c7")
                }))
                .on_press(move |s: &mut State| add_stage(s, pad, &name)),
        );
    }
    list
}

fn pot_slider(
    s: &State,
    pad: usize,
    index: usize,
    value: u8,
    max: u8,
    label: &str,
    unit: &str,
) -> El {
    let slug: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let default = s.pedals.ready().and_then(|catalog| {
        let stage = s.pedals.pads[pad].chain.stages.get(index)?;
        let effect = effect(&catalog, &stage.effect)?;
        effect
            .pots
            .iter()
            .find(|pot| pot.label == label)
            .map(|pot| pot.default)
    });
    let display = if max <= 4 {
        format!("{value} / {max}")
    } else if Some(value) == default {
        format!("{value} / {max} {unit} DEF")
    } else {
        format!("{value} / {max} {unit}")
    };
    let normalized = f32::from(value) / f32::from(max);
    let (w, h, k) = (92.0, 14.0, 10.0);
    let travel = w - k;
    let name = label.to_string();
    let to_value =
        move |x: f32| (f32::from(max) * ((x - k / 2.0) / travel).clamp(0.0, 1.0)).round() as u8;
    col()
        .w(w)
        .child(
            caption(name.to_uppercase())
                .key(format!("pot-{slug}-label"))
                .color(ink("#8c9e98"))
                .truncate(),
        )
        .child(
            caption(display.to_uppercase())
                .key(format!("pot-{slug}-value"))
                .color(if Some(value) == default {
                    ink("#7f9187")
                } else {
                    ink("#edf4dc")
                })
                .truncate(),
        )
        .child(
            el().key(format!("pot-{slug}"))
                .role("slider")
                .label(name.clone())
                .focusable(true)
                .range_value(f64::from(value), 0.0, f64::from(max))
                .range_step(1.0)
                .a11y_value(display.clone())
                .on_step({
                    let name = name.clone();
                    move |s: &mut State, steps, _| {
                        let next = (i32::from(value) + steps).clamp(0, i32::from(max));
                        set_pot(s, pad, index, &name, next as u8);
                    }
                })
                .on_step_limit({
                    let name = name.clone();
                    move |s: &mut State, end, _| {
                        set_pot(s, pad, index, &name, if end { max } else { 0 });
                    }
                })
                .size(w, h)
                .on_drag(move |s: &mut State, d: &mut Drag, tx| match d.phase {
                    Phase::Down => {
                        d.data = f32::from(value);
                        let knob = k / 2.0 + normalized * travel;
                        if (d.x - knob).abs() > k / 2.0 + 2.0 {
                            set_pot(s, pad, index, &name, to_value(d.x));
                        }
                    }
                    Phase::Move => {
                        let knob = k / 2.0 + (d.data / f32::from(max)) * travel;
                        let grab = if (d.start_x - knob).abs() <= k / 2.0 + 2.0 {
                            d.start_x - knob
                        } else {
                            0.0
                        };
                        set_pot(s, pad, index, &name, to_value(d.x - grab));
                        tx.instant();
                    }
                    Phase::Up => {
                        if d.cancelled() {
                            set_pot(s, pad, index, &name, d.data as u8);
                        }
                    }
                })
                .child(
                    el().key("track")
                        .size(w, 4.0)
                        .absolute(0.0, 5.0)
                        .radius(2.0)
                        .fill(ink("#101b14"))
                        .border(1.0, ink("#394a3c"))
                        .a11y_hidden(),
                )
                .child(
                    el().key("fill")
                        .size(k / 2.0 + normalized * travel, 2.0)
                        .absolute(0.0, 6.0)
                        .fill(accent(PADS[pad].sample))
                        .a11y_hidden(),
                )
                .child(
                    el().key("knob")
                        .size(k, k)
                        .absolute(normalized * travel, 2.0)
                        .radius(3.0)
                        .gradient(ink("#eef4d8"), ink("#9eac8b"))
                        .border_gradient(1.0, ink("#fcffe8"), ink("#748069"))
                        .a11y_hidden(),
                ),
        )
}

fn mix_slider(pad: usize, index: usize, mix: Option<u8>, law: MixLaw) -> El {
    let value = mix.unwrap_or(0);
    let display = if mix.is_some() {
        format!("{value} / 120")
    } else {
        format!("DEFAULT · {}", law.text().to_uppercase())
    };
    let normalized = f32::from(value) / 120.0;
    let (w, h, k) = (92.0, 14.0, 10.0);
    let travel = w - k;
    let to_value = move |x: f32| (120.0 * ((x - k / 2.0) / travel).clamp(0.0, 1.0)).round() as u8;
    let mut label_row = row()
        .justify_between()
        .child(caption("MIX").key("pedal-mix-label").color(ink("#8c9e98")));
    if mix.is_some() {
        label_row = label_row.child(
            el().key("pedal-mix-reset")
                .role("button")
                .label("Reset mix")
                .focusable(true)
                .center()
                .child(caption("RESET").color(ink("#ffad92")))
                .on_press(move |s: &mut State| set_mix(s, pad, index, None)),
        );
    }
    col()
        .w(w)
        .child(label_row)
        .child(
            caption(display.clone())
                .key("pedal-mix-value")
                .color(if mix.is_some() {
                    ink("#edf4dc")
                } else {
                    ink("#7f9187")
                }),
        )
        .child(
            el().key("pedal-mix")
                .role("slider")
                .label("Mix")
                .focusable(true)
                .range_value(f64::from(value), 0.0, 120.0)
                .range_step(1.0)
                .a11y_value(display)
                .on_step(move |s: &mut State, steps, _| {
                    let base = mix.unwrap_or(0) as i32 + steps;
                    set_mix(s, pad, index, Some(base.clamp(0, 120) as u8));
                })
                .on_step_limit(move |s: &mut State, end, _| {
                    set_mix(s, pad, index, Some(if end { 120 } else { 0 }));
                })
                .size(w, h)
                .on_drag(move |s: &mut State, d: &mut Drag, tx| match d.phase {
                    Phase::Down => {
                        // A negative mark means DEFAULT, which `Some(0)` cannot
                        // stand in for: cancelling has to give it back.
                        d.data = mix.map_or(-1.0, f32::from);
                        let knob = k / 2.0 + normalized * travel;
                        if (d.x - knob).abs() > k / 2.0 + 2.0 {
                            set_mix(s, pad, index, Some(to_value(d.x)));
                        }
                    }
                    Phase::Move => {
                        set_mix(s, pad, index, Some(to_value(d.x)));
                        tx.instant();
                    }
                    Phase::Up => {
                        if d.cancelled() {
                            let original = if d.data < 0.0 {
                                None
                            } else {
                                Some(d.data as u8)
                            };
                            set_mix(s, pad, index, original);
                        }
                    }
                })
                .child(
                    el().key("track")
                        .size(w, 4.0)
                        .absolute(0.0, 5.0)
                        .radius(2.0)
                        .fill(ink("#101b14"))
                        .border(1.0, ink("#394a3c"))
                        .a11y_hidden(),
                )
                .child(
                    el().key("knob")
                        .size(k, k)
                        .absolute(normalized * travel, 2.0)
                        .radius(3.0)
                        .gradient(ink("#eef4d8"), ink("#9eac8b"))
                        .border_gradient(1.0, ink("#fcffe8"), ink("#748069"))
                        .a11y_hidden(),
                ),
        )
}

fn focused_stage(s: &State) -> El {
    let pad = s.selected;
    let Some(catalog) = s.pedals.ready() else {
        return row();
    };
    let Some(index) = s.pedals.pads[pad].focus else {
        return row()
            .key("pedal-pots")
            .h(34.0)
            .center_y()
            .child(caption("ADD A PEDAL TO HEAR IT ON THE NEXT HIT"));
    };
    let Some(stage) = s.pedals.pads[pad].chain.stages.get(index) else {
        return row();
    };
    let Some(effect) = effect(&catalog, &stage.effect) else {
        return row();
    };
    let mut pots = row().key("pedal-pots").gap(10.0).h(34.0).scroll_x();
    for pot in &effect.pots {
        let value = stage
            .pots
            .iter()
            .find(|(label, _)| *label == pot.label)
            .map_or(pot.default, |(_, value)| *value);
        pots = pots.child(pot_slider(
            s, pad, index, value, pot.max, &pot.label, &pot.unit,
        ));
    }
    if effect.mix != MixLaw::None {
        pots = pots.child(mix_slider(pad, index, stage.mix, effect.mix));
    }
    pots
}

/// The strip body: chain chips and the focused stage's controls.
pub fn strip(s: &State) -> El {
    let pad = s.selected;
    if let CatalogState::Unavailable(message) = &s.pedals.catalog {
        return row()
            .key("pedal-message")
            .h(61.0)
            .center_y()
            .child(caption(message.clone()).color(ink("#ffad92")).truncate());
    }
    let chain = &s.pedals.pads[pad].chain;
    let mut chips = row().key("pedal-chain").gap(4.0).h(18.0).scroll_x();
    for (index, stage) in chain.stages.iter().enumerate() {
        chips = chips.child(chip(s, pad, index, stage));
    }
    chips = chips.child(add_button(s, pad));
    let body = if s.pedals.add_open {
        inline_menu(s)
    } else {
        focused_stage(s)
    };
    col().key("pedal-strip").gap(4.0).child(chips).child(body)
}

/// The `PEDALS` tab's view, for the editor header.
pub fn header(s: &State) -> El {
    let spec = &PADS[s.selected];
    let status = &s.pedals.pads[s.selected].status;
    let (message, color) = match &s.pedals.catalog {
        CatalogState::Unavailable(message) => (message.clone(), ink("#ffad92")),
        _ => (status_text(status), status_color(status)),
    };
    row()
        .justify_between()
        .gap(10.0)
        .center_y()
        .child(
            caption(format!("PEDALS {:02}  /  {}", s.selected + 1, spec.label))
                .color(accent(spec.sample)),
        )
        .child(caption(message).key("pedal-status").color(color).truncate())
}

/// A pad's `FX n` tag, drawn on its face when its chain isn't empty.
pub fn tag(s: &State, pad: usize) -> Option<El> {
    let count = s.pedals.pads[pad].chain.stages.len();
    if count == 0 {
        return None;
    }
    let failed = matches!(s.pedals.pads[pad].status, Status::Error(_));
    Some(
        caption(format!("FX {count}"))
            .key(format!("pad-{pad}-fx"))
            .color(if failed {
                ink("#ffad92")
            } else {
                ink("#9fd6b4")
            }),
    )
}
