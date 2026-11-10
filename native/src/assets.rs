//! UI/control-thread runtime identities and transactional slot ownership.
//! Slot allocation is shared by sources and retained renders, never partitioned.
use crate::{
    engine::{Command, Returned, SLOTS, Sample, Slots},
    import::Imported,
    live_audio::StereoClip,
    project::{AssetId, MediaKind, PAD_COUNT, PadId, SampleAssignment},
};
use std::{collections::BTreeMap, sync::Arc};

/// Choke identity is intentional, not derived from a reusable engine slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChokeIdentity {
    Asset(AssetId),
    Group(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replacement {
    pad: PadId,
    generation: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimePad {
    pub assignment: Option<SampleAssignment>,
    pub choke: Option<ChokeIdentity>,
    /// Failed import/replacement feedback never disables its current sound.
    pub error: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct RuntimeAsset {
    pub imported: Imported,
    pub slot: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Free,
    External,
    Live(AssetId),
    PendingUnload,
    Retiring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    Stale,
    Full,
    QueueFull,
    MissingAsset(AssetId),
    WrongKind,
    Occupied,
    UnexpectedReturn,
}

pub struct AssetRegistry {
    slots: Slots,
    states: [SlotState; SLOTS],
    assets: BTreeMap<AssetId, RuntimeAsset>,
    pads: [RuntimePad; PAD_COUNT],
    groups: BTreeMap<ChokeIdentity, usize>,
    generation: u64,
}

impl Default for AssetRegistry {
    fn default() -> Self {
        Self::new(Slots::empty())
    }
}

impl AssetRegistry {
    /// Existing occupied slots remain reserved until explicitly registered.
    /// The caller must use this registry as the sole allocator thereafter.
    pub fn new(slots: Slots) -> Self {
        Self {
            states: std::array::from_fn(|slot| {
                if slots.is_empty(slot) {
                    SlotState::Free
                } else {
                    SlotState::External
                }
            }),
            slots,
            assets: BTreeMap::new(),
            pads: std::array::from_fn(|_| RuntimePad::default()),
            groups: BTreeMap::new(),
            generation: 0,
        }
    }

    /// Already-loaded project/kit samples can be adopted without another Load.
    /// Hash metadata must come from the authoritative encoded media snapshot.
    pub fn register_existing(
        &mut self,
        slot: usize,
        imported: Imported,
    ) -> Result<(), RegistryError> {
        let id = imported.asset.reference.id;
        if self.states.get(slot) != Some(&SlotState::External)
            || self.assets.contains_key(&id)
            || !self.slots.get(slot).is_some_and(|sample| {
                sample.rate == imported.sample.rate && sample.data == imported.sample.data
            })
        {
            return Err(RegistryError::Occupied);
        }
        self.states[slot] = SlotState::Live(id);
        self.assets.insert(id, RuntimeAsset { imported, slot });
        Ok(())
    }

    /// Bootstrap only, before live editing. Does not send or retire anything.
    pub fn bind_existing(
        &mut self,
        pad: PadId,
        assignment: SampleAssignment,
        choke: ChokeIdentity,
    ) -> Result<(), RegistryError> {
        self.require(assignment.source)?;
        if let Some(id) = assignment.retained
            && self.require(id)?.imported.asset.kind != MediaKind::RetainedRender
        {
            return Err(RegistryError::WrongKind);
        }
        if self.pads[pad.index()].assignment.is_some() {
            return Err(RegistryError::Occupied);
        }
        self.group(choke);
        self.pads[pad.index()].assignment = Some(assignment);
        self.pads[pad.index()].choke = Some(choke);
        Ok(())
    }

    pub fn slots(&self) -> &Slots {
        &self.slots
    }

    /// Pad-playable assets this registry holds: one mixer slot each, and
    /// exactly the pad entries a save lists. Retained live take slots are
    /// `External`, not assets, so they are never counted here.
    pub fn pad_assets(&self) -> usize {
        self.assets
            .values()
            .filter(|asset| asset.imported.asset.kind.is_pad_audio())
            .count()
    }

    /// Reserve the next two free slots for a retained live take's stereo audio
    /// and load them into the running mixer. The slots stay `External`: no pad
    /// import can take them, they belong to project media, and only the
    /// project replaces them. Nothing changes if either load is refused.
    pub fn claim_live_take(
        &mut self,
        clip: &StereoClip,
        mut send: impl FnMut(Command) -> bool,
    ) -> Result<(usize, usize), RegistryError> {
        let mut free = self
            .states
            .iter()
            .enumerate()
            .filter(|(_, state)| **state == SlotState::Free)
            .map(|(slot, _)| slot);
        let (Some(left), Some(right)) = (free.next(), free.next()) else {
            return Err(RegistryError::Full);
        };
        let left_sample = Sample {
            data: clip.left.clone(),
            rate: clip.rate,
        };
        let right_sample = Sample {
            data: clip.right.clone(),
            rate: clip.rate,
        };
        if !send(Command::Load {
            slot: left,
            sample: left_sample.clone(),
        }) {
            return Err(RegistryError::QueueFull);
        }
        if !send(Command::Load {
            slot: right,
            sample: right_sample.clone(),
        }) {
            // The registry never claimed the first slot; the mixer returns it.
            let _ = send(Command::Unload { slot: left });
            return Err(RegistryError::QueueFull);
        }
        self.slots.set(left, Some(left_sample));
        self.slots.set(right, Some(right_sample));
        self.states[left] = SlotState::External;
        self.states[right] = SlotState::External;
        Ok((left, right))
    }

    /// Take a claimed slot pair back, for a take that could not be retained.
    /// The mixer retires both samples and the slots become allocatable again.
    pub fn release_live_take(
        &mut self,
        (left, right): (usize, usize),
        send: impl FnMut(Command) -> bool,
    ) {
        for slot in [left, right] {
            if self.states.get(slot) == Some(&SlotState::External) && !self.slots.is_empty(slot) {
                self.states[slot] = SlotState::PendingUnload;
            }
        }
        self.flush_unloads(send);
    }

    pub fn has_retirements(&self) -> bool {
        self.states
            .iter()
            .any(|state| matches!(state, SlotState::PendingUnload | SlotState::Retiring))
    }

    pub fn asset(&self, id: AssetId) -> Option<&RuntimeAsset> {
        self.assets.get(&id)
    }

    pub fn pad(&self, pad: PadId) -> &RuntimePad {
        &self.pads[pad.index()]
    }

    pub fn source(&self, pad: PadId) -> Option<&RuntimeAsset> {
        self.asset(self.pad(pad).assignment.as_ref()?.source)
    }

    pub fn audible(&self, pad: PadId) -> Option<&RuntimeAsset> {
        self.asset(self.pad(pad).assignment.as_ref()?.audible())
    }

    /// Every pad mutation starts a new ticket, invalidating overlapping imports
    /// and pedal completions for that pad without changing its current sound.
    pub fn begin(&mut self, pad: PadId) -> Replacement {
        self.generation = self
            .generation
            .checked_add(1)
            .unwrap_or_else(|| panic!("replacement identity exhausted"));
        self.pads[pad.index()].generation = self.generation;
        Replacement {
            pad,
            generation: self.generation,
        }
    }

    pub fn cancel(&mut self, replacement: Replacement) {
        if self.current(replacement) {
            self.begin(replacement.pad);
        }
    }

    pub fn report_error(&mut self, replacement: Replacement, error: String) -> bool {
        if !self.current(replacement) {
            return false;
        }
        self.pads[replacement.pad.index()].error = Some(error);
        self.begin(replacement.pad);
        true
    }

    pub fn replace_source(
        &mut self,
        replacement: Replacement,
        imported: &Imported,
        choke: ChokeIdentity,
        mut send: impl FnMut(Command) -> bool,
    ) -> Result<(), RegistryError> {
        self.check(replacement)?;
        let id = imported.asset.reference.id;
        self.install(imported, &mut send)?;
        self.group(choke);
        self.commit(
            replacement,
            SampleAssignment {
                source: id,
                retained: None,
            },
            choke,
        );
        self.flush_unloads(send);
        Ok(())
    }

    /// Only authoritative, project-published renders belong here, never a
    /// disposable cache-only result. Choke follows the pad's source identity.
    pub fn replace_retained(
        &mut self,
        replacement: Replacement,
        imported: &Imported,
        mut send: impl FnMut(Command) -> bool,
    ) -> Result<(), RegistryError> {
        self.check(replacement)?;
        if imported.asset.kind != MediaKind::RetainedRender {
            return Err(RegistryError::WrongKind);
        }
        let pad = self.pad(replacement.pad);
        let Some(mut assignment) = pad.assignment.clone() else {
            return Err(RegistryError::MissingAsset(imported.asset.reference.id));
        };
        let choke = pad.choke.ok_or(RegistryError::UnexpectedReturn)?;
        self.install(imported, &mut send)?;
        assignment.retained = Some(imported.asset.reference.id);
        self.commit(replacement, assignment, choke);
        self.flush_unloads(send);
        Ok(())
    }

    pub fn reset_to_source(&mut self, pad: PadId, send: impl FnMut(Command) -> bool) {
        let Some(mut assignment) = self.pad(pad).assignment.clone() else {
            return;
        };
        let Some(choke) = self.pad(pad).choke else {
            return;
        };
        assignment.retained = None;
        let replacement = self.begin(pad);
        self.commit(replacement, assignment, choke);
        self.flush_unloads(send);
    }

    /// A snapshot for the real Command::Trigger, including intentional choke.
    pub fn playback(&self, pad: PadId) -> Option<(usize, usize)> {
        let asset = self.audible(pad)?;
        let group = self.groups.get(&self.pad(pad).choke?)?;
        Some((asset.slot, *group))
    }

    /// Retry only unadmitted Unloads. An accepted one is sent exactly once.
    pub fn flush_unloads(&mut self, mut send: impl FnMut(Command) -> bool) {
        for slot in 0..SLOTS {
            if self.states[slot] == SlotState::PendingUnload && send(Command::Unload { slot }) {
                self.states[slot] = SlotState::Retiring;
            }
        }
    }

    /// A fresh Output built from slots() has a fresh command queue. Call only
    /// after discarding the old Output and its return queue.
    pub fn output_rebuilt(&mut self) {
        for state in &mut self.states {
            if *state == SlotState::Retiring {
                *state = SlotState::PendingUnload;
            }
        }
    }

    /// Only the mixer's matching retirement makes a slot reusable.
    pub fn returned(&mut self, returned: Returned) -> Result<usize, RegistryError> {
        let Returned::Retired { slot, sample } = returned else {
            return Err(RegistryError::UnexpectedReturn);
        };
        if self.states.get(slot) != Some(&SlotState::Retiring)
            || !self.slots.get(slot).is_some_and(|held| {
                held.rate == sample.rate && Arc::ptr_eq(&held.data, &sample.data)
            })
        {
            return Err(RegistryError::UnexpectedReturn);
        }
        self.slots.set(slot, None);
        self.states[slot] = SlotState::Free;
        Ok(slot)
    }

    pub fn current(&self, replacement: Replacement) -> bool {
        self.pad(replacement.pad).generation == replacement.generation
    }

    fn check(&self, replacement: Replacement) -> Result<(), RegistryError> {
        if self.current(replacement) {
            Ok(())
        } else {
            Err(RegistryError::Stale)
        }
    }

    fn group(&mut self, identity: ChokeIdentity) {
        // Identities persist across retirements: an old voice must still choke
        // correctly if that sound is imported again while the voice is alive.
        let next = self.groups.len();
        self.groups.entry(identity).or_insert(next);
    }

    fn require(&self, id: AssetId) -> Result<&RuntimeAsset, RegistryError> {
        self.asset(id).ok_or(RegistryError::MissingAsset(id))
    }

    fn install(
        &mut self,
        imported: &Imported,
        send: &mut impl FnMut(Command) -> bool,
    ) -> Result<(), RegistryError> {
        let id = imported.asset.reference.id;
        if let Some(existing) = self.assets.get_mut(&id) {
            // Identical encoded bytes can serve as both source and render.
            // RetainedRender is the stronger document retention requirement.
            if imported.asset.kind == MediaKind::RetainedRender {
                existing.imported.asset.kind = MediaKind::RetainedRender;
            }
            return Ok(());
        }
        let slot = self
            .states
            .iter()
            .position(|state| *state == SlotState::Free)
            .ok_or(RegistryError::Full)?;
        // The registry is mutably borrowed for admission; no second allocator
        // can take this slot between selection and commit.
        if !send(Command::Load {
            slot,
            sample: imported.sample.clone(),
        }) {
            return Err(RegistryError::QueueFull);
        }
        self.slots.set(slot, Some(imported.sample.clone()));
        self.states[slot] = SlotState::Live(id);
        self.assets.insert(
            id,
            RuntimeAsset {
                imported: imported.clone(),
                slot,
            },
        );
        Ok(())
    }

    fn commit(
        &mut self,
        replacement: Replacement,
        assignment: SampleAssignment,
        choke: ChokeIdentity,
    ) {
        let previous = self.pads[replacement.pad.index()]
            .assignment
            .replace(assignment);
        self.pads[replacement.pad.index()].choke = Some(choke);
        self.pads[replacement.pad.index()].error = None;
        self.begin(replacement.pad);
        if let Some(previous) = previous {
            self.retire_unreferenced(previous.source);
            if let Some(id) = previous.retained {
                self.retire_unreferenced(id);
            }
        }
    }

    fn retire_unreferenced(&mut self, id: AssetId) {
        if self.pads.iter().any(|pad| {
            pad.assignment.as_ref().is_some_and(|assignment| {
                assignment.source == id || assignment.retained == Some(id)
            })
        }) {
            return;
        }
        if let Some(asset) = self.assets.remove(&id) {
            self.states[asset.slot] = SlotState::PendingUnload;
        }
    }
}
