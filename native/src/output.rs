use crate::engine::{Command, Meters, Mixer, QUEUE_CAPACITY, Returned, SLOTS, Sample, Slots};
use crate::performance::{
    MusicalPosition, OriginalTime, PerformanceError, SampleRate, StreamEpoch, Tempo,
    rounded_stream_frame,
    scheduler::{ScheduleCounters, ScheduleResult, ScheduleSender, ScheduledEvent},
};
use crate::plugins::live::LiveVoice;
use cpal::{
    BufferSize, DeviceId, OutputStreamTimestamp, Sample as CpalSample, SampleFormat, Stream,
    StreamConfig, StreamInstant, SupportedBufferSize, SupportedStreamConfig,
    SupportedStreamConfigRange,
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
    time::{Duration, Instant},
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

/// What one output callback cost and what CPAL predicted around it. `lead` is
/// None when the delivered prediction does not follow the callback instant; an
/// unavailable prediction is counted, never coerced to zero. Neither value is
/// round-trip latency, which output-only instants cannot measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackMeasurement {
    pub stream_id: StreamId,
    /// Publication order within one probe, starting at zero. Overflow skips
    /// numbers; a consumer rejects anything that does not advance.
    pub sequence: u64,
    /// Frames CPAL actually delivered, never a requested buffer size.
    pub frames: usize,
    /// Wall time inside the callback body: timestamp publish, command drain,
    /// sample render and live mix. No lock is held or taken to measure it.
    pub duration: Duration,
    /// `playback - callback` from this callback's own CPAL timestamp.
    pub lead: Option<Duration>,
}

const PROBE_CAPACITY: usize = 64;

/// Construct off-thread, then move into the callback with
/// [`Callback::set_probe`]. Publishing is bounded, allocation, lock, I/O and
/// wait free, and touches no samples.
pub struct CallbackProbe {
    stream_id: StreamId,
    sequence: u64,
    sender: rtrb::Producer<CallbackMeasurement>,
    dropped: Arc<AtomicU64>,
}

pub struct CallbackMeasurements {
    pub stream_id: StreamId,
    receiver: rtrb::Consumer<CallbackMeasurement>,
    dropped: Arc<AtomicU64>,
}

impl CallbackMeasurements {
    /// Observations the audio thread could not enqueue because the bounded
    /// queue was full. They are counted, never blocked on.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }
}

impl CallbackProbe {
    pub fn new(stream_id: StreamId) -> (Self, CallbackMeasurements) {
        let (sender, receiver) = rtrb::RingBuffer::new(PROBE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        (
            Self {
                stream_id,
                sequence: 0,
                sender,
                dropped: dropped.clone(),
            },
            CallbackMeasurements {
                stream_id,
                receiver,
                dropped,
            },
        )
    }

    /// Report the callback that just finished. Overflow drops the newest
    /// observation, never audio, and never the running stream.
    pub fn publish(&mut self, duration: Duration, frames: usize, timestamp: OutputStreamTimestamp) {
        let item = CallbackMeasurement {
            stream_id: self.stream_id,
            sequence: self.sequence,
            frames,
            duration,
            lead: presentation_lead(timestamp),
        };
        self.sequence = self.sequence.saturating_add(1);
        if self.sender.push(item).is_err() {
            self.dropped.fetch_add(1, Relaxed);
        }
    }
}

/// CPAL predicts playback after the callback instant it reports. A prediction
/// at or before it is unavailable, not a zero-length lead, and neither is a
/// round-trip measurement.
fn presentation_lead(timestamp: OutputStreamTimestamp) -> Option<Duration> {
    timestamp
        .playback
        .checked_duration_since(timestamp.callback)
}

/// Per-distribution ceiling for the consumer-side summary. The audio thread
/// never stores values; the consumer keeps at most this many and counts the
/// rest, so a long probe cannot grow without bound.
const SUMMARY_CAPACITY: usize = 4096;

#[derive(Default)]
struct Distribution {
    values: Vec<u64>,
    total: u64,
    min: u64,
    max: u64,
}

impl Distribution {
    fn add(&mut self, nanos: u64) {
        if self.total == 0 {
            self.min = nanos;
            self.max = nanos;
        } else {
            self.min = self.min.min(nanos);
            self.max = self.max.max(nanos);
        }
        self.total += 1;
        if self.values.len() < SUMMARY_CAPACITY {
            self.values.push(nanos);
        }
    }

    /// Nearest-rank p50 and p95 over the kept observations, in nanos, sorting
    /// once. No interpolation, so the same observations always report the same
    /// numbers. None when nothing was observed.
    fn percentiles(&self) -> Option<(u64, u64)> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_unstable();
        let rank = |percent: usize| {
            sorted
                .len()
                .saturating_mul(percent)
                .div_ceil(100)
                .clamp(1, sorted.len())
        };
        Some((sorted[rank(50) - 1], sorted[rank(95) - 1]))
    }
}

