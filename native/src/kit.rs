use crate::engine::Sample;
use std::{
    fmt,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

pub use crate::paths::DEFAULT_SAMPLES;
pub const FILES: [&str; 5] = [
    "fart_dry.wav",
    "fart_fuzz.wav",
    "fart_delay.wav",
    "fart_chorus.wav",
    "fart_chain.wav",
];

pub struct Pad {
    pub key: char,
    pub label: &'static str,
    pub sample: usize,
    pub semitones: f32,
}

macro_rules! pad {
    ($key:literal, $label:literal, $sample:literal, $pitch:literal) => {
        Pad {
            key: $key,
            label: $label,
            sample: $sample,
            semitones: $pitch,
        }
    };
}

pub const PADS: [Pad; 16] = [
    pad!('1', "DRY", 0, 0.0),
    pad!('2', "DRY -5", 0, -5.0),
    pad!('3', "SEWER", 0, -12.0),
    pad!('4', "SQUEAK", 0, 7.0),
    pad!('q', "FUZZ", 1, 0.0),
    pad!('w', "FUZZ +7", 1, 7.0),
    pad!('e', "FUZZ OCT", 1, 12.0),
    pad!('r', "SLUDGE", 1, -7.0),
    pad!('a', "DELAY", 2, 0.0),
    pad!('s', "DELAY -5", 2, -5.0),
    pad!('d', "CHORUS", 3, 0.0),
    pad!('f', "CHORUS +5", 3, 5.0),
    pad!('z', "CHAIN", 4, 0.0),
    pad!('x', "THUD", 4, -12.0),
    pad!('c', "CHORUS -7", 3, -7.0),
    pad!('v', "DELAY OCT", 2, 12.0),
];

#[derive(Debug)]
pub struct KitError {
    path: PathBuf,
    reason: String,
}

impl fmt::Display for KitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.reason)
    }
}
impl std::error::Error for KitError {}

/// A kit sample is at most 30 seconds long.
pub const KIT_SECONDS: u64 = 30;

pub fn load_wav(path: &Path) -> Result<Sample, KitError> {
    load_wav_max(path, |rate| u64::from(rate) * KIT_SECONDS)
}

/// `load_wav` for files that are legitimately longer than a kit sample, such
/// as a render whose pedal added a ring-out tail. `max` gets the file's rate
/// and returns the longest frame count to accept.
pub fn load_wav_max(path: &Path, max: impl Fn(u32) -> u64) -> Result<Sample, KitError> {
    let reader = std::fs::File::open(path).map_err(|e| KitError {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    decode_wav_max(BufReader::new(reader), path, max)
}

/// Decode the same byte snapshot that import hashes and publishes.
pub fn decode_wav(reader: impl Read, path: &Path) -> Result<Sample, KitError> {
    decode_wav_max(reader, path, |rate| u64::from(rate) * KIT_SECONDS)
}

fn decode_wav_max(
    reader: impl Read,
    path: &Path,
    max: impl Fn(u32) -> u64,
) -> Result<Sample, KitError> {
    let error = |reason: String| KitError {
        path: path.to_path_buf(),
        reason,
    };
    let mut reader = hound::WavReader::new(reader).map_err(|e| error(e.to_string()))?;
    let spec = reader.spec();
    if !matches!(spec.channels, 1 | 2) {
        return Err(error("expected mono or stereo".into()));
    }
    if !(8_000..=192_000).contains(&spec.sample_rate) {
        return Err(error("sample rate must be 8000..192000 Hz".into()));
    }
    if !matches!(
        (spec.sample_format, spec.bits_per_sample),
        (hound::SampleFormat::Int, 16) | (hound::SampleFormat::Float, 32)
    ) {
        return Err(error("expected 16-bit PCM or 32-bit float WAV".into()));
    }
    let frames = reader.duration();
    let limit = max(spec.sample_rate);
    if frames == 0 || u64::from(frames) > limit {
        return Err(error(format!(
            "length must be 1 frame..{limit} frames at {} Hz",
            spec.sample_rate
        )));
    }
    let values: Result<Vec<f32>, _> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|v| v.map(|v| f32::from(v) / 32768.0))
            .collect(),
        hound::SampleFormat::Float => reader.samples::<f32>().collect(),
    };
    let values = values.map_err(|e| error(e.to_string()))?;
    if values.len() != frames as usize * usize::from(spec.channels)
        || values.iter().any(|v| !v.is_finite())
    {
        return Err(error("incomplete frames or non-finite sample".into()));
    }
    let data: Arc<[f32]> = values
        .chunks_exact(usize::from(spec.channels))
        .map(|frame| frame.iter().map(|v| v / f32::from(spec.channels)).sum())
        .collect();
    Ok(Sample {
        data,
        rate: spec.sample_rate,
    })
}

