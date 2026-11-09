//! Reversible edit model over the #104/#106 stable IDs (issue #107, wave 3).
//!
//! [`Takes`] originals are never rewritten (D9). This module keeps small
//! edit-layer overrides that reference stable [`EventId`]/[`TakeId`] values
//! and derives the *effective* performance view consumed by transport and
//! export: quantization is applied per take while deriving that view (D13),
//! and turning it off restores every original time exactly. Take mute and
//! removal go through the owning [`Takes`] registry, so take lifecycle has a
//! single owner.
//!
//! Undo/redo stacks are bounded to [`MAX_UNDO_STEPS`] steps (D15) and live only
//! in memory: history does not survive quitting, only the current edited
//! state is persisted (that persistence is #100's job). One continuous drag is
//! one undo step: operations recorded between [`EditModel::begin_drag`] and
//! [`EditModel::end_drag`] coalesce, and [`EditModel::cancel_drag`] rolls them
//! back, matching the drag-start/Escape precedent in `ui/control.rs`. History
//! entries reference stable IDs only; they never own source or retained media.
//!
//! This is a concrete operation set, not a general command bus. Keyboard/UI
//! wiring is a deliberate second PR.
use crate::performance::{
    EffectiveNote, EventId, Note, NoteLifecycle, NoteOn, OriginalTime, PerformanceError,
    Quantization, TakeId, TakeTiming, Velocity,
};
use crate::takes::{Take, Takes, TakesError};
use std::{collections::BTreeMap, fmt};

/// D15: undo history keeps the last 100 steps.
pub const MAX_UNDO_STEPS: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// No take with this ID exists in the registry.
    UnknownTake(TakeId),
    /// The take exists but is removed; take-level edits need a live take.
    RemovedTake(TakeId),
    /// No committed event with this ID exists in its take.
    UnknownEvent(EventId),
    /// The owning takes registry rejected a lifecycle operation.
    Takes(TakesError),
    /// Deriving an effective note failed (overflow and similar).
    Performance(PerformanceError),
    /// A drag group is already open.
    DragActive,
    /// No drag group is open.
    NoDragActive,
    /// A nudge left the representable nanosecond range.
    NudgeOverflow,
}
impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "edit: {self:?}")
    }
}
impl std::error::Error for EditError {}
impl From<PerformanceError> for EditError {
    fn from(error: PerformanceError) -> Self {
        Self::Performance(error)
    }
}
impl From<TakesError> for EditError {
    fn from(error: TakesError) -> Self {
        Self::Takes(error)
    }
}

/// Current edit-layer state of one event. The stored original is untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventEdit {
    /// Hidden from the effective view; the original note is retained.
    pub deleted: bool,
    /// Effective velocity override, if any.
    pub velocity: Option<Velocity>,
    /// Accumulated onset/end shift in nanoseconds; negative moves earlier.
    pub nudge_nanos: i64,
}

/// Which side of an operation to (re)apply: undo applies `from`, redo `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    From,
    To,
}

/// One reversible primitive. `from`/`to` are the states on each side, so the
/// same entry serves undo and redo. Entries reference stable IDs only.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Quantization {
        take: TakeId,
        from: Quantization,
        to: Quantization,
    },
    Event {
        id: EventId,
        from: EventEdit,
        to: EventEdit,
    },
    Mute {
        take: TakeId,
        from: bool,
        to: bool,
    },
    Remove {
        take: TakeId,
    },
}

impl Op {
    fn value_side<T>(&self, side: Side, from: T, to: T) -> T {
        match side {
            Side::From => from,
            Side::To => to,
        }
    }

    fn is_noop(&self) -> bool {
        match self {
            Op::Quantization { from, to, .. } => from == to,
            Op::Mute { from, to, .. } => from == to,
            Op::Event { from, to, .. } => from == to,
            Op::Remove { .. } => false,
        }
    }

    /// Coalesce a follow-up operation on the same target into this one, so a
    /// drag keeps the drag-start value as `from` and the latest as `to`.
    fn merge(&mut self, other: &Op) -> bool {
        match (self, other) {
            (
                Op::Quantization { take, to, .. },
                Op::Quantization {
                    take: other_take,
                    to: other_to,
                    ..
                },
            ) if take == other_take => {
                *to = *other_to;
                true
            }
            (
                Op::Event { id, to, .. },
                Op::Event {
                    id: other_id,
                    to: other_to,
                    ..
                },
            ) if id == other_id => {
                *to = *other_to;
                true
            }
            (
                Op::Mute { take, to, .. },
                Op::Mute {
                    take: other_take,
                    to: other_to,
                    ..
                },
            ) if take == other_take => {
                *to = *other_to;
                true
            }
            (Op::Remove { take }, Op::Remove { take: other_take }) if take == other_take => true,
            _ => false,
        }
    }

