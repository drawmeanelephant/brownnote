use super::*;
use std::f32::consts::FRAC_1_SQRT_2 as C;

fn sample(data: Vec<f32>, rate: u32) -> Sample {
    Sample {
        data: data.into(),
        rate,
    }
}

fn rig_with_returns(
    samples: Vec<Sample>,
    rate: u32,
) -> (Mixer, Arc<Meters>, rtrb::Consumer<Sample>) {
    let meters = Arc::new(Meters::default());
    let (retired, returns) = rtrb::RingBuffer::new(SLOTS);
    let mut mixer = Mixer::new(Slots::from_samples(samples), rate, meters.clone(), retired);
    mixer.command(Command::SetMaster(1.0));
    mixer.render(&mut [0.0; 2], 2);
    (mixer, meters, returns)
}

fn rig(samples: Vec<Sample>, rate: u32) -> (Mixer, Arc<Meters>) {
    let (mixer, meters, returns) = rig_with_returns(samples, rate);
    drop(returns);
    (mixer, meters)
}

fn trigger(pad: usize, slot: usize) -> Command {
    Command::Trigger {
        pad,
        slot,
        group: slot,
        velocity: 1.0,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    }
}

fn trigger_in(pad: usize, slot: usize, group: usize) -> Command {
    Command::Trigger {
        pad,
        slot,
        group,
        velocity: 1.0,
        gain: 1.0,
        pan: 0.0,
        semitones: 0.0,
    }
}

