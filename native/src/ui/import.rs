//! Typed-path controls for the existing import worker. Mounted by the shared view.
#[cfg(test)]
#[path = "import/tests.rs"]
mod tests;

use crate::ui::{State, button, caption, ink, media::ImportStatus};
use brownnote_native::{import::StartError, kit::PADS};
use rune::{El, Key, KeyEvent, TextEdit, TextStyle, Tx, col, row, text_input};
use std::{path::PathBuf, time::Duration};

#[derive(Default)]
pub struct Controls {
    pub visible: bool,
    pub path: TextEdit,
    target: usize,
    error: Option<String>,
}

pub fn header(s: &State) -> El {
    button("import-toggle", "IMPORT WAV")
        .h(16.0)
        .px(7.0)
        .label("Import WAV sample for the selected pad")
        .checked(s.media.controls.visible)
        .help("Import a typed local WAV path · Command-I")
        .on_press(|s: &mut State| {
            if s.media.controls.visible {
                close(s);
            } else {
                open(s);
            }
        })
}

fn open(s: &mut State) {
    s.media.controls.visible = true;
    s.media.controls.target = s.selected;
    s.media.controls.error = None;
}

fn close(s: &mut State) {
    s.cancel_import(s.media.controls.target);
    s.media.controls.visible = false;
    s.media.controls.error = None;
}

fn select(s: &mut State, pad: usize) {
    s.cancel_import(s.media.controls.target);
    s.selected = pad;
    s.media.controls.target = pad;
    s.media.controls.error = None;
}

fn submit(s: &mut State, tx: &mut Tx) {
    poll(s);
    let pad = s.media.controls.target;
    if tx.key_repeat() || s.media.importing(pad) {
        return;
    }
    let path = &s.media.controls.path.text;
    if path.is_empty() {
        s.media.controls.error = Some("Enter a local WAV path first.".into());
        return;
    }
    // This is a literal filesystem path, not shell syntax: spaces, quotes and
    // leading/trailing whitespace are not stripped or expanded.
    match s.import_sample(pad, Some(PathBuf::from(path))) {
        Ok(()) => s.media.controls.error = None,
        Err(StartError::Busy) => {
            s.media.controls.error =
                Some("Import worker busy. Wait for the current read, then try again.".into());
        }
        Err(StartError::WorkerStopped) => {
            s.media.controls.error =
                Some("Import worker unavailable. Restart to try again.".into());
        }
    }
    tx.wake_after(Duration::from_millis(1));
}

/// Call before media::poll so an externally changed selection cancels its old ticket.
pub fn poll(s: &mut State) -> bool {
    if s.media.controls.visible && s.media.controls.target != s.selected {
        select(s, s.selected);
        return true;
    }
    false
}

/// Runs before the performance key handler. Text editing is handled by Rune first.
pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if event.is_command('i') {
        if !tx.key_repeat() {
            if s.media.controls.visible {
                close(s);
            } else {
                open(s);
            }
        }
        return true;
    }
    if !s.media.controls.visible {
        return false;
    }
    if event.key == Key::Escape {
        close(s);
        return true;
    }
    // A pad letter passed on by a focused button must not play/change targets.
    matches!(&event.key, Key::Text(_))
        && !event.mods.cmd
        && !event.mods.ctrl
        && !event.mods.alt
        && !event.mods.logo
}

fn feedback(s: &State) -> (String, bool) {
    let pad = s.media.controls.target;
    if let Some(error) = &s.media.controls.error {
        return (error.clone(), true);
    }
    match s.media.import_status(pad) {
        ImportStatus::Idle => (
            "Choose a path. The current sound stays playable.".into(),
            false,
        ),
        ImportStatus::Importing => (
            "IMPORTING · reading, decoding and owning media off the UI thread.".into(),
            false,
        ),
        ImportStatus::Waiting => (
            "WAITING · media prepared; waiting for a free audio slot or command queue.".into(),
            false,
        ),
        ImportStatus::Success(label) => (
            format!(
                "IMPORTED · {label} · pad {:02}. Ready for the next hit.",
                pad + 1
            ),
            false,
        ),
        ImportStatus::Error(error) => (
            format!("IMPORT FAILED · {error} · current sound kept."),
            true,
        ),
        ImportStatus::Cancelled => (
            "CANCELLED · current sound kept. You can try another path.".into(),
            false,
        ),
    }
}

