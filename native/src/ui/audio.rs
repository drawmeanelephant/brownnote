use super::{State, pedals};
use brownnote_native::{
    engine::{Command, SLOTS},
    output::Output,
};

impl State {
    pub(super) fn retry(&mut self) {
        self.prune_slots();
        let output = Output::open(self.slots.clone())
            .unwrap_or_else(|e| Output::virtual_driver(self.slots.clone(), e.to_string(), true));
        self.install(output);
    }

    /// Drop every rendered slot no pad points at. A fresh `Output` has no
    /// voices, so a slot whose unload was waiting for its voice to end can
    /// never retire there; keeping it would leak render slots on every
    /// recovery until none were left.
    pub(super) fn prune_slots(&mut self) {
        let owned: Vec<usize> = self
            .pedals
            .pads
            .iter()
            .filter_map(|pad| pad.rendered_slot)
            .collect();
        for slot in pedals::FIRST_RENDER_SLOT..SLOTS {
            if !owned.contains(&slot) {
                self.slots.set(slot, None);
            }
        }
        self.pedals.unloads.clear();
    }

    fn install(&mut self, output: Output) {
        self.output = output;
        self.output.send(Command::SetMaster(self.master));
        self.output.send(Command::SetChoke(self.choke));
        if let Some(wake) = &self.wake {
            self.output.set_waker(wake.clone());
        }
    }

    pub(super) fn recover_stream_error(&mut self) -> bool {
        let Some(error) = self.output.take_error() else {
            return false;
        };
        self.prune_slots();
        let output =
            Output::virtual_driver(self.slots.clone(), format!("Audio error: {error}"), true);
        self.install(output);
        true
    }
}
