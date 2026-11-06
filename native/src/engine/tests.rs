use super::*;
use std::f32::consts::FRAC_1_SQRT_2 as C;

fn sample(data: Vec<f32>, rate: u32) -> Sample {
    Sample {
        data: data.into(),
        rate,
    }
}

fn rig(samples: Vec<Sample>, rate: u32) -> (Mixer, Arc<Meters>) {
    let meters = Arc::new(Meters::default());
    let mut mixer = Mixer::new(samples.into_boxed_slice(), rate, meters.clone());
    mixer.command(Command::SetMaster(1.0));
    mixer.render(&mut [0.0; 2], 2);
    (mixer, meters)
}

fn trigger(pad: usize, sample: usize) -> Command {
    Command::Trigger {
        pad,
        sample,
        velocity: 1.0,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    }
}

fn hit(m: &mut Mixer, gain: f32, pan: f32, semitones: f32) {
    m.command(Command::Trigger {
        pad: 0,
        sample: 0,
        velocity: 1.0,
        gain,
        pan,
        semitones,
    });
}

fn render(m: &mut Mixer, frames: usize, channels: usize) -> Vec<f32> {
    let mut output = vec![0.0; frames * channels];
    m.render(&mut output, channels);
    output
}

fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-6, "{a} != {b}");
}

#[test]
fn unity_gain_velocity_and_constant_power_pan_at_both_rates() {
    for rate in [44_100, 48_000] {
        let src = vec![0.0, 0.1, -0.2, 0.5, -0.8];
        for gain in [0.5, 1.0] {
            for pan in [-1.0, 0.0, 1.0] {
                let (mut m, _) = rig(vec![sample(src.clone(), rate)], rate);
                hit(&mut m, gain, pan, 0.0);
                let out = render(&mut m, src.len(), 2);
                for (frame, value) in out.as_chunks::<2>().0.iter().zip(&src) {
                    let (l, r) = match pan {
                        -1.0 => (1.0, 0.0),
                        1.0 => (0.0, 1.0),
                        _ => (C, C),
                    };
                    close(frame[0], value * gain * l);
                    close(frame[1], value * gain * r);
                }
            }
        }
        let (mut m, _) = rig(vec![sample(vec![0.8; 32], rate)], rate);
        m.command(Command::Trigger {
            pad: 0,
            sample: 0,
            velocity: 0.4,
            gain: 0.5,
            pan: 0.0,
            semitones: 0.0,
        });
        close(render(&mut m, 1, 2)[0], 0.8 * 0.4 * 0.5 * C);
    }
}

#[test]
fn octave_pitch_values_and_durations_at_both_rates() {
    for rate in [44_100, 48_000] {
        let src: Vec<_> = (0..100).map(|i| i as f32 / 200.0 + 0.01).collect();
        for (pitch, length) in [(12.0, 50), (-12.0, 200)] {
            let (mut m, meters) = rig(vec![sample(src.clone(), rate)], rate);
            hit(&mut m, 1.0, 0.0, pitch);
            let out = render(&mut m, length + 3, 2);
            let actual = out.as_chunks::<2>().0.iter().filter(|f| f[0] > 0.0).count();
            assert!(actual.abs_diff(length) <= 2);
            assert_eq!(meters.counts()[0], 0);
            for (k, frame) in out.as_chunks::<2>().0.iter().take(length).enumerate() {
                let expected = if pitch > 0.0 {
                    src[k * 2]
                } else {
                    let i = k / 2;
                    if k % 2 == 0 {
                        src[i]
                    } else {
                        (src[i] + src[(i + 1).min(99)]) / 2.0
                    }
                };
                close(frame[0], expected * C);
                close(frame[1], expected * C);
            }
        }
    }
}

#[test]
fn sample_rate_conversion_uses_the_same_playback_step() {
    for rate in [44_100, 48_000] {
        let (mut m, meters) = rig(vec![sample(vec![0.2; 4410], 44_100)], rate);
        hit(&mut m, 1.0, 0.0, 0.0);
        let out = render(&mut m, 5000, 2);
        let frames = out
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|f| f[0] != 0.0)
            .count();
        let expected = (4410.0 * f64::from(rate) / 44_100.0) as usize;
        assert!(frames.abs_diff(expected) <= 2, "{rate}: {frames}");
        assert_eq!(meters.counts()[0], 0);
        let (mut ramp, _) = rig(vec![sample(vec![0.0, 0.1, 0.2, 0.3], 44_100)], rate);
        hit(&mut ramp, 1.0, 0.0, 0.0);
        let out = render(&mut ramp, 3, 2);
        for k in 0..3 {
            close(out[k * 2], 0.1 * k as f32 * 44_100.0 / rate as f32 * C);
        }
    }
}

#[test]
fn distinct_samples_overlap_as_the_sum_of_solo_renders() {
    for rate in [44_100, 48_000] {
        let samples = vec![sample(vec![0.1; 64], rate), sample(vec![0.3; 64], rate)];
        let (mut a, _) = rig(samples.clone(), rate);
        let (mut b, _) = rig(samples.clone(), rate);
        let (mut together, _) = rig(samples, rate);
        a.command(trigger(0, 0));
        b.command(trigger(4, 1));
        together.command(trigger(0, 0));
        together.command(trigger(4, 1));
        for ((a, b), both) in render(&mut a, 64, 2)
            .iter()
            .zip(render(&mut b, 64, 2))
            .zip(render(&mut together, 64, 2))
        {
            close(a + b, both);
        }
    }
}