    fn apply(&self, model: &mut EditModel, takes: &mut Takes, side: Side) -> Result<(), EditError> {
        match self {
            Op::Quantization { take, from, to } => {
                let value = self.value_side(side, *from, *to);
                model.set_quantization(*take, value);
            }
            Op::Event { id, from, to } => {
                let value = self.value_side(side, *from, *to);
                model.set_event_edit(*id, value);
            }
            Op::Mute { take, from, to } => {
                let value = self.value_side(side, *from, *to);
                takes.set_muted(*take, value)?;
            }
            Op::Remove { take } => match side {
                Side::To => takes.remove(*take)?,
                Side::From => takes.restore(*take)?,
            },
        }
        Ok(())
    }
}

/// One undo step: a single discrete operation, or all operations of one
/// coalesced drag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Step {
    ops: Vec<Op>,
}

impl Step {
    fn apply(&self, model: &mut EditModel, takes: &mut Takes, side: Side) -> Result<(), EditError> {
        // Undo replays inverses in reverse application order; redo replays
        // forward.
        match side {
            Side::To => {
                for op in &self.ops {
                    op.apply(model, takes, side)?;
                }
            }
            Side::From => {
                for op in self.ops.iter().rev() {
                    op.apply(model, takes, side)?;
                }
            }
        }
        Ok(())
    }

    fn is_noop(&self) -> bool {
        self.ops.iter().all(Op::is_noop)
    }
}

/// Bounded undo/redo history plus the current edit layer over a [`Takes`]
/// registry. Owns no takes, notes, assets, or media: it references stable IDs
/// and keeps small value overrides only. History is in-memory only and does
/// not survive quitting (D15).
#[derive(Debug, Default)]
pub struct EditModel {
    quantization: BTreeMap<TakeId, Quantization>,
    events: BTreeMap<EventId, EventEdit>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    drag: Option<Step>,
}

