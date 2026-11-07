use crate::engine::{Command, Meters, Mixer, QUEUE_CAPACITY, SLOTS, Sample, Slots};
use cpal::{
    SampleFormat, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    fmt,
    sync::{Arc, OnceLock, mpsc},
    time::Duration,
};

#[derive(Debug)]
pub enum AudioError {
    NoDevice,
    UnsupportedFormat(SampleFormat),
    Device(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => write!(f, "No default output device"),
            Self::UnsupportedFormat(format) => write!(f, "Unsupported audio format: {format}"),
            Self::Device(message) => write!(f, "Audio error: {message}"),
        }
    }
}
impl std::error::Error for AudioError {}

fn device_error(e: impl fmt::Display) -> AudioError {
    AudioError::Device(e.to_string())
}

enum Driver {
    Device {
        _stream: Stream,
        errors: mpsc::Receiver<String>,
    },
    Virtual {
        mixer: Box<Mixer>,
        commands: rtrb::Consumer<Command>,
        remainder: f64,
    },
}

pub struct Output {
    driver: Driver,
    wake: Arc<OnceLock<rune::Waker>>,
    pub commands: rtrb::Producer<Command>,
    /// Samples the mixer has finished with, for the UI thread to drop.
    retired: rtrb::Consumer<Sample>,
    pub meters: Arc<Meters>,
    pub status: String,
    pub failed: bool,
    pub dropped: u64,
}

impl Output {
    pub fn virtual_driver(slots: impl Into<Slots>, status: String, failed: bool) -> Self {
        let (commands, consumer) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let (retired_commands, retired) = rtrb::RingBuffer::new(SLOTS);
        let meters = Arc::new(Meters::default());
        Self {
            driver: Driver::Virtual {
                mixer: Box::new(Mixer::new(
                    slots.into(),
                    44_100,
                    meters.clone(),
                    retired_commands,
                )),
                commands: consumer,
                remainder: 0.0,
            },
            commands,
            retired,
            meters,
            wake: Arc::new(OnceLock::new()),
            status,
            failed,
            dropped: 0,
        }
    }

    pub fn open(slots: impl Into<Slots>) -> Result<Self, AudioError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(AudioError::NoDevice)?;
        let supported = device.default_output_config().map_err(device_error)?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let name = device
            .description()
            .map_err(device_error)?
            .name()
            .to_owned();
        let status = format!(
            "Output: {name} · {} Hz · {format:?} · {} ch",
            config.sample_rate, config.channels
        );
        let (commands, consumer) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let (retired_commands, retired) = rtrb::RingBuffer::new(SLOTS);
        let meters = Arc::new(Meters::default());
        let mixer = Mixer::new(
            slots.into(),
            config.sample_rate,
            meters.clone(),
            retired_commands,
        );
        let (send_error, errors) = mpsc::channel();
        let wake = Arc::new(OnceLock::new());
        let send_error = ErrorSink {
            sender: send_error,
            wake: wake.clone(),
        };
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, mixer, consumer, send_error),
            SampleFormat::I16 => build::<i16>(&device, config, mixer, consumer, send_error),
            SampleFormat::U16 => build::<u16>(&device, config, mixer, consumer, send_error),
            SampleFormat::I32 => build::<i32>(&device, config, mixer, consumer, send_error),
            other => return Err(AudioError::UnsupportedFormat(other)),
        }?;
        stream.play().map_err(device_error)?;
        Ok(Self {
            driver: Driver::Device {
                _stream: stream,
                errors,
            },
            commands,
            retired,
            meters,
            wake,
            status,
            failed: false,
            dropped: 0,
        })
    }

    pub fn send(&mut self, command: Command) {
        if self.commands.push(command).is_err() {
            self.dropped += 1;
        }
    }

    /// Samples the mixer has finished with. Call from the UI thread; dropping
    /// the result drops the samples there, never in the callback.
    pub fn take_retired(&mut self) -> Vec<Sample> {
        let mut samples = Vec::new();
        while let Ok(sample) = self.retired.pop() {
            samples.push(sample);
        }
        samples
    }

    pub fn set_waker(&self, wake: rune::Waker) {
        let _ = self.wake.set(wake);
    }

    /// Device failures are handed to the UI, never sent from the data callback.
    pub fn take_error(&self) -> Option<String> {
        match &self.driver {
            Driver::Device { errors, .. } => errors.try_recv().ok(),
            Driver::Virtual { .. } => None,
        }
    }

    pub fn advance(&mut self, seconds: f64) {
        if let Driver::Virtual {
            mixer,
            commands,
            remainder,
        } = &mut self.driver
        {
            let exact = seconds.clamp(0.0, 0.25) * 44_100.0 + *remainder;
            let mut frames = exact.floor() as usize;
            *remainder = exact.fract();
            mixer.drain(commands);
            let mut scratch = [0.0; 512];
            if frames == 0 {
                mixer.render(&mut [], 2);
            }
            while frames > 0 {
                let count = frames.min(scratch.len() / 2);
                mixer.render(&mut scratch[..count * 2], 2);
                frames -= count;
            }
        }
    }
}