#[test]
fn choke_matches_sample_not_pad_and_off_stacks() {
    for rate in [44_100, 48_000] {
        for choke in [true, false] {
            let (mut m, meters) = rig(vec![sample(vec![0.1; 10_000], rate)], rate);
            m.command(Command::SetChoke(choke));
            m.command(trigger(0, 0));
            render(&mut m, 64, 2);
            m.command(trigger(1, 0));
            render(&mut m, 1, 2);
            assert_eq!(&meters.counts()[..2], &[1, 1]);
            let fade_frames = (f64::from(rate) * 0.005).ceil() as usize;
            render(&mut m, fade_frames - 1, 2);
            assert_eq!(&meters.counts()[..2], if choke { &[0, 1] } else { &[1, 1] });
            close(
                render(&mut m, 1, 2)[0],
                0.1 * C * if choke { 1.0 } else { 2.0 },
            );
        }
    }
}

#[test]
fn panic_fades_to_exact_silence_and_next_trigger_works() {
    for rate in [44_100, 48_000] {
        let (mut m, meters) = rig(vec![sample(vec![0.4; 10_000], rate)], rate);
        hit(&mut m, 1.0, 0.0, 0.0);
        render(&mut m, 32, 2);
        m.command(Command::Panic);
        let fade = (f64::from(rate) * 0.005).ceil() as usize;
        let output = render(&mut m, fade + 40, 2);
        assert!(output[0] > 0.0);
        assert!(output.windows(2).all(|p| p[0] >= p[1]));
        assert!(output[fade * 2..].iter().all(|v| *v == 0.0));
        assert_eq!(meters.counts(), [0; 16]);
        hit(&mut m, 1.0, 0.0, 0.0);
        assert!(render(&mut m, 1, 2)[0] > 0.0);
    }
}

#[test]
fn voice_cap_steals_oldest_keeps_newest_and_clamps_output() {
    for rate in [44_100, 48_000] {
        let (mut m, meters) = rig(vec![sample(vec![1.0; 10_000], rate)], rate);
        m.command(Command::SetChoke(false));
        for i in 0..40 {
            m.command(trigger(
                if i < 8 {
                    0
                } else if i == 39 {
                    15
                } else {
                    1
                },
                0,
            ));
        }
        let out = render(&mut m, 64, 2);
        assert_eq!(meters.counts().iter().sum::<u32>(), 32);
        assert_eq!(meters.counts()[0], 0);
        assert_eq!(meters.counts()[15], 1);
        assert!(out.iter().all(|v| v.abs() <= 1.0));
        assert_eq!(out[0], 1.0);
    }
}

#[test]
fn master_ramps_one_block_and_zero_remains_silent() {
    for rate in [44_100, 48_000] {
        let (mut m, _) = rig(vec![sample(vec![0.5; 10_000], rate)], rate);
        hit(&mut m, 1.0, 0.0, 0.0);
        m.command(Command::SetMaster(0.0));
        let out = render(&mut m, 100, 2);
        close(out[0], 0.5 * C * 0.99);
        close(out[198], 0.0);
        assert!(render(&mut m, 100, 2).iter().all(|v| *v == 0.0));
    }
}

#[test]
fn mono_averages_stereo_and_surround_channels_are_zero() {
    for rate in [44_100, 48_000] {
        for channels in [1, 4] {
            let (mut m, _) = rig(vec![sample(vec![0.4; 8], rate)], rate);
            hit(&mut m, 1.0, -1.0, 0.0);
            let out = render(&mut m, 8, channels);
            for frame in out.chunks_exact(channels) {
                close(frame[0], if channels == 1 { 0.2 } else { 0.4 });
                if channels == 4 {
                    assert!(frame[1..].iter().all(|v| v.abs() < 1e-6));
                }
            }
        }
    }
}

#[test]
fn trigger_settings_are_snapshots_not_live_pad_settings() {
    let (mut m, _) = rig(vec![sample(vec![0.4; 64], 44_100)], 44_100);
    hit(&mut m, 0.5, -1.0, 0.0);
    let first = render(&mut m, 1, 2);
    m.command(Command::SetChoke(false));
    hit(&mut m, 1.0, 1.0, 0.0);
    let both = render(&mut m, 1, 2);
    close(first[0], both[0]);
    close(both[0], 0.2);
    close(both[1], 0.4);
}

#[test]
fn finite_float_extremes_cannot_overflow_interpolation_or_mix_to_nan() {
    for rate in [44_100, 48_000] {
        let (mut mixer, _) = rig(
            vec![sample(vec![f32::MAX, -f32::MAX, f32::MAX, -f32::MAX], rate)],
            rate,
        );
        mixer.command(Command::SetChoke(false));
        for _ in 0..32 {
            hit(&mut mixer, 1.5, 0.0, -12.0);
        }
        let out = render(&mut mixer, 10, 2);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert_eq!(out[0], 1.0);
        assert_eq!(out[2], 0.0);
    }
}