/// Off-thread summary of [`CallbackMeasurement`]s for one stream.
/// [`CallbackSummary::absorb`] drains the bounded queue on the consumer thread;
/// percentiles interpolate nothing, so a report is reproducible from the same
/// observations.
#[derive(Default)]
pub struct CallbackSummary {
    stream_id: Option<StreamId>,
    next_sequence: Option<u64>,
    callbacks: u64,
    dropped: u64,
    rejected: u64,
    inverted_lead: u64,
    frames_min: u64,
    frames_max: u64,
    durations: Distribution,
    leads: Distribution,
}

impl CallbackSummary {
    /// Drain every queued observation. Foreign streams, duplicate sequences
    /// and out-of-order sequences are counted as rejected and never merged, so
    /// a replayed or reordered queue cannot invent a measurement.
    pub fn absorb(&mut self, measurements: &mut CallbackMeasurements) {
        self.dropped = measurements.dropped();
        for _ in 0..PROBE_CAPACITY {
            let Ok(item) = measurements.receiver.pop() else {
                break;
            };
            self.observe(item);
        }
    }

    fn observe(&mut self, item: CallbackMeasurement) {
        let same_stream = match self.stream_id {
            None => {
                self.stream_id = Some(item.stream_id);
                true
            }
            Some(seen) => seen == item.stream_id,
        };
        if !same_stream || self.next_sequence.is_some_and(|next| item.sequence < next) {
            self.rejected += 1;
            return;
        }
        self.next_sequence = Some(item.sequence.saturating_add(1));
        self.callbacks += 1;
        let frames = item.frames as u64;
        if self.callbacks == 1 {
            self.frames_min = frames;
            self.frames_max = frames;
        } else {
            self.frames_min = self.frames_min.min(frames);
            self.frames_max = self.frames_max.max(frames);
        }
        self.durations.add(nanos(item.duration));
        match item.lead {
            Some(lead) => self.leads.add(nanos(lead)),
            None => self.inverted_lead += 1,
        }
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Whole microseconds, rounded, so a stable text block never hides a sub-unit
/// observation behind a truncated zero.
fn micros(nanos: u64) -> u64 {
    nanos.saturating_add(500) / 1_000
}

fn summary(distribution: &Distribution) -> String {
    match distribution.percentiles() {
        Some((p50, p95)) => format!(
            "min={}, p50={}, p95={}, n={}",
            micros(distribution.min),
            micros(p50),
            micros(p95),
            distribution.total
        ),
        None => "min=none, p50=none, p95=none, n=0".into(),
    }
}

impl fmt::Display for CallbackSummary {
    /// The stable, parseable `--check-audio` measurement block. Round-trip
    /// latency is labelled unmeasured because output-only timestamps cannot
    /// derive it; nothing here is invented.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "callbacks={}, dropped={}, rejected={}, inverted_lead={}",
            self.callbacks, self.dropped, self.rejected, self.inverted_lead
        )?;
        writeln!(f, "callback_duration_us={}", summary(&self.durations))?;
        if self.callbacks == 0 {
            f.write_str("callback_frames=min=none, max=none, n=0\n")?;
        } else {
            writeln!(
                f,
                "callback_frames=min={}, max={}, n={}",
                self.frames_min, self.frames_max, self.callbacks
            )?;
        }
        writeln!(f, "presentation_lead_us={}", summary(&self.leads))?;
        write!(
            f,
            "round_trip_latency=unmeasured (output-only callback and presentation \
             instants cannot derive it)"
        )
    }
}

/// Frames mixed from the live instrument per scratch pass, so a helper block is
/// never larger than this preallocated buffer.
const LIVE_CHUNK_FRAMES: usize = 1024;

/// Everything one output stream's data callback owns. Construct it (and attach
/// a live voice with [`Callback::set_live`]) off the callback thread; `process`
/// then allocates, frees, locks, and waits for nothing.
pub struct Callback {
    clock: PlaybackClock,
    mixer: Mixer,
    commands: rtrb::Consumer<Command>,
    channels: usize,
    live: Option<LiveVoice>,
    live_scratch: Vec<f32>,
    /// Absent on the normal app path; only a probe-enabled open measures.
    probe: Option<CallbackProbe>,
}

impl Callback {
    pub fn new(
        clock: PlaybackClock,
        mixer: Mixer,
        commands: rtrb::Consumer<Command>,
        channels: usize,
    ) -> Self {
        Self {
            clock,
            mixer,
            commands,
            channels,
            live: None,
            live_scratch: vec![0.0; LIVE_CHUNK_FRAMES * channels.max(1)],
            probe: None,
        }
    }