struct ErrorSink {
    sender: mpsc::Sender<String>,
    wake: Arc<OnceLock<rune::Waker>>,
}

impl ErrorSink {
    fn report(&self, error: String) {
        let _ = self.sender.send(error);
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: StreamConfig,
    mut mixer: Mixer,
    mut commands: rtrb::Consumer<Command>,
    errors: ErrorSink,
) -> Result<Stream, AudioError> {
    let channels = usize::from(config.channels);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                mixer.drain(&mut commands);
                mixer.render_into(data, channels, T::from_sample);
            },
            move |error| {
                errors.report(error.to_string());
            },
            None,
        )
        .map_err(device_error)
}

pub fn check_audio(samples: Box<[Sample]>) -> Result<String, AudioError> {
    if samples.first().is_none_or(|s| s.data.is_empty()) {
        return Err(AudioError::Device(
            "pad 0 sample is missing or invalid".into(),
        ));
    }
    let mut output = Output::open(samples)?;
    output.send(Command::Trigger {
        pad: 0,
        slot: 0,
        group: 0,
        velocity: 1.0,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    });
    std::thread::sleep(Duration::from_secs(2));
    if let Some(error) = output.take_error() {
        return Err(AudioError::Device(error));
    }
    use std::sync::atomic::Ordering::Relaxed;
    let frames = output.meters.frames.load(Relaxed);
    let peak = f32::from_bits(output.meters.peak.load(Relaxed));
    if frames == 0 || peak <= 0.0 {
        return Err(AudioError::Device(format!(
            "no signal: frames={frames}, peak={peak}"
        )));
    }
    Ok(format!(
        "{}\nframes={frames}, peak={peak:.6}",
        output.status
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_error_reports_and_wakes_even_when_ui_is_idle() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        let (sender, receiver) = mpsc::channel();
        let wake = Arc::new(OnceLock::<rune::Waker>::new());
        let woken = Arc::new(AtomicBool::new(false));
        let flag = woken.clone();
        assert!(
            wake.set(Arc::new(move || {
                flag.store(true, Relaxed);
            }))
            .is_ok()
        );
        ErrorSink { sender, wake }.report("device disconnected".into());
        assert_eq!(receiver.try_recv().unwrap(), "device disconnected");
        assert!(woken.load(Relaxed));
    }

    #[test]
    fn virtual_time_is_capped_and_full_queue_counts_drops() {
        let mut output = Output::virtual_driver(Slots::empty(), "test".into(), false);
        for _ in 0..QUEUE_CAPACITY + 3 {
            output.send(Command::Panic);
        }
        assert_eq!(output.dropped, 3);
        output.advance(10.0);
        assert_eq!(
            output
                .meters
                .frames
                .load(std::sync::atomic::Ordering::Relaxed),
            11_025
        );
    }

    fn format_round_trip<T>()
    where
        T: cpal::Sample + cpal::FromSample<f32>,
        f32: cpal::FromSample<T>,
    {
        let samples = vec![Sample {
            data: Arc::from([0.5; 8]),
            rate: 44_100,
        }];
        let mut mixer = Mixer::new(
            Slots::from_samples(samples),
            44_100,
            Arc::new(Meters::default()),
            rtrb::RingBuffer::new(SLOTS).0,
        );
        mixer.command(Command::Trigger {
            pad: 0,
            slot: 0,
            group: 0,
            velocity: 1.0,
            gain: 1.0,
            pan: -1.0,
            semitones: 0.0,
        });
        let mut output = [T::from_sample(0.0); 12];
        mixer.render_into(&mut output, 4, T::from_sample);
        for frame in output.as_chunks::<4>().0 {
            assert!((frame[0].to_sample::<f32>() - 0.4).abs() < 1.0 / 32768.0);
            for sample in &frame[1..] {
                assert_eq!(sample.to_sample::<f32>(), 0.0);
            }
        }
    }

    #[test]
    fn all_four_device_formats_convert_signal_and_silence() {
        format_round_trip::<f32>();
        format_round_trip::<i16>();
        format_round_trip::<u16>();
        format_round_trip::<i32>();
    }
}