fn hit(m: &mut Mixer, gain: f32, pan: f32, semitones: f32) {
    m.command(Command::Trigger {
        pad: 0,
        slot: 0,
        group: 0,
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
            slot: 0,
            group: 0,
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
fn choke_is_by_group_across_slots_and_never_across_groups() {
    for rate in [44_100, 48_000] {
        for choke in [true, false] {
            let (mut m, meters) = rig(vec![sample(vec![0.1; 10_000], rate); 2], rate);
            m.command(Command::SetChoke(choke));
            m.command(trigger_in(0, 0, 7));
            render(&mut m, 64, 2);
            m.command(trigger_in(1, 1, 7));
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
        let (mut m, meters) = rig(vec![sample(vec![0.1; 10_000], rate); 2], rate);
        m.command(trigger_in(0, 0, 3));
        render(&mut m, 64, 2);
        m.command(trigger_in(1, 1, 4));
        render(&mut m, 1, 2);
        assert_eq!(&meters.counts()[..2], &[1, 1]);
        let fade_frames = (f64::from(rate) * 0.005).ceil() as usize;
        render(&mut m, fade_frames - 1, 2);
        assert_eq!(&meters.counts()[..2], &[1, 1], "different groups choke");
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

#[test]
fn a_voice_plays_to_its_end_after_unload_and_the_sample_returns_once() {
    for rate in [44_100, 48_000] {
        let source = sample(vec![0.25; 1000], rate);
        let data = source.data.clone();
        let (mut m, _meters, mut returns) = rig_with_returns(vec![source], rate);
        let (mut reference, _) = rig(vec![sample(vec![0.25; 1000], rate)], rate);
        reference.command(trigger(0, 0));
        let expected = render(&mut reference, 1000, 2);

        m.command(trigger(0, 0));
        m.command(Command::Unload { slot: 0 });
        let mut got = render(&mut m, 900, 2);
        assert!(returns.pop().is_err(), "returned before the voice ended");
        got.extend(render(&mut m, 100, 2));
        assert_eq!(got, expected);
        let returned = returns.pop().unwrap();
        assert!(Arc::ptr_eq(&returned.data, &data));
        assert!(returns.pop().is_err(), "sample returned twice");
    }
}

#[test]
fn a_trigger_on_an_empty_or_retiring_slot_plays_nothing() {
    let (mut m, meters, _returns) = rig_with_returns(vec![sample(vec![0.5; 1000], 44_100)], 44_100);
    m.command(trigger(0, 5));
    render(&mut m, 1, 2);
    assert_eq!(meters.counts(), [0; 16]);

    m.command(trigger(0, 0));
    m.command(Command::Unload { slot: 0 });
    m.command(trigger(1, 0));
    render(&mut m, 1, 2);
    assert_eq!(&meters.counts()[..2], &[1, 0]);
}

#[test]
fn a_load_and_trigger_drained_in_one_block_play_the_new_sample() {
    for rate in [44_100, 48_000] {
        let (mut commands, mut consumer) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let (retired, _returns) = rtrb::RingBuffer::new(SLOTS);
        let mut m = Mixer::new(Slots::empty(), rate, Arc::new(Meters::default()), retired);
        commands.push(Command::SetMaster(1.0)).unwrap();
        m.drain(&mut consumer);
        m.render(&mut [0.0; 2], 2);

        commands
            .push(Command::Load {
                slot: 9,
                sample: sample(vec![0.3; 32], rate),
            })
            .unwrap();
        commands.push(trigger(0, 9)).unwrap();
        m.drain(&mut consumer);
        close(render(&mut m, 1, 2)[0], 0.3 * C);
    }
}

#[test]
fn a_load_into_an_occupied_slot_returns_the_incoming_sample_and_keeps_the_old() {
    let old = sample(vec![0.2; 64], 44_100);
    let incoming = sample(vec![0.9; 64], 44_100);
    let incoming_data = incoming.data.clone();
    let (mut m, _meters, mut returns) = rig_with_returns(vec![old], 44_100);
    m.command(Command::Load {
        slot: 0,
        sample: incoming,
    });
    let returned = returns.pop().unwrap();
    assert!(Arc::ptr_eq(&returned.data, &incoming_data));
    m.command(trigger(0, 0));
    close(render(&mut m, 1, 2)[0], 0.2 * C);
}

#[test]
fn a_full_return_queue_delays_retirement_and_the_overflow_never_loses_a_sample() {
    let rate = 44_100;
    let samples: Vec<Sample> = (0..SLOTS)
        .map(|i| sample(vec![0.001 * (i as f32 + 1.0); 8], rate))
        .collect();
    let (mut m, meters, mut returns) = rig_with_returns(samples, rate);
    for slot in 0..SLOTS {
        m.command(Command::Unload { slot });
    }
    render(&mut m, 1, 2);
    assert_eq!(returns.slots(), SLOTS, "return queue not full");

    let a = sample(vec![0.4; 8], rate);
    let b = sample(vec![0.6; 8], rate);
    let (a_data, b_data) = (a.data.clone(), b.data.clone());
    m.command(Command::Load { slot: 0, sample: a });
    m.command(Command::Load { slot: 0, sample: b });
    m.command(trigger(0, 0));
    close(render(&mut m, 1, 2)[0], 0.4 * C);
    render(&mut m, 7, 2);

    m.command(Command::Unload { slot: 0 });
    render(&mut m, 1, 2);
    m.command(trigger(1, 0));
    render(&mut m, 1, 2);
    assert_eq!(&meters.counts()[..2], &[0, 0], "retiring slot played");

    returns.pop().unwrap();
    render(&mut m, 1, 2);
    let held = drain(&mut returns);
    assert!(
        held.iter()
            .filter(|s| Arc::ptr_eq(&s.data, &b_data))
            .count()
            == 1,
        "the overflow sample was not returned exactly once"
    );
    assert!(held.iter().all(|s| !Arc::ptr_eq(&s.data, &a_data)));
    render(&mut m, 1, 2);
    let retired = returns.pop().unwrap();
    assert!(Arc::ptr_eq(&retired.data, &a_data));
    assert!(returns.pop().is_err());
}

#[test]
fn an_old_voice_keeps_playing_after_a_shorter_sample_loads_and_the_old_slot_unloads() {
    for rate in [44_100, 48_000] {
        let long = sample(vec![0.1; rate as usize * 3], rate);
        let (mut m, meters, mut returns) = rig_with_returns(vec![long.clone()], rate);
        let (mut reference, _) = rig(vec![long], rate);
        m.command(trigger(0, 0));
        reference.command(trigger(0, 0));
        m.command(Command::Load {
            slot: 1,
            sample: sample(vec![0.9; 4], rate),
        });
        m.command(Command::Unload { slot: 0 });

        let mut got = Vec::new();
        let mut expected = Vec::new();
        let mut left = rate as usize * 3 + 100;
        while left > 0 {
            let frames = left.min(512);
            got.extend(render(&mut m, frames, 2));
            expected.extend(render(&mut reference, frames, 2));
            left -= frames;
        }
        assert_eq!(got, expected);
        assert_eq!(meters.counts()[0], 0);
        assert!(returns.pop().is_ok());
        assert!(returns.pop().is_err());
    }
}

fn drain(returns: &mut rtrb::Consumer<Sample>) -> Vec<Sample> {
    let mut samples = Vec::new();
    while let Ok(sample) = returns.pop() {
        samples.push(sample);
    }
    samples
}
