#![allow(clippy::unwrap_used)]

use brownnote_native::engine::{Sample, Slots};
use brownnote_native::output::{
    AudioError, DeviceInfo, Output, OutputSelection, SelectionSource, check_audio,
};
use cpal::{
    BufferSize, DeviceId, SampleFormat, SupportedBufferSize, SupportedStreamConfig,
    SupportedStreamConfigRange,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

fn device(id: &str) -> DeviceInfo {
    DeviceInfo {
        id: DeviceId::new(cpal::default_host().id(), id),
        name: "Duplicate output name".into(),
        default_config: SupportedStreamConfig::new(
            2,
            44_100,
            SupportedBufferSize::Range { min: 64, max: 1024 },
            SampleFormat::F32,
        ),
        supported_configs: vec![SupportedStreamConfigRange::new(
            2,
            44_100,
            48_000,
            SupportedBufferSize::Range { min: 64, max: 1024 },
            SampleFormat::F32,
        )],
    }
}

#[test]
fn duplicate_names_do_not_alias_and_stale_ids_do_not_fall_back() {
    let devices = [device("first"), device("second")];
    let selection = OutputSelection {
        device_id: Some(devices[1].id.clone()),
        ..Default::default()
    };
    assert_eq!(
        selection
            .resolve(&devices, Some(&devices[0].id))
            .unwrap()
            .id,
        devices[1].id
    );
    assert!(matches!(
        selection.resolve(&devices[..1], Some(&devices[0].id)),
        Err(AudioError::SelectedDeviceUnavailable)
    ));
    assert_eq!(
        selection
            .resolve(&devices, Some(&devices[0].id))
            .unwrap()
            .id,
        devices[1].id,
        "the remembered ID reconnects even though a namesake stayed present"
    );
    assert!(matches!(
        selection.resolve(&[], None),
        Err(AudioError::SelectedDeviceUnavailable)
    ));

    let fallback = OutputSelection::default();
    assert_eq!(
        fallback.resolve(&devices, Some(&devices[0].id)).unwrap().id,
        devices[0].id
    );
    assert!(matches!(
        fallback.resolve(&devices, None),
        Err(AudioError::NoDevice)
    ));
    assert!(matches!(
        fallback.resolve(&[], Some(&devices[0].id)),
        Err(AudioError::NoDevice)
    ));
}

#[test]
fn defaults_and_both_rates_resolve_without_mutating_the_preference() {
    let device = device("rates");
    let default = device.configuration(&OutputSelection::default()).unwrap();
    assert_eq!(default.stream, device.default_config.config());
    assert_eq!(default.format, SampleFormat::F32);
    for rate in [44_100, 48_000] {
        for buffer in [
            BufferSize::Default,
            BufferSize::Fixed(64),
            BufferSize::Fixed(1024),
        ] {
            let selection = OutputSelection {
                sample_rate: Some(rate),
                buffer_size: buffer,
                ..Default::default()
            };
            let config = device.configuration(&selection).unwrap();
            assert_eq!(config.stream.sample_rate, rate);
            assert_eq!(config.stream.channels, 2);
            assert_eq!(config.stream.buffer_size, buffer);
            assert_eq!(selection.sample_rate, Some(rate));
        }
    }
}

#[test]
fn invalid_explicit_settings_and_invalid_defaults_are_rejected() {
    let mut device = device("invalid");
    for selection in [
        OutputSelection {
            sample_rate: Some(0),
            ..Default::default()
        },
        OutputSelection {
            sample_rate: Some(96_000),
            ..Default::default()
        },
        OutputSelection {
            channels: Some(0),
            ..Default::default()
        },
        OutputSelection {
            channels: Some(1),
            ..Default::default()
        },
        OutputSelection {
            buffer_size: BufferSize::Fixed(0),
            ..Default::default()
        },
        OutputSelection {
            buffer_size: BufferSize::Fixed(63),
            ..Default::default()
        },
        OutputSelection {
            buffer_size: BufferSize::Fixed(1025),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            device.configuration(&selection),
            Err(AudioError::UnsupportedConfiguration)
        ));
    }
    device.default_config =
        SupportedStreamConfig::new(0, 0, SupportedBufferSize::Unknown, SampleFormat::F32);
    assert!(matches!(
        device.configuration(&OutputSelection::default()),
        Err(AudioError::UnsupportedConfiguration)
    ));
}

