use super::{State, pedals};
use brownnote_native::{
    engine::{Command, SLOTS, Slots},
    output::{Output, OutputSelection},
};

impl State {
    pub(super) fn retry(&mut self) {
        let selection = self.output.selection().clone();
        self.stop_for_output_change(selection.clone(), "Retrying selected output".into());
        self.open_output_selection(&selection, &|slots, selection| {
            Output::open_selected(slots, selection).map_err(|error| error.to_string())
        });
    }

    pub(super) fn stop_for_output_change(&mut self, selection: OutputSelection, reason: String) {
        self.prune_slots();
        let output = Output::silent_selected(self.slots.clone(), selection, reason);
        self.install(output);
    }

    pub(super) fn open_output_selection(
        &mut self,
        selection: &OutputSelection,
        open: &dyn Fn(Slots, &OutputSelection) -> Result<Output, String>,
    ) {
        // The stop phase has already dropped the previous device stream.
        // Prune again: a render may have retired while opening was pending.
        self.prune_slots();
        let output = match open(self.slots.clone(), selection) {
            Ok(mut output) => {
                output.status.push_str(" · Playing voices stopped");
                output
            }
            Err(error) => Output::silent_selected(self.slots.clone(), selection.clone(), error),
        };
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
        self.counts = [0; 16];
        self.peak = 0.0;
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
        self.recover_audio_error(error);
        true
    }

    pub(super) fn recover_audio_error(&mut self, error: String) {
        self.stop_for_output_change(
            self.output.selection().clone(),
            format!("Audio error: {error}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brownnote_native::{
        kit::{DEFAULT_SAMPLES, Kit},
        output::OutputSelection,
    };
    use std::path::Path;

    #[test]
    fn retry_remembers_a_missing_id_and_replaces_active_voices_with_an_honest_silent_mixer() {
        let mut state = State::new(Kit::load(Path::new(DEFAULT_SAMPLES)), true, true);
        let selection = OutputSelection {
            device_id: Some(cpal::DeviceId::new(
                cpal::default_host().id(),
                "brownnote-missing-device-fixture",
            )),
            sample_rate: Some(48_000),
            ..Default::default()
        };
        state.output =
            Output::silent_selected(state.slots.clone(), selection.clone(), "lost".into());
        state.trigger(0, 1.0);
        state.output.advance(0.01);
        assert!(state.output.meters.counts()[0] > 0);

        state.retry();
        assert_eq!(state.output.selection(), &selection);
        assert!(state.output.failed);
        assert!(state.output.opened().is_none());
        assert!(state.output.timestamps().is_none());
        assert!(state.output.status.starts_with("Silent: "));
        assert!(state.output.status.contains("voices stopped"));
        assert_eq!(state.output.meters.counts(), [0; 16]);
        state.output.advance(0.01);
        assert_eq!(state.output.meters.counts(), [0; 16]);
        state.trigger(0, 1.0);
        state.output.advance(0.01);
        assert!(
            state.output.meters.counts()[0] > 0,
            "samples survived retry"
        );
    }
}
