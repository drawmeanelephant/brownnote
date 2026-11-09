//! Selected-event controls. The integrator supplies access to `State.edit`
//! and the recording lane's single `Takes` registry; this module owns neither
//! a second registry nor a second history.
#![allow(dead_code)] // Removed when the integrator mounts the panel and poll.

#[cfg(test)]
mod tests;

use super::{button, caption, ink};
use brownnote_native::{
    edit::{EditError, EditModel},
    performance::{EventId, Note, Quantization, TakeId, Velocity},
    takes::{Take, Takes},
};
use rune::{Drag, El, Phase, col, el, row};

/// The accessors let the lane compile before the shared State fields land.
/// For State, return `(&s.edit, &s.takes)` and their mutable counterparts.
pub type Read<S> = fn(&S) -> (&Editor, &Takes);
pub type Access<S> = fn(&mut S) -> (&mut Editor, &mut Takes);

#[derive(Default)]
pub struct Editor {
    pub model: EditModel,
    pub selected_event: Option<EventId>,
    pub notice: Option<String>,
    pub error: Option<String>,
    velocity_drag: Option<EventId>,
}

#[derive(Clone, Copy)]
enum Action {
    Take(TakeId),
    Event(EventId),
    Quantize,
    Nudge(i64),
    Delete,
    Velocity(i16),
    Mute,
    Remove,
    Undo,
    Redo,
}

fn selected_take(takes: &Takes) -> Option<&Take> {
    takes
        .selected()
        .and_then(|id| takes.take(id))
        .filter(|take| take.removal().is_none())
}

fn selected_note<'a>(edit: &Editor, takes: &'a Takes) -> Option<&'a Note> {
    let id = edit.selected_event?;
    selected_take(takes)?
        .visible_notes(takes.loop_bars())
        .find(|note| note.on().id == id)
}

fn velocity(edit: &Editor, note: &Note) -> Velocity {
    edit.model
        .event_edit(note.on().id)
        .velocity
        .unwrap_or(note.on().velocity)
}