#[test]
fn configurations_cannot_mix_constraints_from_different_ranges() {
    let mut device = device("tuples");
    device.supported_configs = vec![
        SupportedStreamConfigRange::new(
            2,
            44_100,
            44_100,
            SupportedBufferSize::Range { min: 64, max: 128 },
            SampleFormat::F32,
        ),
        SupportedStreamConfigRange::new(
            1,
            48_000,
            48_000,
            SupportedBufferSize::Range { min: 256, max: 512 },
            SampleFormat::F32,
        ),
        SupportedStreamConfigRange::new(
            2,
            48_000,
            48_000,
            SupportedBufferSize::Range { min: 256, max: 512 },
            SampleFormat::I16,
        ),
    ];
    assert!(matches!(
        device.configuration(&OutputSelection {
            sample_rate: Some(48_000),
            ..Default::default()
        }),
        Err(AudioError::UnsupportedConfiguration)
    ));
    assert!(matches!(
        device.configuration(&OutputSelection {
            buffer_size: BufferSize::Fixed(256),
            ..Default::default()
        }),
        Err(AudioError::UnsupportedConfiguration)
    ));
    let mono = device
        .configuration(&OutputSelection {
            sample_rate: Some(48_000),
            channels: Some(1),
            buffer_size: BufferSize::Fixed(256),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(mono.stream.channels, 1);
    assert_eq!(mono.stream.sample_rate, 48_000);
}

#[test]
fn unknown_buffer_bounds_allow_only_default_and_unsupported_formats_stay_errors() {
    let mut device = device("unknown");
    device.supported_configs = vec![SupportedStreamConfigRange::new(
        2,
        44_100,
        48_000,
        SupportedBufferSize::Unknown,
        SampleFormat::F32,
    )];
    assert!(device.configuration(&OutputSelection::default()).is_ok());
    assert!(matches!(
        device.configuration(&OutputSelection {
            buffer_size: BufferSize::Fixed(256),
            ..Default::default()
        }),
        Err(AudioError::UnsupportedConfiguration)
    ));
    device.default_config =
        SupportedStreamConfig::new(2, 44_100, SupportedBufferSize::Unknown, SampleFormat::F64);
    assert!(matches!(
        device.configuration(&OutputSelection::default()),
        Err(AudioError::UnsupportedFormat(SampleFormat::F64))
    ));
}

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

/// An owned preference root, so the probe never reads the maintainer's file.
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "brownnote-saved-probe-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn write(&self, content: &str) {
        fs::write(self.0.join("audio-output.json"), content).unwrap();
    }

    fn clear(&self) {
        let _ = fs::remove_file(self.0.join("audio-output.json"));
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn saved_probe(root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_brownnote-native"))
        .args(["--check-audio", "--selection", "saved"])
        .env("BROWNNOTE_PREFERENCES", root)
        .output()
        .unwrap()
}

#[test]
fn saved_probe_exits_non_zero_with_its_reason_and_prints_no_measurement() {
    let root = Root::new();
    for (content, reason) in [
        (None, "No saved output selection"),
        (Some("{broken"), "Corrupt machine output preferences"),
        (
            Some(
                r#"{"version":2,"device_id":null,"sample_rate":null,"channels":null,"buffer_frames":null}"#,
            ),
            "Unsupported machine output preference version",
        ),
        (
            Some(
                r#"{"version":1,"device_id":"invalid","sample_rate":null,"channels":null,"buffer_frames":null}"#,
            ),
            "Invalid saved output device ID",
        ),
        (
            Some(
                r#"{"version":1,"device_id":null,"sample_rate":0,"channels":null,"buffer_frames":null}"#,
            ),
            "Invalid saved output configuration",
        ),
    ] {
        match content {
            Some(content) => root.write(content),
            None => root.clear(),
        }
        let output = saved_probe(&root.0);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(3), "{reason}: {stderr}");
        assert!(stderr.contains(reason), "{reason}: {stderr}");
        assert!(
            output.stdout.is_empty(),
            "a failed probe printed a measurement block: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn probe_labels_its_source_and_refuses_a_stale_saved_id_without_a_stream() {
    assert_eq!(SelectionSource::Saved.to_string(), "saved");
    assert_eq!(SelectionSource::SystemDefault.to_string(), "system-default");
    let selection = OutputSelection {
        device_id: Some(DeviceId::new(
            cpal::default_host().id(),
            "brownnote-saved-probe-missing",
        )),
        sample_rate: Some(48_000),
        buffer_size: BufferSize::Fixed(256),
        ..Default::default()
    };
    let samples = Box::from([Sample {
        data: Arc::from([0.25; 8]),
        rate: 44_100,
    }]);
    let error = check_audio(samples, &selection, SelectionSource::Saved).unwrap_err();
    assert!(matches!(error, AudioError::SelectedDeviceUnavailable));
    assert!(error.to_string().contains("unavailable"));
}

#[test]
fn a_saved_rate_or_buffer_is_validated_as_whole_tuple_before_opening() {
    let device = device("saved-tuple");
    let selection = OutputSelection {
        device_id: Some(device.id.clone()),
        sample_rate: Some(96_000),
        buffer_size: BufferSize::Fixed(4_096),
        ..Default::default()
    };
    let resolved = selection
        .resolve(std::slice::from_ref(&device), None)
        .unwrap();
    assert!(matches!(
        resolved.configuration(&selection),
        Err(AudioError::UnsupportedConfiguration)
    ));
    let in_range = OutputSelection {
        sample_rate: Some(48_000),
        buffer_size: BufferSize::Fixed(256),
        ..selection
    };
    let resolved = in_range
        .resolve(std::slice::from_ref(&device), None)
        .unwrap();
    assert_eq!(
        resolved
            .configuration(&in_range)
            .unwrap()
            .stream
            .buffer_size,
        BufferSize::Fixed(256)
    );
}

#[test]
fn silent_output_keeps_the_exact_machine_preference_and_never_invents_timestamps() {
    let selection = OutputSelection {
        device_id: Some(device("remember-me").id),
        sample_rate: Some(48_000),
        buffer_size: BufferSize::Fixed(128),
        ..Default::default()
    };
    let mut output = Output::silent_selected(Slots::empty(), selection.clone(), "lost".into());
    output.advance(0.1);
    assert_eq!(output.selection(), &selection);
    assert!(output.failed);
    assert!(output.opened().is_none());
    assert!(output.timestamps().is_none());
    assert!(output.status.contains("Silent: lost"));
    assert!(output.status.contains("voices stopped"));
}
