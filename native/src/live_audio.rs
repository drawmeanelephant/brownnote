//! Stereo float WAV for audio a live instrument played into a take (#112).
//!
//! This is retained project media (D4), not a render recipe: the exact
//! interleaved samples the live callback played are encoded once and later
//! decoded to two mono channels for the mixer. Export copies the encoded bytes
//! unchanged, so the file stays byte-for-byte project media.
use crate::pedals::offline::{MAX_RATE, MIN_RATE};
use std::{
    io::{self, Cursor, Read},
    sync::Arc,
};

/// Interleaved stereo audio at the live stream's rate.
#[derive(Clone, Debug, PartialEq)]
pub struct StereoClip {
    pub rate: u32,
    pub frames: u64,
    pub left: Arc<[f32]>,
    pub right: Arc<[f32]>,
}

impl StereoClip {
    /// Split interleaved stereo samples into the two channels the mixer plays.
    pub fn from_interleaved(rate: u32, samples: &[f32]) -> Self {
        let frames = samples.len() / 2;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for frame in samples.as_chunks::<2>().0.iter().take(frames) {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        Self {
            rate,
            frames: frames as u64,
            left: left.into(),
            right: right.into(),
        }
    }
}

/// Encode interleaved stereo float32 samples as a canonical float WAV.
pub fn encode(rate: u32, interleaved: &[f32]) -> io::Result<Vec<u8>> {
    if !(MIN_RATE..=MAX_RATE).contains(&rate) || interleaved.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "live take audio needs a valid rate and at least one stereo frame",
        ));
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut bytes = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut bytes, spec)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        for sample in interleaved.as_chunks::<2>().0 {
            writer
                .write_sample(sample[0])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            writer
                .write_sample(sample[1])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        }
        writer
            .finalize()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    }
    Ok(bytes.into_inner())
}

/// Decode only the exact format this module writes. `max_frames` bounds the
/// allocation by the project's encoded bytes, like retained pad renders.
pub fn decode(bytes: &[u8], max_frames: u64) -> Result<StereoClip, String> {
    let mut reader =
        hound::WavReader::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let spec = reader.spec();
    if spec.channels != 2
        || spec.sample_format != hound::SampleFormat::Float
        || spec.bits_per_sample != 32
    {
        return Err("live take audio is not stereo 32-bit float WAV".into());
    }
    if !(MIN_RATE..=MAX_RATE).contains(&spec.sample_rate) {
        return Err("live take audio sample rate is out of range".into());
    }
    let limit = u64::from(reader.duration());
    if limit == 0 || limit > max_frames {
        return Err(format!(
            "live take audio length must be 1..={max_frames} frames"
        ));
    }
    let mut left = Vec::with_capacity(limit as usize);
    let mut right = Vec::with_capacity(limit as usize);
    let samples = reader.samples::<f32>();
    for sample in samples {
        let value = sample.map_err(|error| error.to_string())?;
        if !value.is_finite() {
            return Err("live take audio contains a non-finite sample".into());
        }
        if left.len() == right.len() {
            left.push(value);
        } else {
            right.push(value);
        }
    }
    let frames = left.len().min(right.len());
    left.truncate(frames);
    right.truncate(frames);
    if frames == 0 {
        return Err("live take audio has no complete stereo frames".into());
    }
    Ok(StereoClip {
        rate: spec.sample_rate,
        frames: frames as u64,
        left: left.into(),
        right: right.into(),
    })
}

/// Read a whole file for the bounded decoder above.
pub fn read(path: &std::path::Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "live take audio is too large",
        ));
    }
    Ok(bytes)
}
