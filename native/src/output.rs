use crate::engine::{Command, Meters, Mixer, QUEUE_CAPACITY, Returned, SLOTS, Sample, Slots};
use crate::performance::{
    MusicalPosition, OriginalTime, PerformanceError, SampleRate, StreamEpoch, Tempo,
    rounded_stream_frame,
    scheduler::{ScheduleResult, ScheduleSender, ScheduledEvent},
};
use cpal::{
    BufferSize, DeviceId, OutputStreamTimestamp, SampleFormat, Stream, StreamConfig, StreamInstant,
    SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    fmt,
    num::NonZeroU32,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering::Relaxed},
        mpsc,
    },
    time::Duration,
};

#[derive(Debug)]
pub enum AudioError {
    NoDevice,
    SelectedDeviceUnavailable,
    UnsupportedConfiguration,
    UnsupportedFormat(SampleFormat),
    Device(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => write!(f, "No default output device"),
            Self::SelectedDeviceUnavailable => write!(
                f,
                "Selected output is unavailable; retry it or explicitly choose system default"
            ),
            Self::UnsupportedConfiguration => {
                write!(
                    f,
                    "Unsupported output channel, sample-rate or buffer configuration"
                )
            }
            Self::UnsupportedFormat(format) => write!(f, "Unsupported audio format: {format}"),
            Self::Device(message) => write!(f, "Audio error: {message}"),
        }
    }
}
impl std::error::Error for AudioError {}

fn device_error(e: impl fmt::Display) -> AudioError {
    AudioError::Device(e.to_string())
}

/// Machine-local preference, never portable project data (D8).
/// A name is deliberately not an identity or a fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputSelection {
    /// None explicitly follows the system default, Some requires this CPAL ID.
    pub device_id: Option<DeviceId>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// A fixed size is a request, not a guarantee about callback buffer lengths.
    pub buffer_size: BufferSize,
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub default_config: SupportedStreamConfig,
    pub supported_configs: Vec<SupportedStreamConfigRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputConfiguration {
    pub stream: StreamConfig,
    pub format: SampleFormat,
}

impl OutputSelection {
    /// Resolve a discovery snapshot by ID only. Missing/stale IDs never select
    /// a namesake or silently change to the default.
    pub fn resolve<'a>(
        &self,
        devices: &'a [DeviceInfo],
        default_id: Option<&DeviceId>,
    ) -> Result<&'a DeviceInfo, AudioError> {
        let id = self
            .device_id
            .as_ref()
            .or(default_id)
            .ok_or(AudioError::NoDevice)?;
        devices
            .iter()
            .find(|device| &device.id == id)
            .ok_or(if self.device_id.is_some() {
                AudioError::SelectedDeviceUnavailable
            } else {
                AudioError::NoDevice
            })
    }
}

impl DeviceInfo {
    fn query(device: &cpal::Device) -> Result<Self, AudioError> {
        Ok(Self {
            id: device.id().map_err(device_error)?,
            name: device
                .description()
                .map_err(device_error)?
                .name()
                .to_owned(),
            default_config: device.default_output_config().map_err(device_error)?,
            supported_configs: device
                .supported_output_configs()
                .map_err(device_error)?
                .collect(),
        })
    }

    /// Unspecified fields retain this device's defaults, including sample
    /// format. Validate the whole tuple, not each field in unrelated ranges.
    /// Unknown buffer bounds permit only the platform default.
    pub fn configuration(
        &self,
        selection: &OutputSelection,
    ) -> Result<OutputConfiguration, AudioError> {
        let format = self.default_config.sample_format();
        if !matches!(
            format,
            SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16 | SampleFormat::I32
        ) {
            return Err(AudioError::UnsupportedFormat(format));
        }
        let stream = StreamConfig {
            channels: selection.channels.unwrap_or(self.default_config.channels()),
            sample_rate: selection
                .sample_rate
                .unwrap_or(self.default_config.sample_rate()),
            buffer_size: selection.buffer_size,
        };
        let valid = stream.channels > 0
            && stream.sample_rate > 0
            && self.supported_configs.iter().any(|range| {
                range.channels() == stream.channels
                    && range.sample_format() == format
                    && range.contains_rate(stream.sample_rate)
                    && match (stream.buffer_size, range.buffer_size()) {
                        (BufferSize::Default, _) => true,
                        (BufferSize::Fixed(size), SupportedBufferSize::Range { min, max }) => {
                            size > 0 && (*min..=*max).contains(&size)
                        }
                        (BufferSize::Fixed(_), SupportedBufferSize::Unknown) => false,
                    }
            });
        if !valid {
            return Err(AudioError::UnsupportedConfiguration);
        }
        Ok(OutputConfiguration { stream, format })
    }
}