    /// Measure every call from now on. Call only while the stream is not
    /// running, like [`Callback::set_live`].
    pub fn set_probe(&mut self, probe: CallbackProbe) {
        self.probe = Some(probe);
    }

    /// Replace the live voice. Call only while the stream is not running: this
    /// is the off-callback open/close handoff, never a callback operation.
    pub fn set_live(&mut self, live: Option<LiveVoice>) {
        self.live = live;
    }

    pub fn live_frame(&self) -> Option<u64> {
        self.live.as_ref().map(LiveVoice::frame)
    }

    /// The real output callback body, with its wall-clock cost and CPAL's
    /// presentation lead measured around it when a probe is attached. The
    /// measurement is two `Instant` reads outside every lock; it neither
    /// allocates nor blocks, and it changes no sample.
    pub fn process<T: cpal::SizedSample + cpal::FromSample<f32>>(
        &mut self,
        data: &mut [T],
        timestamp: OutputStreamTimestamp,
    ) where
        f32: cpal::FromSample<T>,
    {
        let started = self.probe.is_some().then(Instant::now);
        self.render(data, timestamp);
        let (Some(started), Some(probe)) = (started, self.probe.as_mut()) else {
            return;
        };
        let frames = data.len().checked_div(self.channels).unwrap_or(0);
        probe.publish(started.elapsed(), frames, timestamp);
    }

    /// Publish timing, drain commands, render the sample mixer, then add the
    /// already-rendered live instrument block. The live voice contributes
    /// silence when its worker has not produced audio, so a failed or starved
    /// helper never silences the mixer.
    fn render<T: cpal::SizedSample + cpal::FromSample<f32>>(
        &mut self,
        data: &mut [T],
        timestamp: OutputStreamTimestamp,
    ) where
        f32: cpal::FromSample<T>,
    {
        let channels = self.channels;
        if channels == 0 {
            self.clock.publish(0, timestamp);
            return;
        }
        self.clock.publish(data.len() / channels, timestamp);
        self.mixer.drain(&mut self.commands);
        self.mixer.render_into(data, channels, T::from_sample);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        for chunk in data.chunks_mut(self.live_scratch.len()) {
            let count = chunk.len();
            self.live_scratch[..count].fill(0.0);
            live.render_add(&mut self.live_scratch[..count], channels);
            for (sample, extra) in chunk.iter_mut().zip(&self.live_scratch[..count]) {
                *sample = T::from_sample(f32::from_sample(*sample) + *extra);
            }
        }
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
    measurements: Option<CallbackMeasurements>,
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
            measurements: None,
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
        Self::open_selected_with_live(slots, selection, |_, _| (None, None))
    }

    /// Opens the stream exactly like [`Output::open_selected`], with one
    /// optional live instrument voice. `live` runs after the device
    /// configuration is known but before the stream starts, so the voice can
    /// be created at the stream's rate and buffer size and moved in without the
    /// callback ever opening or closing a helper. The string is a status note
    /// for a voice that could not start; the sample mixer still plays.
    pub fn open_selected_with_live(
        slots: impl Into<Slots>,
        selection: &OutputSelection,
        live: impl FnOnce(u32, u32) -> (Option<LiveVoice>, Option<String>),
    ) -> Result<Self, AudioError> {
        Self::open_inner(slots, selection, live, false)
    }

    /// The system-default wrapper used by `--check-audio`: it additionally
    /// measures every callback's wall duration, observed buffer length and
    /// CPAL presentation lead, for [`CallbackSummary`] to read off-thread.
    /// The normal app path never opts in, so it carries no measurement cost.
    fn open_probed(slots: impl Into<Slots>) -> Result<Self, AudioError> {
        Self::open_inner(
            slots,
            &OutputSelection::default(),
            |_, _| (None, None),
            true,
        )
    }