impl Editor {
    fn report<T>(&mut self, result: Result<T, EditError>, notice: &str) {
        match result {
            Ok(_) => {
                self.error = None;
                self.notice = Some(notice.into());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    /// Reconcile external recording/selection/loop changes without recording
    /// navigation in history. Deleted events remain inspectable for undo.
    fn sync(&mut self, takes: &mut Takes) -> bool {
        let mut changed = false;
        if self.velocity_drag.is_some()
            && (self.velocity_drag != self.selected_event || selected_note(self, takes).is_none())
        {
            self.finish_drag(takes, true);
            changed = true;
        }
        let first = takes
            .takes()
            .find(|take| take.removal().is_none())
            .map(Take::id);
        if selected_take(takes).is_none()
            && let Some(id) = first
        {
            let result = takes.select(id).map_err(EditError::from);
            if let Err(error) = result {
                self.error = Some(error.to_string());
            }
            changed = true;
        }
        if selected_note(self, takes).is_none() {
            let next = selected_take(takes)
                .and_then(|take| take.visible_notes(takes.loop_bars()).next())
                .map(|note| note.on().id);
            changed |= next != self.selected_event;
            self.selected_event = next;
        }
        changed
    }

    fn act(&mut self, takes: &mut Takes, action: Action) {
        self.sync(takes);
        // A keyboard activation during a pointer gesture must not join it.
        if self.model.drag_open() {
            return;
        }
        let take = selected_take(takes).map(Take::id);
        let event = selected_note(self, takes).map(|note| note.on().id);
        let editable = event.filter(|id| !self.model.event_edit(*id).deleted);
        let (result, notice) = match (action, take, editable) {
            (Action::Take(id), _, _) => {
                (takes.select(id).map_err(EditError::from), "Take selected")
            }
            (Action::Event(id), _, _) => {
                if selected_take(takes).is_some_and(|take| {
                    take.visible_notes(takes.loop_bars())
                        .any(|note| note.on().id == id)
                }) {
                    self.selected_event = Some(id);
                    (Ok(()), "Event selected")
                } else {
                    (Err(EditError::UnknownEvent(id)), "Event selected")
                }
            }
            (Action::Undo, _, _) => (self.model.undo(takes).map(|_| ()), "Undid edit"),
            (Action::Redo, _, _) => (self.model.redo(takes).map(|_| ()), "Redid edit"),
            (Action::Quantize, Some(take), _) => (
                self.model.toggle_quantization(takes, take).map(|_| ()),
                "Quantization toggled; original times retained",
            ),
            (Action::Mute, Some(take), _) => {
                let muted = selected_take(takes).is_some_and(Take::muted);
                (
                    self.model.set_muted(takes, take, !muted),
                    if muted { "Take unmuted" } else { "Take muted" },
                )
            }
            (Action::Remove, Some(take), _) => (
                self.model.remove_take(takes, take),
                "Take removed; Undo restores it",
            ),
            (Action::Nudge(delta), _, Some(event)) => (
                self.model.nudge(takes, event, delta),
                "Event nudged; original time retained",
            ),
            (Action::Delete, _, Some(event)) => (
                self.model.delete(takes, event),
                "Event deleted; Undo restores it",
            ),
            (Action::Velocity(delta), _, Some(_)) => {
                let Some(note) = selected_note(self, takes) else {
                    return;
                };
                let current = velocity(self, note);
                let value = (i16::from(current.value()) + delta).clamp(1, 127) as u8;
                if value == current.value() {
                    return;
                }
                (
                    Velocity::try_from(value)
                        .map_err(EditError::from)
                        .and_then(|value| self.model.set_velocity(takes, note.on().id, value)),
                    "Velocity changed",
                )
            }
            _ => return,
        };
        self.report(result, notice);
        self.sync(takes);
    }

    fn set_drag_velocity(&mut self, takes: &mut Takes, value: f32) {
        self.sync(takes);
        let Some(id) = self.velocity_drag else { return };
        let value = value.round().clamp(1.0, 127.0) as u8;
        if selected_note(self, takes).is_some_and(|note| velocity(self, note).value() == value) {
            return;
        }
        let result = Velocity::try_from(value)
            .map_err(EditError::from)
            .and_then(|value| self.model.set_velocity(takes, id, value));
        if result.is_err() {
            self.finish_drag(takes, true);
        }
        self.report(result, "Dragging velocity; Escape cancels");
    }

    fn finish_drag(&mut self, takes: &mut Takes, cancelled: bool) {
        if self.velocity_drag.take().is_none() {
            return;
        }
        let result = if cancelled {
            self.model.cancel_drag(takes)
        } else {
            self.model.end_drag()
        };
        self.report(
            result,
            if cancelled {
                "Velocity drag cancelled"
            } else {
                "Velocity changed"
            },
        );
    }
}

/// Call from the app's `poll` with access to State.edit and its Takes owner.
pub fn poll<S>(s: &mut S, access: Access<S>) -> bool {
    let (edit, takes) = access(s);
    edit.sync(takes)
}

fn action_button<S: 'static>(
    key: &str,
    title: &str,
    enabled: bool,
    action: Action,
    access: Access<S>,
) -> El {
    button(key, title)
        .disabled(!enabled)
        .focusable(enabled)
        .on_press(move |s: &mut S| {
            let (edit, takes) = access(s);
            edit.act(takes, action);
        })
}

/// The fragment takes the app State through `read`, without requiring the
/// feature lane to add shared fields. All actions use the live mutable State.
pub fn panel<S: 'static>(s: &S, read: Read<S>, access: Access<S>) -> El {
    let (edit, takes) = read(s);
    let take = selected_take(takes);
    let note = selected_note(edit, takes);
    let idle = !edit.model.drag_open();
    let can_edit = idle && note.is_some_and(|n| !edit.model.event_edit(n.on().id).deleted);
    let take_ids: Vec<_> = takes
        .takes()
        .filter(|t| t.removal().is_none())
        .map(Take::id)
        .collect();
    let event_ids: Vec<_> = take
        .into_iter()
        .flat_map(|t| t.visible_notes(takes.loop_bars()))
        .map(|n| n.on().id)
        .collect();
    let mut body = col().key("edit-panel").role("group").label("Take and event edits")
        .grow(1.0).min_h(0.0).padding(12.0).gap(8.0)
        .scroll(rune::ScrollSpec { stick_bottom: false })
        .fill(ink("#101b16"))
        .child(caption("TAKE / EVENT EDITS"))
        .child(caption("Tab to a control; Space activates. Velocity: arrows, Home/End, or drag; Escape cancels.").wrap());
    let selection = match (take, note) {
        (Some(take), Some(note)) => format!(
            "Take {} · event {} · note {} · original {} ns · nudge {:+} ns · velocity {}{}{}",
            take.id().0,
            note.on().id.sequence,
            note.on().key.pitch.value(),
            note.on().original_time.nanos(),
            edit.model.event_edit(note.on().id).nudge_nanos,
            velocity(edit, note).value(),
            if take.muted() { " · MUTED" } else { "" },
            if edit.model.event_edit(note.on().id).deleted {
                " · DELETED"
            } else {
                ""
            },
        ),
        (Some(take), None) => format!("Take {} · no events in this loop", take.id().0),
        _ => "No committed takes. Record a take to edit, or Undo a removal.".into(),
    };
    body = body.child(
        caption(&selection)
            .wrap()
            .key("edit-selection")
            .role("status")
            .label(selection),
    );
    let mut navigation = row().gap(8.0);
    for (key, title, id) in [
        (
            "edit-take-prev",
            "PREVIOUS TAKE",
            previous(&take_ids, takes.selected()),
        ),
        (
            "edit-take-next",
            "NEXT TAKE",
            next(&take_ids, takes.selected()),
        ),
    ] {
        navigation = navigation.child(match id {
            Some(id) => action_button(key, title, idle, Action::Take(id), access),
            None => button(key, title).disabled(true).focusable(false),
        });
    }
    body = body.child(navigation);
    let mut navigation = row().gap(8.0);
    for (key, title, id) in [
        (
            "edit-event-prev",
            "PREVIOUS EVENT",
            previous(&event_ids, edit.selected_event),
        ),
        (
            "edit-event-next",
            "NEXT EVENT",
            next(&event_ids, edit.selected_event),
        ),
    ] {
        navigation = navigation.child(match id {
            Some(id) => action_button(key, title, idle, Action::Event(id), access),
            None => button(key, title).disabled(true).focusable(false),
        });
    }
    let quantized =
        take.is_some_and(|t| edit.model.quantization(t.id()) == Quantization::Sixteenth);
    let muted = take.is_some_and(Take::muted);
    body = body.child(navigation)
        .child(row().gap(8.0)
            .child(action_button("edit-quantize", if quantized { "QUANTIZE 1/16: ON" } else { "QUANTIZE 1/16: OFF" }, idle && take.is_some(), Action::Quantize, access).role("checkbox").checked(quantized))
            .child(action_button("edit-mute", if muted { "TAKE MUTED" } else { "MUTE TAKE" }, idle && take.is_some(), Action::Mute, access).role("checkbox").checked(muted))
            .child(action_button("edit-remove", "REMOVE TAKE", idle && take.is_some(), Action::Remove, access)))
        .child(caption("Nudge moves 1 ms before the optional 1/16 quantization. Original times never change.").wrap())
        .child(row().gap(8.0)
            .child(action_button("edit-left", "NUDGE LEFT 1 ms", can_edit, Action::Nudge(-1_000_000), access))
            .child(action_button("edit-right", "NUDGE RIGHT 1 ms", can_edit, Action::Nudge(1_000_000), access))
            .child(action_button("edit-delete", "DELETE EVENT", can_edit, Action::Delete, access)));
    if let Some(note) = note {
        body = body.child(velocity_slider(edit, note, can_edit, access));
    }
    let history = format!(
        "Undo {} / 100 · Redo {} · history clears on quit",
        edit.model.undo_depth(),
        edit.model.redo_depth()
    );
    body = body
        .child(
            row()
                .gap(8.0)
                .child(action_button(
                    "edit-undo",
                    "UNDO",
                    idle && edit.model.can_undo(),
                    Action::Undo,
                    access,
                ))
                .child(action_button(
                    "edit-redo",
                    "REDO",
                    idle && edit.model.can_redo(),
                    Action::Redo,
                    access,
                )),
        )
        .child(
            caption(&history)
                .wrap()
                .key("edit-history")
                .role("status")
                .label(history),
        );
    if let Some(notice) = &edit.notice {
        body = body.child(
            caption(notice)
                .wrap()
                .key("edit-status")
                .role("status")
                .label(notice),
        );
    }
    if let Some(error) = &edit.error {
        body = body.child(
            caption(error)
                .wrap()
                .key("edit-error")
                .role("alert")
                .label(error)
                .color(ink("#ffad92")),
        );
    }
    body
}

fn previous<T: Copy + PartialEq>(ids: &[T], selected: Option<T>) -> Option<T> {
    ids.iter()
        .position(|id| Some(*id) == selected)?
        .checked_sub(1)
        .map(|i| ids[i])
}

fn next<T: Copy + PartialEq>(ids: &[T], selected: Option<T>) -> Option<T> {
    ids.get(ids.iter().position(|id| Some(*id) == selected)? + 1)
        .copied()
}

fn velocity_slider<S: 'static>(edit: &Editor, note: &Note, enabled: bool, access: Access<S>) -> El {
    let value = velocity(edit, note).value();
    let normalized = f32::from(value - 1) / 126.0;
    let (w, h, k) = (220.0, 28.0, 14.0);
    let travel = w - k;
    let to_value = move |x: f32| 1.0 + ((x - k / 2.0) / travel).clamp(0.0, 1.0) * 126.0;
    // Keep the captured slider enabled until its Up/cancel event arrives.
    let dragging = edit.velocity_drag == Some(note.on().id);
    col()
        .gap(6.0)
        .child(caption(format!("VELOCITY {value} / 127")))
        .child(
            row()
                .gap(8.0)
                .child(action_button(
                    "edit-velocity-down",
                    "VELOCITY DOWN",
                    enabled && value > 1,
                    Action::Velocity(-1),
                    access,
                ))
                .child(action_button(
                    "edit-velocity-up",
                    "VELOCITY UP",
                    enabled && value < 127,
                    Action::Velocity(1),
                    access,
                )),
        )
        .child(
            el().key("edit-velocity")
                .role("slider")
                .label("Event velocity")
                .size(w, h)
                .range_value(f64::from(value), 1.0, 127.0)
                .range_step(1.0)
                .a11y_value(value.to_string())
                .on_step(move |s: &mut S, steps, _| {
                    let (edit, takes) = access(s);
                    edit.act(takes, Action::Velocity(steps.clamp(-126, 126) as i16));
                })
                .on_step_limit(move |s: &mut S, end, _| {
                    let (edit, takes) = access(s);
                    edit.act(takes, Action::Velocity(if end { 126 } else { -126 }));
                })
                .focusable(enabled || dragging)
                .disabled(!enabled && !dragging)
                .on_drag(move |s: &mut S, d: &mut Drag, tx| {
                    let (edit, takes) = access(s);
                    match d.phase {
                        Phase::Down => {
                            edit.sync(takes);
                            let Some(note) = selected_note(edit, takes) else {
                                return;
                            };
                            if edit.model.drag_open() || edit.model.event_edit(note.on().id).deleted
                            {
                                return;
                            }
                            let id = note.on().id;
                            d.data = f32::from(velocity(edit, note).value());
                            match edit.model.begin_drag() {
                                Ok(()) => edit.velocity_drag = Some(id),
                                Err(error) => {
                                    edit.error = Some(error.to_string());
                                    return;
                                }
                            }
                            let knob = k / 2.0 + (d.data - 1.0) / 126.0 * travel;
                            if (d.x - knob).abs() > k / 2.0 + 2.0 {
                                edit.set_drag_velocity(takes, to_value(d.x));
                            }
                        }
                        Phase::Move => {
                            let knob = k / 2.0 + (d.data - 1.0) / 126.0 * travel;
                            let grab = if (d.start_x - knob).abs() <= k / 2.0 + 2.0 {
                                d.start_x - knob
                            } else {
                                0.0
                            };
                            edit.set_drag_velocity(takes, to_value(d.x - grab));
                            tx.instant();
                        }
                        Phase::Up => {
                            let unchanged = selected_note(edit, takes).is_some_and(|note| {
                                f32::from(velocity(edit, note).value()) == d.data
                            });
                            edit.finish_drag(takes, d.cancelled() || unchanged);
                        }
                    }
                })
                .child(
                    el().size(w, 5.0)
                        .absolute(0.0, 11.5)
                        .fill(ink("#394a3c"))
                        .a11y_hidden(),
                )
                .child(
                    el().size(k, h)
                        .absolute(normalized * travel, 0.0)
                        .radius(4.0)
                        .fill(ink("#c8f77b"))
                        .a11y_hidden(),
                ),
        )
}

