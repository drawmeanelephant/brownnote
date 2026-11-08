use super::{State, apply, refresh, retry, status, system_default};
use crate::ui::{button, caption, ink};
use cpal::SupportedBufferSize;
use rune::{El, TextStyle, col, row, text_input};

pub fn header(s: &State) -> El {
    button(
        "output-toggle",
        if s.devices.visible {
            "BACK TO PADS"
        } else {
            "AUDIO OUTPUT"
        },
    )
    .label(if s.devices.visible {
        "Close audio output settings"
    } else {
        "Open audio output settings"
    })
    .on_press(|s: &mut State| {
        s.devices.visible = !s.devices.visible;
        if s.devices.visible {
            refresh(s);
        }
    })
}

/// Replaces the pads/editor area while open, rather than shrinking playable
/// pads below their minimum height. Every control participates in Tab order.
pub fn panel(s: &State) -> El {
    let busy = s.devices.busy();
    let status = status(s);
    let mut body = col()
        .key("output-panel")
        .role("group")
        .label("Machine audio output settings")
        .grow(1.0)
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .padding(12.0)
        .gap(8.0)
        .fill(ink("#101b16"))
        .child(caption("AUDIO OUTPUT · THIS MACHINE ONLY"))
        .child(
            caption(&status)
                .wrap()
                .key("output-status")
                .role("status")
                .label(status),
        );
    if let Some(notice) = &s.devices.notice {
        body = body.child(
            caption(notice)
                .wrap()
                .key("output-notice")
                .role("status")
                .label(notice),
        );
    }
    if let Some(error) = &s.devices.error {
        body = body.child(
            caption(error)
                .wrap()
                .key("output-error")
                .role("alert")
                .label(error)
                .color(ink("#ffad92")),
        );
    }
    if let Some(error) = &s.devices.preference_error {
        body = body.child(
            caption(error)
                .wrap()
                .key("output-preference-error")
                .role("alert")
                .label(error)
                .color(ink("#ffad92")),
        );
    }
    let mut devices = col().key("output-devices").gap(4.0);
    let default_selected = s.devices.draft.device_id.is_none();
    devices = devices.child(
        button(
            "output-device-default",
            "System default (follow system choice)",
        )
        .role("radio")
        .checked(default_selected)
        .focus_selected(default_selected)
        .disabled(busy)
        .focusable(!busy)
        .on_press(|s: &mut State| s.devices.select_device(None)),
    );
    if let Some(discovery) = &s.devices.discovery {
        for (index, device) in discovery.devices.iter().enumerate() {
            let id = device.id.clone();
            let selected = s.devices.draft.device_id.as_ref() == Some(&id);
            // Display names are not unique. Show the host-qualified ID locally;
            // never use a name or list position to reopen a device.
            let title = format!("{} · {}", device.name, device.id);
            devices = devices.child(
                button(&format!("output-device-{index}"), &title)
                    .role("radio")
                    .checked(selected)
                    .focus_selected(selected)
                    .disabled(busy)
                    .focusable(!busy)
                    .on_press(move |s: &mut State| s.devices.select_device(Some(id.clone()))),
            );
        }
    }
    body = body
        .child(
            devices
                .role("radiogroup")
                .focus_group()
                .label("Output device"),
        )
        .child(button("output-refresh", "REFRESH OUTPUTS").on_press(refresh));
    if let Some(discovery) = &s.devices.discovery {
        match s
            .devices
            .draft
            .resolve(&discovery.devices, discovery.default_id.as_ref())
        {
            Ok(device) => {
                body = body.child(
                    caption(format!(
                        "Device defaults: {} Hz · {} ch · {:?}",
                        device.default_config.sample_rate(),
                        device.default_config.channels(),
                        device.default_config.sample_format()
                    ))
                    .key("output-defaults"),
                );
                for (index, config) in device.supported_configs.iter().enumerate() {
                    let buffer = match config.buffer_size() {
                        SupportedBufferSize::Unknown => "unknown bounds; use default buffer".into(),
                        SupportedBufferSize::Range { min, max } => {
                            format!("buffer request {min}–{max} frames")
                        }
                    };
                    body = body.child(
                        caption(format!(
                            "{} ch · {}–{} Hz · {:?} · {buffer}",
                            config.channels(),
                            config.min_sample_rate(),
                            config.max_sample_rate(),
                            config.sample_format()
                        ))
                        .key(format!("output-supported-{index}")),
                    );
                }
            }
            Err(error) => {
                body = body.child(
                    caption(error.to_string())
                        .wrap()
                        .key("output-missing")
                        .role("alert")
                        .label(error.to_string()),
                );
            }
        }
    }
    let style = TextStyle::new(12.0, 500.0);
    let field = |key: &str,
                 label: &str,
                 edit: &rune::TextEdit,
                 access: fn(&mut State) -> &mut rune::TextEdit| {
        col().w(170.0).gap(4.0).child(caption(label)).child(
            text_input(edit, style)
                .key(key)
                .label(label)
                .single_line()
                .focusable(!busy)
                .disabled(busy)
                .on_edit(access)
                .h(32.0)
                .px(8.0)
                .fill(ink("#28382f")),
        )
    };
    body.child(caption("Blank fields use device defaults. Format follows the device default."))
        .child(row().gap(8.0)
            .child(field("output-rate", "Sample rate (Hz)", &s.devices.rate,
                |s| &mut s.devices.rate))
            .child(field("output-channels", "Channels", &s.devices.channels,
                |s| &mut s.devices.channels))
            .child(field("output-buffer", "Buffer request (frames)", &s.devices.buffer,
                |s| &mut s.devices.buffer)))
        .child(caption("A buffer request is not an observed callback size or a latency guarantee."))
        .child(row().gap(8.0)
            .child(button("output-apply", "APPLY OUTPUT").disabled(busy).focusable(!busy).on_press(apply))
            .child(button("output-retry", "RETRY REMEMBERED").disabled(busy).focusable(!busy).on_press(retry))
            .child(button("output-fallback", "USE SYSTEM DEFAULT").disabled(busy).focusable(!busy).on_press(system_default)))
        .child(caption("Applying or retrying stops playing voices. Retry uses the last attempted output, not unsaved edits."))
}
