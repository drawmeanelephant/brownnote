//! Real-device seam smoke test. No IDs or timing/latency claims are printed.
use brownnote_native::{
    engine::{Command, Sample, Slots},
    output::{Output, OutputSelection, output_devices},
};
use cpal::traits::{DeviceTrait, HostTrait};
use std::{error::Error, sync::Arc, time::Duration};

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
        let mut output = Output::open_selected(slots.clone(), &requested)?;
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
        std::thread::sleep(Duration::from_millis(100));
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
        // Dropping this stream while its voice is active precedes the next open.
    }
    Ok(())
}