pub enum SceneKind {
    Empty,
    Selected,
    Muted,
    Removed,
    Error,
}

/// Synthetic, silent scenes. Only call with a scene/test registry, not a
/// musician's project: this replaces both the registry and the edit history.
pub fn scene(edit: &mut Editor, takes: &mut Takes, kind: SceneKind) -> Result<(), EditError> {
    use brownnote_native::{
        performance::{Channel, LoopBars, NoteKey, OriginalTime, Pitch, SourceId, Target},
        takes::PassOptions,
    };
    *edit = Editor::default();
    *takes = Takes::new(LoopBars::try_from(8)?, TakeId(700));
    if matches!(kind, SceneKind::Empty) {
        return Ok(());
    }
    let key = NoteKey {
        source: SourceId(7),
        channel: Channel::try_from(0)?,
        pitch: Pitch::try_from(60)?,
    };
    let mut first = None;
    for _ in 0..2 {
        let take = takes.arm(PassOptions::default())?;
        first.get_or_insert(take);
        takes.start(take)?;
        for (start, end, velocity) in [(10_000_000, 50_000_000, 90), (25_000_000, 60_000_000, 100)]
        {
            takes.note_on(
                take,
                Target::Pad(super::media::pad_id(0)),
                key,
                Velocity::try_from(velocity)?,
                OriginalTime::from_nanos(start),
            )?;
            takes.note_off(take, key, OriginalTime::from_nanos(end))?;
        }
        takes.finish(take)?;
    }
    let Some(first) = first else { return Ok(()) };
    takes.select(first)?;
    edit.sync(takes);
    match kind {
        SceneKind::Muted => edit.act(takes, Action::Mute),
        SceneKind::Removed => {
            edit.act(takes, Action::Remove);
            edit.act(takes, Action::Remove);
        }
        SceneKind::Error => edit.act(
            takes,
            Action::Event(EventId {
                take: first,
                sequence: 99,
            }),
        ),
        _ => {}
    }
    Ok(())
}