/// Off-thread discovery, with CPAL's host-qualified stable IDs and raw ranges.
/// Device IDs can contain identifying information; do not publish them in logs.
pub fn output_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    cpal::default_host()
        .output_devices()
        .map_err(device_error)?
        .map(|device| DeviceInfo::query(&device))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

impl StreamId {
    pub fn epoch(self) -> StreamEpoch {
        StreamEpoch(self.0)
    }
}

/// One callback's first output frame and CPAL's predicted presentation time.
/// Instants may only be compared within the same stream_id. This is not a
/// wall-clock mapping, a scheduler, or a round-trip latency measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackTimestamp {
    pub stream_id: StreamId,
    pub first_frame: u64,
    pub frames: usize,
    pub sample_rate: NonZeroU32,
    pub timestamp: OutputStreamTimestamp,
}

/// Off-thread conversion in one stream's CPAL host-clock domain. The caller
/// supplies take time at predicted playback, not at callback invocation.
/// This does not equate UI Instant or MIDI driver clocks with CPAL instants.
#[derive(Clone, Copy, Debug)]
pub struct PlaybackScheduleClock {
    pub observation: PlaybackTimestamp,
    pub take_time_at_playback: OriginalTime,
}

impl PlaybackScheduleClock {
    fn check(&self, epoch: StreamEpoch) -> Result<(), PerformanceError> {
        if epoch != self.observation.stream_id.epoch() {
            return Err(PerformanceError::StaleEpoch);
        }
        Ok(())
    }

    fn delta_nanos(&self, instant: StreamInstant) -> i128 {
        let playback = self.observation.timestamp.playback;
        if instant >= playback {
            instant.duration_since(playback).as_nanos() as i128
        } else {
            -(playback.duration_since(instant).as_nanos() as i128)
        }
    }

    pub fn original_time(
        &self,
        epoch: StreamEpoch,
        instant: StreamInstant,
    ) -> Result<OriginalTime, PerformanceError> {
        self.check(epoch)?;
        let n = i128::from(self.take_time_at_playback.nanos()) + self.delta_nanos(instant);
        if n < 0 {
            return Err(PerformanceError::BeforeTake);
        }
        u64::try_from(n)
            .map(OriginalTime::from_nanos)
            .map_err(|_| PerformanceError::Overflow)
    }

    pub fn frame_for_host(
        &self,
        epoch: StreamEpoch,
        instant: StreamInstant,
    ) -> Result<u64, PerformanceError> {
        self.check(epoch)?;
        let n = i128::from(self.observation.first_frame) * 1_000_000_000
            + self.delta_nanos(instant) * i128::from(self.observation.sample_rate.get());
        rounded_stream_frame(n, 1_000_000_000)
    }

    pub fn frame_for_original(
        &self,
        epoch: StreamEpoch,
        time: OriginalTime,
    ) -> Result<u64, PerformanceError> {
        self.check(epoch)?;
        let delta = i128::from(time.nanos()) - i128::from(self.take_time_at_playback.nanos());
        let n = i128::from(self.observation.first_frame) * 1_000_000_000
            + delta * i128::from(self.observation.sample_rate.get());
        rounded_stream_frame(n, 1_000_000_000)
    }

    pub fn frame_for_position(
        &self,
        epoch: StreamEpoch,
        position: MusicalPosition,
        tempo: Tempo,
    ) -> Result<u64, PerformanceError> {
        self.check(epoch)?;
        position.at_tempo_stream_frame(
            tempo,
            SampleRate::try_from(self.observation.sample_rate.get())?,
            self.take_time_at_playback,
            self.observation.first_frame,
        )
    }
}

const TIMESTAMP_CAPACITY: usize = 64;

/// Construct off-thread, then move into the callback. Publishing is bounded,
/// allocation/lock/I/O-free, and advances frame identity even when full.
pub struct PlaybackClock {
    stream_id: StreamId,
    sample_rate: NonZeroU32,
    next_frame: u64,
    sender: rtrb::Producer<PlaybackTimestamp>,
    dropped: Arc<AtomicU64>,
}

