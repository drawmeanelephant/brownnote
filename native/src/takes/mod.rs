//! Pure producer-side takes. No transport, audio acknowledgement, or persistence.
use crate::performance::{
    EventId, LoopBars, MusicalPosition, Note, NoteKey, NoteLifecycle, NoteOn, OriginalTime,
    PerformanceError, TakeId, TakePhase, TakeTiming, Target, Tempo, TempoControl, Velocity,
};
use std::{collections::BTreeMap, fmt};

mod document;
pub use document::{
    NoteDocument, RemovalDocument, TAKES_FORMAT_VERSION, TakeDocument, TakesDocument,
    TakesDocumentError, TargetDocument,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PassMode {
    #[default]
    Overdub,
    Replace,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CountIn {
    #[default]
    Off,
    OneBar,
}

/// Per-pass intent only. A later transport implements count-in/metronome audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassOptions {
    pub mode: PassMode,
    pub count_in: CountIn,
    pub metronome: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    User,
    ReplacedBy(TakeId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakesError {
    Performance(PerformanceError),
    TakeIdsExhausted,
    EventIdsExhausted,
    UnknownPass(TakeId),
    NotRecording(TakeId),
    EmptyPass(TakeId),
    HeldNotes(TakeId),
    LoopLocked,
    UnknownTake(TakeId),
    Removed(TakeId),
    NotRemoved(TakeId),
}
impl From<PerformanceError> for TakesError {
    fn from(error: PerformanceError) -> Self {
        Self::Performance(error)
    }
}
impl fmt::Display for TakesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "takes: {self:?}")
    }
}
impl std::error::Error for TakesError {}

/// Tentative capture. Notes become a committed take only on successful finish.
pub struct Pass {
    timing: TakeTiming,
    phase: TakePhase,
    options: PassOptions,
    replace: Vec<TakeId>,
    next_sequence: Option<u64>,
    lifecycle: NoteLifecycle,
    notes: BTreeMap<EventId, Note>,
}
impl Pass {
    pub fn phase(&self) -> TakePhase {
        self.phase
    }
    pub fn options(&self) -> PassOptions {
        self.options
    }
    pub fn recording_tempo(&self) -> Tempo {
        self.timing.recording_tempo()
    }
    pub fn held_count(&self) -> usize {
        self.lifecycle.active_count()
    }
    pub fn completed_notes(&self) -> impl ExactSizeIterator<Item = &Note> {
        self.notes.values()
    }
}

/// Immutable original notes and recoverable take-level state.
pub struct Take {
    id: TakeId,
    timing: TakeTiming,
    notes: Vec<Note>,
    muted: bool,
    removal: Option<Removal>,
}
impl Take {
    pub fn id(&self) -> TakeId {
        self.id
    }
    pub fn timing(&self) -> &TakeTiming {
        &self.timing
    }
    /// Note-on acceptance order, including hidden, muted and removed originals.
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }
    pub fn muted(&self) -> bool {
        self.muted
    }
    pub fn removal(&self) -> Option<Removal> {
        self.removal
    }
    /// Visibility uses the original musical onset, not rounded or wrapped time.
    /// Mute/removal do not affect inspection of a take.
    pub fn visible_notes(&self, bars: LoopBars) -> impl Iterator<Item = &Note> {
        self.notes.iter().filter(move |note| {
            MusicalPosition::from_original(note.on().original_time, self.timing.recording_tempo())
                .visible_in(bars)
        })
    }
}

/// One project's in-memory take registry. The owner supplies a globally unused
/// first ID; accepted arms (even cancelled ones) never reuse an ID.
pub struct Takes {
    tempo: TempoControl,
    loop_bars: LoopBars,
    next_take: Option<TakeId>,
    passes: BTreeMap<TakeId, Pass>,
    takes: BTreeMap<TakeId, Take>,
    selected: Option<TakeId>,
}
impl Takes {
    /// The project owner must choose an unused ID range. Reopen goes through
    /// [`TakesDocument::to_takes`], which continues above every saved ID.
    pub fn new(loop_bars: LoopBars, first_take: TakeId) -> Self {
        Self {
            tempo: TempoControl::default(),
            loop_bars,
            next_take: Some(first_take),
            passes: BTreeMap::new(),
            takes: BTreeMap::new(),
            selected: None,
        }
    }
    pub fn tempo(&self) -> Tempo {
        self.tempo.tempo()
    }
    pub fn set_tempo(&mut self, tempo: Tempo) -> Result<(), TakesError> {
        self.tempo.set_tempo(tempo)?;
        Ok(())
    }
    pub fn loop_bars(&self) -> LoopBars {
        self.loop_bars
    }
    pub fn set_loop_bars(&mut self, bars: LoopBars) -> Result<(), TakesError> {
        if !self.passes.is_empty() {
            return Err(TakesError::LoopLocked);
        }
        self.loop_bars = bars;
        Ok(())
    }
    pub fn pass(&self, id: TakeId) -> Option<&Pass> {
        self.passes.get(&id)
    }
    pub fn take(&self, id: TakeId) -> Option<&Take> {
        self.takes.get(&id)
    }
    /// Includes removed takes, in stable ID order, for inspection/restoration.
    pub fn takes(&self) -> impl ExactSizeIterator<Item = &Take> {
        self.takes.values()
    }
    pub fn selected(&self) -> Option<TakeId> {
        self.selected
    }
    /// True while any pass is armed or recording (tempo and loop are locked).
    pub fn has_pass(&self) -> bool {
        !self.passes.is_empty()
    }