impl EditModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
    pub fn drag_open(&self) -> bool {
        self.drag.is_some()
    }

    /// D13: quantization is per take and off unless edited here.
    pub fn quantization(&self, take: TakeId) -> Quantization {
        self.quantization.get(&take).copied().unwrap_or_default()
    }

    /// Current edit-layer state of one event (default when untouched).
    pub fn event_edit(&self, id: EventId) -> EventEdit {
        self.events.get(&id).copied().unwrap_or_default()
    }

    /// Toggle D13 quantization for one live take; returns the new setting.
    pub fn toggle_quantization(
        &mut self,
        takes: &Takes,
        take: TakeId,
    ) -> Result<Quantization, EditError> {
        self.require_live_take(takes, take)?;
        let from = self.quantization(take);
        let to = match from {
            Quantization::Off => Quantization::Sixteenth,
            Quantization::Sixteenth => Quantization::Off,
        };
        self.set_quantization(take, to);
        self.record(Op::Quantization { take, from, to });
        Ok(to)
    }

    /// Shift one event's effective onset and end by `delta_nanos` (negative
    /// moves earlier). The stored original time is untouched (D9).
    pub fn nudge(&mut self, takes: &Takes, id: EventId, delta_nanos: i64) -> Result<(), EditError> {
        self.require_event(takes, id)?;
        let mut to = self.event_edit(id);
        to.nudge_nanos = to
            .nudge_nanos
            .checked_add(delta_nanos)
            .ok_or(EditError::NudgeOverflow)?;
        self.edit_event(id, to);
        Ok(())
    }

    /// Hide one event from the effective view; the original note is retained.
    pub fn delete(&mut self, takes: &Takes, id: EventId) -> Result<(), EditError> {
        self.set_deleted(takes, id, true)
    }

    /// Hide or unhide one event. A no-change call records no history step.
    pub fn set_deleted(
        &mut self,
        takes: &Takes,
        id: EventId,
        deleted: bool,
    ) -> Result<(), EditError> {
        self.require_event(takes, id)?;
        let mut to = self.event_edit(id);
        to.deleted = deleted;
        self.edit_event(id, to);
        Ok(())
    }

    /// Override one event's effective velocity. A no-change call records no
    /// history step.
    pub fn set_velocity(
        &mut self,
        takes: &Takes,
        id: EventId,
        velocity: Velocity,
    ) -> Result<(), EditError> {
        self.require_event(takes, id)?;
        let mut to = self.event_edit(id);
        to.velocity = Some(velocity);
        self.edit_event(id, to);
        Ok(())
    }

    /// Mute or unmute a live take through the owning registry; undoable.
    pub fn set_muted(
        &mut self,
        takes: &mut Takes,
        take: TakeId,
        muted: bool,
    ) -> Result<(), EditError> {
        self.require_live_take(takes, take)?;
        let from = takes.take(take).map(Take::muted).unwrap_or(false);
        if from == muted {
            return Ok(());
        }
        takes.set_muted(take, muted)?;
        self.record(Op::Mute {
            take,
            from,
            to: muted,
        });
        Ok(())
    }

    /// Remove a live take through the owning registry. Original notes and any
    /// media stay recoverable; undo restores the take.
    pub fn remove_take(&mut self, takes: &mut Takes, take: TakeId) -> Result<(), EditError> {
        self.require_live_take(takes, take)?;
        takes.remove(take)?;
        self.record(Op::Remove { take });
        Ok(())
    }

    /// Pop the newest step and invert it. Returns false when there is nothing
    /// to undo. Fails while a drag group is open.
    pub fn undo(&mut self, takes: &mut Takes) -> Result<bool, EditError> {
        if self.drag.is_some() {
            return Err(EditError::DragActive);
        }
        let Some(step) = self.undo.pop() else {
            return Ok(false);
        };
        match step.apply(self, takes, Side::From) {
            Ok(()) => {
                self.redo.push(step);
                Ok(true)
            }
            Err(error) => {
                // Ops are validated when recorded and `Takes` never drops
                // takes or events, so replays do not fail in practice; retain
                // the step if one ever does.
                self.undo.push(step);
                Err(error)
            }
        }
    }

    /// Replay the newest undone step. Returns false when there is nothing to
    /// redo. Fails while a drag group is open.
    pub fn redo(&mut self, takes: &mut Takes) -> Result<bool, EditError> {
        if self.drag.is_some() {
            return Err(EditError::DragActive);
        }
        let Some(step) = self.redo.pop() else {
            return Ok(false);
        };
        match step.apply(self, takes, Side::To) {
            Ok(()) => {
                self.push_undo(step);
                Ok(true)
            }
            Err(error) => {
                self.redo.push(step);
                Err(error)
            }
        }
    }

    /// Open a drag group (`ui/control.rs` precedent): every operation recorded
    /// until [`EditModel::end_drag`] or [`EditModel::cancel_drag`] coalesces
    /// into ONE undo step.
    pub fn begin_drag(&mut self) -> Result<(), EditError> {
        if self.drag.is_some() {
            return Err(EditError::DragActive);
        }
        self.drag = Some(Step::default());
        Ok(())
    }

    /// Commit the open drag group as one undo step and invalidate redo (D15).
    /// Returns false when the drag changed nothing.
    pub fn end_drag(&mut self) -> Result<bool, EditError> {
        let Some(drag) = self.drag.take() else {
            return Err(EditError::NoDragActive);
        };
        if drag.is_noop() {
            return Ok(false);
        }
        self.push_undo(drag);
        self.redo.clear();
        Ok(true)
    }

    /// Roll the open drag group back (Escape precedent) without recording a
    /// step and without invalidating redo.
    pub fn cancel_drag(&mut self, takes: &mut Takes) -> Result<bool, EditError> {
        let Some(drag) = self.drag.take() else {
            return Err(EditError::NoDragActive);
        };
        match drag.apply(self, takes, Side::From) {
            Ok(()) => Ok(true),
            Err(error) => {
                self.drag = Some(drag);
                Err(error)
            }
        }
    }

    /// The effective performance view consumed by transport/export: live,
    /// unmuted, in-loop notes with quantization, nudge, delete and velocity
    /// edits applied, sorted by effective onset then event ID.
    pub fn effective_notes(&self, takes: &Takes) -> Result<Vec<EffectiveNote>, EditError> {
        let mut notes = Vec::new();
        for note in takes.audible_notes() {
            let take = takes
                .take(note.on().id.take)
                .ok_or(EditError::UnknownTake(note.on().id.take))?;
            if let Some(effective) = self.effective_note(take, note)? {
                notes.push(effective);
            }
        }
        sort_effective(&mut notes);
        Ok(notes)
    }

    /// Effective notes of one take for inspection/UI, including muted or
    /// removed takes; still bounded by original-onset loop visibility (D11).
    pub fn effective_take_notes(
        &self,
        takes: &Takes,
        take: TakeId,
    ) -> Result<Vec<EffectiveNote>, EditError> {
        let take_ref = takes.take(take).ok_or(EditError::UnknownTake(take))?;
        let bars = takes.loop_bars();
        let mut notes = Vec::new();
        for note in take_ref.visible_notes(bars) {
            if let Some(effective) = self.effective_note(take_ref, note)? {
                notes.push(effective);
            }
        }
        sort_effective(&mut notes);
        Ok(notes)
    }

    /// Derive one effective note: quantize (D13) the nudged onset, preserve
    /// the original musical duration, then apply the velocity override.
    /// Returns `None` for deleted events.
    fn effective_note(&self, take: &Take, note: &Note) -> Result<Option<EffectiveNote>, EditError> {
        let edit = self.event_edit(note.on().id);
        if edit.deleted {
            return Ok(None);
        }
        let tempo = take.timing().recording_tempo();
        let mut timing = TakeTiming::new(take.id(), tempo);
        timing.set_quantization(self.quantization(take.id()));
        let derived = if edit.nudge_nanos == 0 {
            timing.view(note)?
        } else {
            timing.view(&shifted_note(note, edit.nudge_nanos)?)?
        };
        let mut effective = derived;
        if let Some(velocity) = edit.velocity {
            effective.velocity = velocity;
        }
        Ok(Some(effective))
    }

    fn require_live_take(&self, takes: &Takes, id: TakeId) -> Result<(), EditError> {
        match takes.take(id) {
            None => Err(EditError::UnknownTake(id)),
            Some(take) if take.removal().is_some() => Err(EditError::RemovedTake(id)),
            Some(_) => Ok(()),
        }
    }

    /// Event-level edits stay defined for events of removed takes: the
    /// originals remain inspectable and editable (undo of a take removal
    /// brings its edits back with it).
    fn require_event(&self, takes: &Takes, id: EventId) -> Result<(), EditError> {
        let take = takes.take(id.take).ok_or(EditError::UnknownTake(id.take))?;
        if take.notes().iter().any(|note| note.on().id == id) {
            Ok(())
        } else {
            Err(EditError::UnknownEvent(id))
        }
    }

    fn edit_event(&mut self, id: EventId, to: EventEdit) {
        let from = self.event_edit(id);
        if from == to {
            return;
        }
        self.set_event_edit(id, to);
        self.record(Op::Event { id, from, to });
    }

    fn set_quantization(&mut self, take: TakeId, value: Quantization) {
        self.quantization.insert(take, value);
    }

    fn set_event_edit(&mut self, id: EventId, value: EventEdit) {
        if value == EventEdit::default() {
            self.events.remove(&id);
        } else {
            self.events.insert(id, value);
        }
    }

    fn record(&mut self, op: Op) {
        if op.is_noop() {
            return;
        }
        if let Some(drag) = &mut self.drag {
            if let Some(last) = drag.ops.last_mut()
                && last.merge(&op)
            {
                return;
            }
            drag.ops.push(op);
        } else {
            self.push_undo(Step { ops: vec![op] });
            self.redo.clear();
        }
    }

    fn push_undo(&mut self, step: Step) {
        self.undo.push(step);
        if self.undo.len() > MAX_UNDO_STEPS {
            self.undo.remove(0);
        }
    }
}

fn sort_effective(notes: &mut [EffectiveNote]) {
    notes.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
}

/// Synthesize the nudged original-domain note used only for view derivation.
/// The stored note is never modified (D9); the whole note moves, so its
/// original duration is preserved. The onset clamps at take start.
fn shifted_note(note: &Note, delta_nanos: i64) -> Result<Note, EditError> {
    let on = note.on();
    let start = i128::from(on.original_time.nanos()) + i128::from(delta_nanos);
    let start = u64::try_from(start.max(0)).map_err(|_| EditError::NudgeOverflow)?;
    let end = start.saturating_add(note.duration_nanos());
    let mut lifecycle = NoteLifecycle::default();
    lifecycle.note_on(NoteOn {
        id: on.id,
        target: on.target,
        key: on.key,
        velocity: on.velocity,
        original_time: OriginalTime::from_nanos(start),
    })?;
    Ok(lifecycle.note_off(on.key, OriginalTime::from_nanos(end))?)
}

#[cfg(test)]
mod tests;
