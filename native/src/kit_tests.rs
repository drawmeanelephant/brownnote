use super::*;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "brownnote-kit-{}-{}.wav",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        )))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

fn rejected(f: &Fixture, reason: &str) {
    let error = match load_wav(&f.0) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("accepted invalid fixture"),
    };
    assert!(error.contains(f.0.to_str().unwrap()), "{error}");
    assert!(error.contains(reason), "{error}");
}

#[test]
fn all_repo_samples_and_exact_pad_mapping() {
    let kit = Kit::load(Path::new(DEFAULT_SAMPLES));
    assert!(kit.banner.is_empty(), "{}", kit.banner);
    for sample in &kit.samples {
        assert_eq!(sample.rate, 44_100);
        assert_eq!(sample.data.len(), 79_380);
    }
    assert_eq!(
        PADS.map(|p| p.key).iter().collect::<String>(),
        "1234qwerasdfzxcv"
    );
    assert_eq!(
        PADS.map(|p| p.sample),
        [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 3, 2]
    );
    assert_eq!(
        PADS.map(|p| p.semitones),
        [
            0., -5., -12., 7., 0., 7., 12., -7., 0., -5., 0., 5., 0., -12., -7., 12.
        ]
    );
    assert!(!FILES.iter().any(|f| f.contains("demo")));
}

#[test]
fn errors_name_file_for_missing_corrupt_8bit_empty_rate_and_length() {
    let missing = Fixture::new();
    rejected(&missing, ":");
    let corrupt = Fixture::new();
    std::fs::write(&corrupt.0, b"not a WAV").unwrap();
    rejected(&corrupt, ":");
    for (bits, rate, frames, reason) in [
        (8, 44_100, 1, "16-bit"),
        (16, 44_100, 0, "length"),
        (16, 4_000, 1, "sample rate"),
        (16, 8_000, 240_001, "length"),
    ] {
        let f = Fixture::new();
        let mut writer = hound::WavWriter::create(
            &f.0,
            hound::WavSpec {
                bits_per_sample: bits,
                sample_rate: rate,
                ..spec()
            },
        )
        .unwrap();
        for _ in 0..frames {
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();
        rejected(&f, reason);
    }
}

#[test]
fn stereo_pcm_and_float_average_and_nonfinite_is_rejected() {
    for float in [false, true] {
        let f = Fixture::new();
        let mut writer = hound::WavWriter::create(
            &f.0,
            hound::WavSpec {
                channels: 2,
                bits_per_sample: if float { 32 } else { 16 },
                sample_format: if float {
                    hound::SampleFormat::Float
                } else {
                    hound::SampleFormat::Int
                },
                ..spec()
            },
        )
        .unwrap();
        if float {
            for v in [0.5_f32, -0.25, 1.0, -1.0] {
                writer.write_sample(v).unwrap();
            }
        } else {
            for v in [16384_i16, -8192, 32767, -32767] {
                writer.write_sample(v).unwrap();
            }
        }
        writer.finalize().unwrap();
        assert_eq!(&*load_wav(&f.0).unwrap().data, &[0.125, 0.0]);
    }
    let f = Fixture::new();
    let mut writer = hound::WavWriter::create(
        &f.0,
        hound::WavSpec {
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
            ..spec()
        },
    )
    .unwrap();
    writer.write_sample(f32::NAN).unwrap();
    writer.finalize().unwrap();
    rejected(&f, "non-finite");
}

#[test]
fn missing_folder_is_nonfatal_and_bad_file_only_disables_its_pads() {
    let kit = Kit::load(Path::new("/nonexistent/brownnote-kit"));
    assert!(kit.errors.iter().all(Option::is_some));
    assert!(kit.banner.starts_with("Sample folder not found:"));
    let directory = std::env::temp_dir().join(format!("brownnote-partial-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::copy(
        crate::paths::Resolver::current()
            .unwrap()
            .resolve(
                crate::paths::Resource::Samples,
                Some(Path::new(DEFAULT_SAMPLES)),
            )
            .unwrap()
            .join(FILES[0]),
        directory.join(FILES[0]),
    )
    .unwrap();
    let kit = Kit::load(&directory);
    assert!(kit.errors[0].is_none());
    assert!(kit.errors[1..].iter().all(Option::is_some));
    assert_eq!(kit.samples[0].data.len(), 79_380);
    std::fs::remove_dir_all(directory).unwrap();
}