pub fn panel(s: &State) -> El {
    let controls = &s.media.controls;
    let pad = controls.target;
    let busy = matches!(
        s.media.import_status(pad),
        ImportStatus::Importing | ImportStatus::Waiting
    );
    let (message, error) = feedback(s);
    col()
        .key("import-panel")
        .role("dialog")
        .label("Import WAV sample")
        .modal(false, close)
        .focus_return("import-toggle")
        .grow(1.0)
        .min_h(0.0)
        .padding(12.0)
        .gap(8.0)
        .radius(10.0)
        .gradient(ink("#28382f"), ink("#15231b"))
        .border(1.0, ink("#53684e"))
        .child(caption("YOUR SOUND / IMPORT WAV").color(ink("#c8f77b")))
        .child(
            row()
                .gap(10.0)
                .center_y()
                .child(
                    caption(format!("TARGET {:02} / {}", pad + 1, PADS[pad].label))
                        .key("import-target")
                        .role("status")
                        .label(format!("Import target pad {:02}, {}", pad + 1, PADS[pad].label)),
                )
                .child(button("import-previous", "PREVIOUS PAD").on_press(|s: &mut State| {
                    select(s, (s.media.controls.target + 15) % 16);
                }))
                .child(button("import-next", "NEXT PAD").on_press(|s: &mut State| {
                    select(s, (s.media.controls.target + 1) % 16);
                })),
        )
        .child(
            caption(format!("CURRENT · {}", s.pad_name(pad)))
                .key("import-current")
                .truncate(),
        )
        .child(caption("LOCAL WAV PATH · paste or type the path, then press Return"))
        .child(
            text_input(&controls.path, TextStyle::new(14.0, 500.0))
                .key("import-path")
                .label("Local WAV file path")
                .description("Literal path, including spaces. No shell quotes or tilde expansion.")
                .single_line()
                .on_edit(|s: &mut State| &mut s.media.controls.path)
                .on_input_change(|s: &mut State, _| s.media.controls.error = None)
                .on_input_key(|s: &mut State, event, _| {
                    if event.key == Key::Escape {
                        close(s);
                        true
                    } else {
                        false
                    }
                })
                .on_submit(submit)
                .disabled(busy)
                .focusable(!busy)
                .h(38.0)
                .min_w(0.0)
                .px(10.0)
                .radius(5.0)
                .fill(ink("#0e1a13"))
                .border(1.0, ink(if error { "#ffad92" } else { "#617458" })),
        )
        .child(caption("Use the path itself, without shell escaping. Spaces are preserved.").wrap())
        .child(
            row().gap(8.0)
                .child(
                    button("import-submit", if busy { "IMPORTING…" } else { "IMPORT TO PAD" })
                        .disabled(busy || controls.path.text.is_empty())
                        .focusable(!busy && !controls.path.text.is_empty())
                        .on_press_tx(submit),
                )
                .child(
                    button("import-cancel", if busy { "CANCEL IMPORT" } else { "CANCEL / CLOSE" })
                        .on_press(close),
                ),
        )
        .child(
            caption(&message)
                .key("import-status")
                .role(if error { "alert" } else { "status" })
                .label(&message)
                .help(&message)
                .color(ink(if error { "#ffad92" } else { "#c8f77b" }))
                .wrap()
                .max_lines(2),
        )
        .child(rune::el().grow(1.0).min_h(0.0))
        .child(caption("WAV · PCM16 / FLOAT32 · MONO / STEREO · 8–192 kHz · 1 FRAME–30 SECONDS").wrap())
        .child(caption("Replaces this pad only. Playing voices keep their sound; the next hit uses the import.").wrap())
        .child(caption("Unsaved session media. Project saving is not available here yet.").wrap())
}

pub enum SceneKind {
    Path,
    Busy,
    Error,
    Success,
}

/// Presentation fixtures only: scenes never read paths or start import work.
pub fn scene(s: &mut State, kind: SceneKind) {
    open(s);
    s.media.controls.path = TextEdit::new("/sample library/My sound.wav");
    s.media.status[s.selected] = match kind {
        SceneKind::Path => ImportStatus::Idle,
        SceneKind::Busy => ImportStatus::Importing,
        SceneKind::Error => ImportStatus::Error("My sound.wav: unsupported WAV encoding".into()),
        SceneKind::Success => ImportStatus::Success("My sound.wav".into()),
    };
}