    pub fn arm(&mut self, options: PassOptions) -> Result<TakeId, TakesError> {
        let id = self.next_take.ok_or(TakesError::TakeIdsExhausted)?;
        let recording_tempo = self.tempo.arm(id)?;
        let replace = if options.mode == PassMode::Replace {
            self.takes
                .values()
                .filter(|take| take.removal.is_none())
                .map(|take| take.id)
                .collect()
        } else {
            Vec::new()
        };
        self.passes.insert(
            id,
            Pass {
                timing: TakeTiming::new(id, recording_tempo),
                phase: TakePhase::Armed,
                options,
                replace,
                next_sequence: Some(0),
                lifecycle: NoteLifecycle::default(),
                notes: BTreeMap::new(),
            },
        );
        self.next_take = id.0.checked_add(1).map(TakeId);
        Ok(id)
    }

    pub fn start(&mut self, id: TakeId) -> Result<(), TakesError> {
        let pass = self
            .passes
            .get_mut(&id)
            .ok_or(TakesError::UnknownPass(id))?;
        self.tempo.record(id)?;
        pass.phase = TakePhase::Recording;
        Ok(())
    }

    /// Input times are unwrapped nanoseconds from this pass's start. Callers
    /// route each release back to its owning pass; no cross-clock guessing.
    pub fn note_on(
        &mut self,
        take: TakeId,
        target: Target,
        key: NoteKey,
        velocity: Velocity,
        time: OriginalTime,
    ) -> Result<EventId, TakesError> {
        let pass = self.recording_pass(take)?;
        let sequence = pass.next_sequence.ok_or(TakesError::EventIdsExhausted)?;
        let id = EventId { take, sequence };
        pass.lifecycle.note_on(NoteOn {
            id,
            target,
            key,
            velocity,
            original_time: time,
        })?;
        pass.next_sequence = sequence.checked_add(1);
        Ok(id)
    }

    /// FIFO by existing logical source/channel/pitch identity (D14).
    pub fn note_off(
        &mut self,
        take: TakeId,
        key: NoteKey,
        time: OriginalTime,
    ) -> Result<EventId, TakesError> {
        let pass = self.recording_pass(take)?;
        let note = pass.lifecycle.note_off(key, time)?;
        let id = note.on().id;
        pass.notes.insert(id, note);
        Ok(id)
    }

    fn recording_pass(&mut self, id: TakeId) -> Result<&mut Pass, TakesError> {
        let pass = self
            .passes
            .get_mut(&id)
            .ok_or(TakesError::UnknownPass(id))?;
        if pass.phase != TakePhase::Recording {
            return Err(TakesError::NotRecording(id));
        }
        Ok(pass)
    }

    /// Empty or held-note passes fail without changing prior takes, selection,
    /// the pass, or tempo locks. No synthetic note ends are invented.
    pub fn finish(&mut self, id: TakeId) -> Result<(), TakesError> {
        let pass = self.recording_pass(id)?;
        if pass.held_count() != 0 {
            return Err(TakesError::HeldNotes(id));
        }
        if pass.notes.is_empty() {
            return Err(TakesError::EmptyPass(id));
        }
        self.tempo.finish(id)?;
        let pass = self.passes.remove(&id).ok_or(TakesError::UnknownPass(id))?;
        for replaced in pass.replace {
            if let Some(take) = self.takes.get_mut(&replaced)
                && take.removal.is_none()
            {
                take.removal = Some(Removal::ReplacedBy(id));
            }
        }
        self.takes.insert(
            id,
            Take {
                id,
                timing: pass.timing,
                notes: pass.notes.into_values().collect(),
                muted: false,
                removal: None,
            },
        );
        self.selected = Some(id);
        Ok(())
    }

    /// Discards only tentative capture. Previously committed takes are intact.
    pub fn cancel(&mut self, id: TakeId) -> Result<(), TakesError> {
        if !self.passes.contains_key(&id) {
            return Err(TakesError::UnknownPass(id));
        }
        self.tempo.finish(id)?;
        self.passes.remove(&id);
        Ok(())
    }

    /// An inspectable, unquantized event collection, not a scheduled/sounded
    /// acknowledgement. Selection never solos a take or changes this view.
    pub fn audible_notes(&self) -> impl Iterator<Item = &Note> {
        self.takes
            .values()
            .filter(|take| take.removal.is_none() && !take.muted)
            .flat_map(|take| take.visible_notes(self.loop_bars))
    }
    pub fn select(&mut self, id: TakeId) -> Result<(), TakesError> {
        self.live_take(id)?;
        self.selected = Some(id);
        Ok(())
    }
    pub fn set_muted(&mut self, id: TakeId, muted: bool) -> Result<(), TakesError> {
        self.live_take(id)?.muted = muted;
        Ok(())
    }
    pub fn remove(&mut self, id: TakeId) -> Result<(), TakesError> {
        self.live_take(id)?.removal = Some(Removal::User);
        if self.selected == Some(id) {
            self.selected = None;
        }
        Ok(())
    }
    pub fn restore(&mut self, id: TakeId) -> Result<(), TakesError> {
        let take = self.takes.get_mut(&id).ok_or(TakesError::UnknownTake(id))?;
        if take.removal.is_none() {
            return Err(TakesError::NotRemoved(id));
        }
        take.removal = None;
        Ok(())
    }
    fn live_take(&mut self, id: TakeId) -> Result<&mut Take, TakesError> {
        let take = self.takes.get_mut(&id).ok_or(TakesError::UnknownTake(id))?;
        if take.removal.is_some() {
            return Err(TakesError::Removed(id));
        }
        Ok(take)
    }
}

#[cfg(test)]
mod tests;