pub struct PlaybackTimestamps {
    pub stream_id: StreamId,
    receiver: rtrb::Consumer<PlaybackTimestamp>,
    dropped: Arc<AtomicU64>,
}

impl PlaybackClock {
    pub fn new(sample_rate: NonZeroU32) -> (Self, PlaybackTimestamps) {
        let stream_id = StreamId(StreamEpoch::fresh().0);
        let (sender, receiver) = rtrb::RingBuffer::new(TIMESTAMP_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        (
            Self {
                stream_id,
                sample_rate,
                next_frame: 0,
                sender,
                dropped: dropped.clone(),
            },
            PlaybackTimestamps {
                stream_id,
                receiver,
                dropped,
            },
        )
    }

    pub fn publish(&mut self, frames: usize, timestamp: OutputStreamTimestamp) {
        let item = PlaybackTimestamp {
            stream_id: self.stream_id,
            first_frame: self.next_frame,
            frames,
            sample_rate: self.sample_rate,
            timestamp,
        };
        self.next_frame = self.next_frame.saturating_add(frames as u64);
        if self.sender.push(item).is_err() {
            self.dropped.fetch_add(1, Relaxed);
        }
    }
}

impl PlaybackTimestamps {
    /// Drain a bounded queue on the consumer thread. Overflow drops new
    /// observations, not audio; first_frame still counts every rendered frame.
    pub fn take_latest(&mut self) -> Option<PlaybackTimestamp> {
        let mut latest = None;
        for _ in 0..TIMESTAMP_CAPACITY {
            match self.receiver.pop() {
                Ok(item) => latest = Some(item),
                Err(_) => break,
            }
        }
        latest
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct OpenedOutput {
    pub device_id: DeviceId,
    pub name: String,
    pub configuration: OutputConfiguration,
    pub stream_id: StreamId,
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
    selection: OutputSelection,
    opened: Option<OpenedOutput>,
    timestamps: Option<PlaybackTimestamps>,
    wake: Arc<OnceLock<rune::Waker>>,
    pub commands: rtrb::Producer<Command>,
    schedule: ScheduleSender,
    /// Samples the mixer has finished with, for the UI thread to drop.
    retired: rtrb::Consumer<Returned>,
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
        let mut mixer = Mixer::new(slots.into(), 44_100, meters.clone(), retired_commands);
        let Some(schedule) = mixer.take_scheduler() else {
            unreachable!("new mixer has its unique producer");
        };
        Self {
            schedule,
            selection: OutputSelection::default(),
            opened: None,
            timestamps: None,
            driver: Driver::Virtual {
                mixer: Box::new(mixer),
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
        Self::open_selected(slots, &OutputSelection::default())
    }

    /// Every open creates a fresh mixer; replacing an Output stops its voices.
    /// Explicit selections are never replaced with a silent default fallback.
    pub fn open_selected(
        slots: impl Into<Slots>,
        selection: &OutputSelection,
    ) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = match &selection.device_id {
            Some(id) => host
                .device_by_id(id)
                .ok_or(AudioError::SelectedDeviceUnavailable)?,
            None => host.default_output_device().ok_or(AudioError::NoDevice)?,
        };
        let info = DeviceInfo::query(&device)?;
        let configuration = info.configuration(selection)?;
        let OutputConfiguration {
            stream: config,
            format,
        } = configuration;
        let rate =
            NonZeroU32::new(config.sample_rate).ok_or(AudioError::UnsupportedConfiguration)?;
        let (clock, timestamps) = PlaybackClock::new(rate);
        let opened = OpenedOutput {
            device_id: info.id,
            name: info.name,
            configuration,
            stream_id: timestamps.stream_id,
        };
        let status = format!(
            "Output: {} · {} Hz · {format:?} · {} ch",
            opened.name, config.sample_rate, config.channels
        );
        let (commands, consumer) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let (retired_commands, retired) = rtrb::RingBuffer::new(SLOTS);
        let meters = Arc::new(Meters::default());
        let mut mixer = Mixer::new_with_epoch(
            slots.into(),
            config.sample_rate,
            meters.clone(),
            retired_commands,
            timestamps.stream_id.epoch(),
        );
        let Some(schedule) = mixer.take_scheduler() else {
            unreachable!("new mixer has its unique producer");
        };
        let (send_error, errors) = mpsc::channel();
        let wake = Arc::new(OnceLock::new());
        let send_error = ErrorSink {
            sender: send_error,
            wake: wake.clone(),
        };
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, mixer, consumer, send_error, clock),
            SampleFormat::I16 => build::<i16>(&device, config, mixer, consumer, send_error, clock),
            SampleFormat::U16 => build::<u16>(&device, config, mixer, consumer, send_error, clock),
            SampleFormat::I32 => build::<i32>(&device, config, mixer, consumer, send_error, clock),
            other => return Err(AudioError::UnsupportedFormat(other)),
        }?;
        stream.play().map_err(device_error)?;
        Ok(Self {
            schedule,
            selection: selection.clone(),
            opened: Some(opened),
            timestamps: Some(timestamps),
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

    pub fn selection(&self) -> &OutputSelection {
        &self.selection
    }

    /// None for a silent mixer: virtual time never claims device timestamps.
    pub fn opened(&self) -> Option<&OpenedOutput> {
        self.opened.as_ref()
    }

    pub fn timestamps(&mut self) -> Option<&mut PlaybackTimestamps> {
        self.timestamps.as_mut()
    }

    /// The virtual mixer also has an epoch/cursor, but never device timestamps.
    pub fn scheduler(&mut self) -> &mut ScheduleSender {
        &mut self.schedule
    }

    pub fn schedule(&mut self, event: ScheduledEvent) -> ScheduleResult {
        self.schedule.try_schedule(event)
    }

    pub fn silent_selected(
        slots: impl Into<Slots>,
        selection: OutputSelection,
        reason: String,
    ) -> Self {
        let mut output = Self::virtual_driver(
            slots,
            format!("Silent: {reason} · Playing voices stopped"),
            true,
        );
        output.selection = selection;
        output
    }

    /// Retry exactly the remembered preference, preserving it on failure.
    /// System-default fallback requires a separate explicit selection.
    pub fn retry(&self, slots: impl Into<Slots>) -> Self {
        Self::retry_with(slots.into(), &self.selection, Self::open_selected)
    }

    fn retry_with(
        slots: Slots,
        selection: &OutputSelection,
        open: impl FnOnce(Slots, &OutputSelection) -> Result<Self, AudioError>,
    ) -> Self {
        match open(slots.clone(), selection) {
            Ok(mut output) => {
                output.status.push_str(" · Playing voices stopped");
                output
            }
            Err(error) => Self::silent_selected(slots, selection.clone(), error.to_string()),
        }
    }

    /// Queues a command. Returns false when the queue was full and the command
    /// was dropped, which lifecycle commands (`Load`, `Unload`) must not
    /// ignore: the slot they name would be reserved by the UI and left empty in
    /// the mixer. Triggers and settings may ignore it.
    pub fn send(&mut self, command: Command) -> bool {
        if self.commands.push(command).is_err() {
            self.dropped += 1;
            return false;
        }
        true
    }

    /// Samples the mixer has finished with, with the slot each came from. Call
    /// from the UI thread; dropping the result drops the samples there, never
    /// in the callback.
    pub fn take_retired(&mut self) -> Vec<Returned> {
        let mut items = Vec::new();
        while let Ok(item) = self.retired.pop() {
            items.push(item);
        }
        items
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
    mut clock: PlaybackClock,
) -> Result<Stream, AudioError> {
    let channels = usize::from(config.channels);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], info| {
                clock.publish(data.len() / channels, info.timestamp());
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
    fn failed_retry_passes_exact_selection_and_slots_and_remembers_them() {
        let selection = OutputSelection {
            device_id: Some(DeviceId::new(cpal::default_host().id(), "missing")),
            sample_rate: Some(48_000),
            ..Default::default()
        };
        let slots = Slots::from_samples([Sample {
            data: Arc::from([0.1; 8]),
            rate: 44_100,
        }]);
        let failed = Output::retry_with(slots, &selection, |slots, requested| {
            assert_eq!(requested, &selection);
            assert!(slots.get(0).is_some());
            Err(AudioError::SelectedDeviceUnavailable)
        });
        assert_eq!(failed.selection(), &selection);
        assert!(failed.failed);
        assert!(failed.opened().is_none());
        assert!(failed.status.contains("explicitly choose system default"));
        assert!(failed.status.contains("voices stopped"));
    }

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
