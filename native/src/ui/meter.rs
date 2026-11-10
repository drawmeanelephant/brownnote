//! The app's output-meter clip latch (#185). The engine keeps `Meters.clipped`
//! set until the UI exchanges it for false and owns no timing of its own
//! (`native/src/engine/mod.rs`), so this module exchanges it on every poll and
//! holds the indication for [`CLIP_LATCH`] seconds of app clock time after the
//! last overload, matching the showcase's `Meter::feed` (D17).
use super::State;
use brownnote_native::look::motion::CLIP_LATCH;
use std::sync::atomic::Ordering::Relaxed;

/// When the last overload arrived, and the indication it lights.
#[derive(Clone, Copy, Debug, Default)]
pub struct Meter {
    /// App-clock time of the last fresh overload; stale after the latch ends.
    at: Option<f64>,
    lit: bool,
}

impl Meter {
    /// Advance the latch with one poll's exchanged overload flag. Only a
    /// fresh overload restarts it; silence does not extend it. Returns
    /// whether the indication changed.
    pub fn feed(&mut self, clock: f64, clipped: bool) -> bool {
        let was = self.lit;
        if clipped {
            self.at = Some(clock);
        }
        self.lit = self.at.is_some_and(|at| clock - at < CLIP_LATCH);
        if !self.lit {
            self.at = None;
        }
        self.lit != was
    }

    /// Whether the clip indication is lit at the last polled clock time.
    pub fn lit(&self) -> bool {
        self.lit
    }
}

/// One poll step: consume the engine's flag beside the peak swap, so no stale
/// `true` survives a poll and an output replaced before it is read (device
/// loss) cannot latch anything.
pub(super) fn poll(s: &mut State, clock: f64) -> bool {
    let clipped = s.output.meters.clipped.swap(false, Relaxed);
    s.meter.feed(clock, clipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brownnote_native::output::Output;

    #[test]
    fn an_overload_lights_the_indication_and_silence_does_not_extend_it() {
        let mut meter = Meter::default();
        assert!(meter.feed(1.0, true));
        assert!(meter.lit());
        assert!(!meter.feed(1.5, false));
        assert!(!meter.feed(2.499, false));
        assert!(meter.lit(), "still lit just under 1.5 s");
        assert!(meter.feed(2.5, false));
        assert!(!meter.lit(), "clears 1.5 s after the last overload");
        assert!(!meter.feed(9.0, false));
    }

    #[test]
    fn a_fresh_overload_restarts_the_latch_instead_of_extending_it() {
        let mut meter = Meter::default();
        meter.feed(1.0, true);
        meter.feed(2.0, true);
        meter.feed(3.0, false);
        assert!(
            meter.lit(),
            "silence after the second overload keeps it lit"
        );
        meter.feed(3.49, false);
        assert!(meter.lit());
        meter.feed(3.5, false);
        assert!(!meter.lit(), "the second overload set its own deadline");
    }

    #[test]
    fn a_second_overload_after_it_cleared_lights_it_again() {
        let mut meter = Meter::default();
        meter.feed(1.0, true);
        for clock in [1.0, 1.5, 2.5, 3.0] {
            meter.feed(clock, false);
        }
        assert!(!meter.lit());
        assert!(meter.feed(4.0, true));
        assert!(meter.lit());
    }

    #[test]
    fn a_clean_peak_never_lights_it() {
        let mut meter = Meter::default();
        for clock in [0.0, 0.5, 1.0, 10.0] {
            assert!(!meter.feed(clock, false));
            assert!(!meter.lit());
        }
    }

    #[test]
    fn a_replaced_output_does_not_latch_a_flag_it_never_polled() {
        let kit = crate::ui_test_support::kit(0.5, 44_100, 44_100);
        let mut s = State::new(kit, true, true);
        s.output.meters.clipped.store(true, Relaxed);
        // Device loss replaces the mixer before the next poll
        // (`State::recover_stream_error` runs first); the unpolled flag leaves
        // with the old output instead of lighting the latch.
        s.install_output(Output::virtual_driver(
            s.assets.slots().clone(),
            "Audio device lost".into(),
            true,
        ));
        assert!(!poll(&mut s, 1.0));
        assert!(!s.meter.lit());
        assert!(!s.output.meters.clipped.load(Relaxed));
    }
}