    fn open_inner(
        slots: impl Into<Slots>,
        selection: &OutputSelection,
        live: impl FnOnce(u32, u32) -> (Option<LiveVoice>, Option<String>),
        probing: bool,
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
        let (probe, measurements) = if probing {
            let (probe, measurements) = CallbackProbe::new(timestamps.stream_id);
            (Some(probe), Some(measurements))
        } else {
            (None, None)
        };
        let opened = OpenedOutput {
            device_id: info.id,
            name: info.name,
            configuration,
            stream_id: timestamps.stream_id,
        };
        let buffer_frames = match config.buffer_size {
            BufferSize::Fixed(frames) => frames,
            BufferSize::Default => 512,
        };
        let (live, live_status) = live(config.sample_rate, buffer_frames);
        let mut status = format!(
            "Output: {} · {} Hz · {format:?} · {} ch",
            opened.name, config.sample_rate, config.channels
        );
        if let Some(note) = live_status {
            status.push_str(" · ");
            status.push_str(&note);
        }
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
        let mut callback = Callback::new(clock, mixer, consumer, usize::from(config.channels));
        callback.set_live(live);
        if let Some(probe) = probe {
            callback.set_probe(probe);
        }
        let (send_error, errors) = mpsc::channel();
        let wake = Arc::new(OnceLock::new());
        let send_error = ErrorSink {
            sender: send_error,
            wake: wake.clone(),
        };
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, callback, send_error),
            SampleFormat::I16 => build::<i16>(&device, config, callback, send_error),
            SampleFormat::U16 => build::<u16>(&device, config, callback, send_error),
            SampleFormat::I32 => build::<i32>(&device, config, callback, send_error),
            other => return Err(AudioError::UnsupportedFormat(other)),
        }?;
        stream.play().map_err(device_error)?;
        Ok(Self {
            schedule,
            selection: selection.clone(),
            opened: Some(opened),
            timestamps: Some(timestamps),
            measurements,
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

    /// Bounded callback measurements. Present only for the probe-enabled open
    /// that `--check-audio` uses; the app's normal open never measures.
    pub fn measurements(&mut self) -> Option<&mut CallbackMeasurements> {
        self.measurements.as_mut()
    }

    /// The virtual mixer also has an epoch/cursor, but never device timestamps.
    pub fn scheduler(&mut self) -> &mut ScheduleSender {
        &mut self.schedule
    }

    pub fn schedule_counters(&self) -> ScheduleCounters {
        self.schedule.counters()
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
    mut callback: Callback,
    errors: ErrorSink,
) -> Result<Stream, AudioError>
where
    f32: cpal::FromSample<T>,
{
    device
        .build_output_stream(
            config,
            move |data: &mut [T], info| {
                callback.process(data, info.timestamp());
            },
            move |error| {
                errors.report(error.to_string());
            },
            None,
        )
        .map_err(device_error)
}

/// How long `--check-audio` plays pad 0, and how often it drains the bounded
/// callback queue so a two-second run does not overflow it.
const CHECK_AUDIO_WINDOW: Duration = Duration::from_secs(2);
const CHECK_AUDIO_POLL: Duration = Duration::from_millis(10);

pub fn check_audio(samples: Box<[Sample]>) -> Result<String, AudioError> {
    if samples.first().is_none_or(|s| s.data.is_empty()) {
        return Err(AudioError::Device(
            "pad 0 sample is missing or invalid".into(),
        ));
    }
    let mut output = Output::open_probed(samples)?;
    output.send(Command::Trigger {
        pad: 0,
        slot: 0,
        group: 0,
        velocity: 1.0,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    });
    let deadline = Instant::now() + CHECK_AUDIO_WINDOW;
    let mut timing = CallbackSummary::default();
    while Instant::now() < deadline {
        std::thread::sleep(CHECK_AUDIO_POLL);
        if let Some(measurements) = output.measurements() {
            timing.absorb(measurements);
        }
    }
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
        "{}\nframes={frames}, peak={peak:.6}\n{timing}",
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

    #[test]
    fn replayed_reordered_and_foreign_observations_are_rejected_not_merged() {
        let (_clock, observations) = PlaybackClock::new(NonZeroU32::new(48_000).unwrap());
        let stream_id = observations.stream_id;
        let foreign = StreamId(stream_id.0 ^ 1);
        let measurement = |sequence: u64, stream_id: StreamId| CallbackMeasurement {
            stream_id,
            sequence,
            frames: 128,
            duration: Duration::from_micros(500),
            lead: Some(Duration::from_millis(4)),
        };
        let mut summary = CallbackSummary::default();
        summary.observe(measurement(0, stream_id));
        summary.observe(measurement(0, stream_id)); // Replayed sequence.
        summary.observe(measurement(2, stream_id)); // Queue overflow skipped 1.
        summary.observe(measurement(1, stream_id)); // Out of order.
        summary.observe(measurement(3, foreign)); // Another stream entirely.
        assert_eq!(
            summary.to_string(),
            "callbacks=2, dropped=0, rejected=3, inverted_lead=0\n\
             callback_duration_us=min=500, p50=500, p95=500, n=2\n\
             callback_frames=min=128, max=128, n=2\n\
             presentation_lead_us=min=4000, p50=4000, p95=4000, n=2\n\
             round_trip_latency=unmeasured (output-only callback and presentation \
             instants cannot derive it)"
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
