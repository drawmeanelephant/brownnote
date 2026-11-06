use crate::engine::Sample;
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const DEFAULT_SAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../recordings");
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

pub fn load_wav(path: &Path) -> Result<Sample, KitError> {
    let error = |reason: String| KitError {
        path: path.to_path_buf(),
        reason,
    };
    let mut reader = hound::WavReader::open(path).map_err(|e| error(e.to_string()))?;
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
    if frames == 0 || frames > spec.sample_rate * 30 {
        return Err(error("length must be 1 frame..30 seconds".into()));
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

pub struct Kit {
    pub samples: Box<[Sample]>,
    pub errors: [Option<String>; 5],
    pub banner: String,
}

impl Kit {
    pub fn load(folder: &Path) -> Self {
        let mut errors = std::array::from_fn(|_| None);
        let samples = FILES
            .iter()
            .enumerate()
            .map(|(i, file)| {
                load_wav(&folder.join(file)).unwrap_or_else(|e| {
                    errors[i] = Some(e.to_string());
                    Sample {
                        data: Arc::from([]),
                        rate: 44_100,
                    }
                })
            })
            .collect();
        let banner = if !folder.is_dir() {
            format!(
                "Sample folder not found: {}. Pass --samples DIR.",
                folder.display()
            )
        } else {
            errors
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ")
        };
        Self {
            samples,
            errors,
            banner,
        }
    }
}

#[cfg(test)]
#[path = "kit_tests.rs"]
mod tests;
