//! Real-device seam smoke test. No IDs or names are printed. The measurement
//! block describes one machine's run; it is not a latency guarantee.
use brownnote_native::{
    engine::{Command, Sample, Slots},
    output::{CallbackSummary, Output, OutputSelection, output_devices},
};
use cpal::traits::{DeviceTrait, HostTrait};
use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
};

/// Long enough per rate to collect a small distribution of real callbacks, and
/// short enough to keep the manual run quick. `--check-audio` uses a
/// two-second window; the example only needs to show the block shape.
const PROBE_WINDOW: Duration = Duration::from_millis(500);
/// The same drain cadence as `--check-audio`, so the bounded queue cannot
/// overflow inside the shorter window.
const PROBE_POLL: Duration = Duration::from_millis(10);

fn main() -> Result<(), Box<dyn Error>> {
    let devices = output_devices()?;
    let default_id = cpal::default_host()
        .default_output_device()
        .ok_or("no default output")?
        .id()?;
    let selection = OutputSelection {
        device_id: Some(default_id),
        ..Default::default()
    };
    let device = selection.resolve(&devices, None)?;
    // Use the current default's stable ID explicitly, never a name lookup.
    // Keep the device name and identifier out of shareable probe output.
    let slots = Slots::from_samples([Sample {
        data: Arc::from(vec![0.0; 44_100]),
        rate: 44_100,
    }]);
    let mut previous_id = None;
    for rate in [44_100, 48_000] {
        let requested = OutputSelection {
            sample_rate: Some(rate),
            ..selection.clone()
        };
        device.configuration(&requested)?;
        // The same explicit device, opened with probing so the callback block
        // below can be read off-thread.
        let mut output = Output::open_selected_probed(slots.clone(), &requested)?;
        let opened = output.opened().ok_or("real output was not opened")?;
        if opened.configuration.stream.sample_rate != rate || Some(opened.stream_id) == previous_id
        {
            return Err("stream configuration or fresh identity mismatch".into());
        }
        previous_id = Some(opened.stream_id);
        // A new stream cannot inherit a voice from the previous stream.
        if output.meters.counts().iter().any(|count| *count != 0) {
            return Err("fresh mixer inherited playing voices".into());
        }
        output.send(Command::Trigger {
            pad: 0,
            slot: 0,
            group: 0,
            velocity: 1.0,
            gain: 1.0,
            pan: 0.0,
            semitones: 0.0,
        });
        // Bounded like `--check-audio`: absorb as we go, never block the audio
        // thread and never grow the summary without bound.
        let deadline = Instant::now() + PROBE_WINDOW;
        let mut timing = CallbackSummary::default();
        while Instant::now() < deadline {
            std::thread::sleep(PROBE_POLL);
            let measurements = output
                .measurements()
                .ok_or("probed open did not expose measurements")?;
            timing.absorb(measurements);
        }
        if output.take_error().is_some() {
            return Err("device reported a stream error".into());
        }
        if output.meters.counts()[0] == 0 {
            return Err("trigger did not reach the real stream".into());
        }
        let observed = output
            .timestamps()
            .and_then(|timestamps| timestamps.take_latest())
            .ok_or("no callback timestamps")?;
        if Some(observed.stream_id) != previous_id
            || observed.sample_rate.get() != rate
            || observed.frames == 0
        {
            return Err("callback identity/rate/buffer mismatch".into());
        }
        println!(
            "explicit CPAL ID: {rate} Hz, real callbacks and active voice observed; \
             fresh stream identity and empty initial voice pool verified"
        );
        // The same stable block `--check-audio` prints. Round-trip latency is
        // unmeasured because output-only instants cannot derive it.
        println!("{timing}");
        // Dropping this stream while its voice is active precedes the next open.
    }
    Ok(())
}