pub const WAVEFORM_BINS: usize = 96;
pub type Waveform = [[f32; 2]; WAVEFORM_BINS];

/// Lossless float32 WAV snapshot for retained renders and encoded fixtures.
pub fn encode_wav(sample: &Sample) -> std::io::Result<Vec<u8>> {
    use std::io::{Cursor, Error, ErrorKind};
    if !(8_000..=192_000).contains(&sample.rate)
        || sample.data.is_empty()
        || sample.data.iter().any(|value| !value.is_finite())
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "invalid retained sample",
        ));
    }
    let mut bytes = Cursor::new(Vec::new());
    let error = |e: hound::Error| Error::other(e.to_string());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
        hound::WavSpec {
            channels: 1,
            sample_rate: sample.rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .map_err(error)?;
    for value in sample.data.iter() {
        writer.write_sample(*value).map_err(error)?;
    }
    writer.finalize().map_err(error)?;
    Ok(bytes.into_inner())
}

/// Peak-preserving normalized preview, computed off the audio callback.
pub fn waveform(sample: &Sample) -> Waveform {
    let mut bins = [[0.0_f32; 2]; WAVEFORM_BINS];
    for (i, value) in sample.data.iter().enumerate() {
        let bin = i * WAVEFORM_BINS / sample.data.len();
        bins[bin][0] = bins[bin][0].min(*value);
        bins[bin][1] = bins[bin][1].max(*value);
    }
    let peak = bins.iter().flatten().fold(0.0_f32, |a, b| a.max(b.abs()));
    if peak > 0.0 {
        for value in bins.iter_mut().flatten() {
            *value /= peak;
        }
    }
    bins
}

pub struct Kit {
    pub samples: Box<[Sample]>,
    pub media: [Option<EncodedSample>; 5],
    pub errors: [Option<String>; 5],
    pub banner: String,
}

pub struct EncodedSample {
    pub imported: crate::import::Imported,
    pub bytes: Arc<[u8]>,
}

impl Kit {
    /// Capture, identify and decode one snapshot per file, before audio starts.
    pub fn from_encoded(files: [Result<Vec<u8>, String>; 5]) -> Self {
        let mut errors = std::array::from_fn(|_| None);
        let mut media = std::array::from_fn(|_| None);
        let samples = files
            .into_iter()
            .enumerate()
            .map(|(i, bytes)| {
                let decoded = bytes.and_then(|bytes| {
                    decode_wav(bytes.as_slice(), Path::new(FILES[i]))
                        .map(|sample| (bytes, sample))
                        .map_err(|e| e.to_string())
                });
                match decoded {
                    Ok((bytes, sample)) => {
                        media[i] = Some(EncodedSample {
                            imported: crate::import::Imported {
                                asset: crate::project::Asset {
                                    reference: crate::project::AssetRef::of(&bytes),
                                    kind: crate::project::MediaKind::Source,
                                },
                                waveform: waveform(&sample),
                                sample: sample.clone(),
                                label: FILES[i].into(),
                            },
                            bytes: bytes.into(),
                        });
                        sample
                    }
                    Err(error) => {
                        errors[i] = Some(error);
                        Sample {
                            data: Arc::from([]),
                            rate: 44_100,
                        }
                    }
                }
            })
            .collect();
        let banner = errors
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        Self {
            samples,
            media,
            errors,
            banner,
        }
    }

    pub fn load(folder: &Path) -> Self {
        let folder = match crate::paths::Resolver::current()
            .and_then(|paths| paths.resolve(crate::paths::Resource::Samples, Some(folder)))
        {
            Ok(folder) => folder,
            Err(error) => {
                let message = if error.kind == crate::paths::PathErrorKind::Missing {
                    format!(
                        "Sample folder not found: {}. Pass --samples DIR.",
                        error.path.as_deref().unwrap_or(folder).display()
                    )
                } else {
                    error.to_string()
                };
                let mut kit = Self::from_encoded(std::array::from_fn(|_| Err(message.clone())));
                kit.banner = message;
                return kit;
            }
        };
        Self::from_encoded(std::array::from_fn(|i| {
            let path = folder.join(FILES[i]);
            std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
        }))
    }
}

#[cfg(test)]
#[path = "kit_tests.rs"]
mod tests;
