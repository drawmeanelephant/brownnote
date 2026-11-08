//! Silent real-device scheduling smoke. No identifiers or latency claims.
use brownnote_native::{
    engine::{Sample, Slots},
    output::{Output, OutputSelection, PlaybackScheduleClock},
    performance::{
        EventId, OriginalTime, TakeId,
        scheduler::{ScheduleOutcome, ScheduledAction, ScheduledEvent, Trigger},
    },
};
use std::{error::Error, sync::Arc, time::Duration};

fn main() -> Result<(), Box<dyn Error>> {
    let mut previous_epoch = None;
    for rate in [44_100, 48_000, 48_000] {
        let mut output = Output::open_selected(
            Slots::from_samples([Sample {
                data: Arc::from(vec![0.0; rate as usize]),
                rate,
            }]),
            &OutputSelection {
                sample_rate: Some(rate),
                ..Default::default()
            },
        )?;
        let epoch = output.scheduler().epoch();
        if Some(epoch) == previous_epoch || output.meters.counts() != [0; 16] {
            return Err("reopened stream reused epoch or voices".into());
        }
        std::thread::sleep(Duration::from_millis(40));
        let timestamps = output.timestamps().ok_or("no physical timestamp seam")?;
        let observation = timestamps.take_latest().ok_or("no physical callbacks")?;
        if timestamps.dropped() != 0 || observation.stream_id.epoch() != epoch {
            return Err("timestamp overflow or scheduler/CPAL epoch mismatch".into());
        }
        let mapping = PlaybackScheduleClock {
            observation,
            take_time_at_playback: OriginalTime::from_nanos(0),
        };
        let id = EventId {
            take: TakeId(1),
            sequence: 0,
        };
        let original_time = OriginalTime::from_nanos(100_000_000);
        let on = ScheduledEvent {
            epoch,
            frame: mapping.frame_for_original(epoch, original_time)?,
            original_time,
            action: ScheduledAction::NoteOn {
                id,
                trigger: Trigger {
                    pad: 0,
                    slot: 0,
                    group: 0,
                    velocity: 1.0,
                    gain: 1.0,
                    pan: 0.0,
                    semitones: 0.0,
                },
            },
        };
        if let Some(old_epoch) = previous_epoch
            && output
                .schedule(ScheduledEvent {
                    epoch: old_epoch,
                    ..on
                })
                .outcome
                != ScheduleOutcome::StaleEpoch
        {
            return Err("old stream work was accepted after reopen".into());
        }
        if output.schedule(on).outcome != ScheduleOutcome::Accepted {
            return Err("scheduled note-on was not accepted".into());
        }
        let end = OriginalTime::from_nanos(250_000_000);
        let off = ScheduledEvent {
            frame: mapping.frame_for_original(epoch, end)?,
            original_time: end,
            action: ScheduledAction::NoteOff { id },
            ..on
        };
        if output.schedule(off).outcome != ScheduleOutcome::Accepted {
            return Err("scheduled release was not accepted".into());
        }
        std::thread::sleep(Duration::from_millis(500));
        if output.take_error().is_some() {
            return Err("physical stream reported an error".into());
        }
        let counters = output.scheduler().counters();
        if counters.dispatched != 2
            || counters.unavailable != 0
            || counters.late != 0
            || counters.outstanding != 0
            || output.meters.counts() != [0; 16]
        {
            return Err(format!("scheduled callback/release mismatch: {counters:?}").into());
        }
        println!(
            "{rate} Hz: physical timestamp/epoch conversion and two scheduled dispatches; release completed (silent sample)"
        );
        previous_epoch = Some(epoch);
    }
    Ok(())
}
