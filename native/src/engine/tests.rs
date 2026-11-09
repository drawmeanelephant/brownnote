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
) -> (Mixer, Arc<Meters>, rtrb::Consumer<Returned>) {
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
fn overload_peak_is_mastered_before_clamping_at_both_rates_and_channel_layouts() {
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 4] {
            for sign in [-1.0, 1.0] {
                let (mut m, meters) = rig(vec![sample(vec![sign * 0.75; 8], rate)], rate);
                m.command(Command::SetMaster(0.75));
                render(&mut m, 1, channels);
                m.command(Command::SetChoke(false));
                hit(&mut m, 1.5, -1.0, 0.0);
                hit(&mut m, 1.5, -1.0, 0.0);
                let out = render(&mut m, 8, channels);
                assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                assert_eq!(out[0], sign * if channels == 1 { 0.5 } else { 1.0 });
                close(f32::from_bits(meters.peak.load(Relaxed)), 1.6875);
                assert!(meters.clipped.load(Relaxed));
                if channels == 4 {
                    assert!(
                        out.as_chunks::<4>()
                            .0
                            .iter()
                            .all(|frame| frame[2..] == [0.0; 2])
                    );
                }
            }
        }
    }
}

#[test]
fn overload_in_either_channel_sets_the_flag() {
    for rate in [44_100, 48_000] {
        for pan in [-1.0, 1.0] {
            let (mut m, meters) = rig(vec![sample(vec![-1.25], rate)], rate);
            hit(&mut m, 1.0, pan, 0.0);
            let out = render(&mut m, 1, 2);
            assert_eq!(out[usize::from(pan > 0.0)], -1.0);
            assert_eq!(f32::from_bits(meters.peak.load(Relaxed)), 1.25);
            assert!(meters.clipped.load(Relaxed));
        }
    }
}

#[test]
fn clean_near_unity_full_scale_and_mastered_headroom_do_not_report_overload() {
    for rate in [44_100, 48_000] {
        for value in [-1.0, -0.9995, 0.999, 0.9995, 1.0] {
            let (mut m, meters) = rig(vec![sample(vec![value; 8], rate)], rate);
            hit(&mut m, 1.0, -1.0, 0.0);
            assert_eq!(render(&mut m, 1, 2)[0], value);
            assert_eq!(f32::from_bits(meters.peak.load(Relaxed)), value.abs());
            assert!(!meters.clipped.load(Relaxed), "clean peak {value}");
        }
        let (mut m, meters) = rig(vec![sample(vec![2.0; 8], rate)], rate);
        m.command(Command::SetMaster(0.25));
        render(&mut m, 1, 2);
        hit(&mut m, 1.0, -1.0, 0.0);
        assert_eq!(render(&mut m, 1, 2)[0], 0.5);
        assert_eq!(f32::from_bits(meters.peak.load(Relaxed)), 0.5);
        assert!(!meters.clipped.load(Relaxed), "measure after master gain");
    }
}

#[test]
fn overload_flag_survives_quiet_blocks_until_consumed_and_rearms() {
    for rate in [44_100, 48_000] {
        let (mut m, meters) = rig(vec![sample(vec![1.25], rate)], rate);
        hit(&mut m, 1.0, -1.0, 0.0);
        render(&mut m, 1, 2);
        render(&mut m, 16, 2);
        assert_eq!(f32::from_bits(meters.peak.swap(0, Relaxed)), 1.25);
        // Reading peak must not consume the independent overload event.
        render(&mut m, 16, 2);
        assert_eq!(f32::from_bits(meters.peak.load(Relaxed)), 0.0);
        assert!(meters.clipped.swap(false, Relaxed));
        assert!(!meters.clipped.swap(false, Relaxed));
        render(&mut m, 16, 2);
        assert!(!meters.clipped.load(Relaxed), "silence must not rearm");
        hit(&mut m, 1.0, -1.0, 0.0);
        render(&mut m, 1, 2);
        assert!(meters.clipped.swap(false, Relaxed));
    }
}

#[test]
fn overload_flag_detects_overrange_that_rounds_to_unity_in_the_peak() {
    for rate in [44_100, 48_000] {
        let (mut m, meters) = rig(
            vec![
                sample(vec![1.0], rate),
                sample(vec![f32::EPSILON / 4.0], rate),
            ],
            rate,
        );
        hit(&mut m, 1.0, -1.0, 0.0);
        m.command(Command::Trigger {
            pad: 1,
            slot: 1,
            group: 1,
            velocity: 1.0,
            gain: 1.0,
            pan: -1.0,
            semitones: 0.0,
        });
        assert_eq!(render(&mut m, 1, 2)[0], 1.0);
        assert_eq!(f32::from_bits(meters.peak.load(Relaxed)), 1.0);
        assert!(meters.clipped.load(Relaxed));
    }
}

#[test]
fn overload_meter_follows_the_per_frame_master_ramp() {
    for rate in [44_100, 48_000] {
        let (mut m, meters) = rig(vec![sample(vec![1.25; 8], rate)], rate);
        hit(&mut m, 1.0, -1.0, 0.0);
        m.command(Command::SetMaster(0.0));
        // The first mastered sample is 1.25 * 0.75, not an overload.
        let out = render(&mut m, 4, 2);
        close(f32::from_bits(meters.peak.swap(0, Relaxed)), 0.9375);
        assert!(!meters.clipped.load(Relaxed));
        assert_eq!(out[6], 0.0);
        m.command(Command::SetMaster(1.0));
        let out = render(&mut m, 4, 2);
        close(f32::from_bits(meters.peak.load(Relaxed)), 1.25);
        assert!(meters.clipped.load(Relaxed));
        close(out[0], 0.3125);
        assert_eq!(out[6], 1.0);
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
        let (mut mixer, meters) = rig(
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
        assert_eq!(f32::from_bits(meters.peak.load(Relaxed)), f32::MAX);
        assert!(meters.clipped.load(Relaxed));
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
        let returned = returned_sample(returns.pop().unwrap());
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
    let returned = returned_sample(returns.pop().unwrap());
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
    let retired = returned_sample(returns.pop().unwrap());
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

fn returned_sample(item: Returned) -> Sample {
    match item {
        Returned::Retired { sample, .. } | Returned::Rejected(sample) => sample,
        Returned::Instrument(_) => panic!("no instrument was mounted"),
    }
}

fn drain(returns: &mut rtrb::Consumer<Returned>) -> Vec<Sample> {
    let mut samples = Vec::new();
    while let Ok(item) = returns.pop() {
        match item {
            Returned::Retired { sample, .. } | Returned::Rejected(sample) => samples.push(sample),
            Returned::Instrument(_) => panic!("no instrument was mounted"),
        }
    }
    samples
}

fn steal_hit(pad: usize, slot: usize, group: usize, pan: f32) -> Command {
    Command::Trigger {
        pad,
        slot,
        group,
        velocity: 1.0,
        gain: 1.0,
        pan,
        semitones: 0.0,
    }
}

/// Keep 31 silent voices behind the audible oldest voice, isolating its steal.
fn fill_steal_pool(m: &mut Mixer, pan: f32) {
    m.command(Command::SetChoke(false));
    m.command(steal_hit(0, 0, 7, pan));
    for _ in 1..VOICES {
        m.command(steal_hit(1, 1, 8, pan));
    }
}

#[test]
fn steal_tail_starts_new_hit_immediately_and_old_fade_is_monotonic() {
    for rate in [44_100, 48_000] {
        // Independent of Mixer::fade_frames: the existing five-millisecond law.
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        for old in [-0.5_f32, 0.5] {
            for new in [-0.125_f32, 0.125] {
                for start in [1, 64, 511] {
                    for pan in [-1.0, 0.0, 1.0] {
                        for channels in [1, 2, 4] {
                            let (mut m, meters) = rig(
                                vec![
                                    sample(vec![old; 2048], rate),
                                    sample(vec![0.0; 2048], rate),
                                    sample(vec![new; 2048], rate),
                                ],
                                rate,
                            );
                            fill_steal_pool(&mut m, pan);
                            render(&mut m, start, channels);
                            m.command(steal_hit(15, 2, 9, pan));
                            let out = render(&mut m, n + 1, channels);
                            let (l, r) = match pan {
                                -1.0 => (1.0, 0.0),
                                1.0 => (0.0, 1.0),
                                _ => (C, C),
                            };
                            let mut previous_old = old.abs() + 1.0;
                            for (i, frame) in out.chunks_exact(channels).enumerate() {
                                let fading_old = old * (n - i) as f32 / n as f32;
                                let value = fading_old + new;
                                if channels == 1 {
                                    close(frame[0], value * (l + r) * 0.5);
                                } else {
                                    close(frame[0], value * l);
                                    close(frame[1], value * r);
                                    assert!(frame[2..].iter().all(|v| *v == 0.0));
                                }
                                let scale = if channels == 1 {
                                    (l + r) * 0.5
                                } else {
                                    l.max(r)
                                };
                                let audible = if channels > 1 && r > l {
                                    frame[1]
                                } else {
                                    frame[0]
                                };
                                let isolated_old = (audible / scale - new).abs();
                                assert!(isolated_old <= previous_old + 2.0 * f32::EPSILON);
                                previous_old = isolated_old;
                            }
                            // A tail is audible, but no longer occupies an active pad voice.
                            assert_eq!(meters.counts()[0], 0);
                            assert_eq!(meters.counts()[15], 1);
                            assert_eq!(meters.counts().iter().sum::<u32>(), VOICES as u32);
                            m.command(Command::Panic);
                            let silence = render(&mut m, n + 4, channels);
                            assert!(silence[n * channels..].iter().all(|v| *v == 0.0));
                            assert_eq!(meters.counts(), [0; 16]);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn steal_tail_worst_phase_first_frame_improves_on_immediate_steal_fixture() {
    for rate in [44_100, 48_000] {
        let mut immediate_worst = 0.0_f32;
        let mut tail_worst = 0.0_f32;
        // Every phase, including both peaks, and both signs of the new onset.
        for start in 1..=64 {
            for onset in [-0.125_f32, 0.125] {
                let source: Vec<_> = (0..1024)
                    .map(|i| (std::f64::consts::TAU * i as f64 / 64.0).sin() as f32 * 0.5)
                    .collect();
                let previous = source[start - 1];
                let continuation = source[start];
                let (mut m, _) = rig(
                    vec![
                        sample(source, rate),
                        sample(vec![0.0; 1024], rate),
                        sample(vec![onset; 1024], rate),
                    ],
                    rate,
                );
                fill_steal_pool(&mut m, -1.0);
                close(render(&mut m, start, 2)[(start - 1) * 2], previous);
                m.command(steal_hit(15, 2, 9, -1.0));
                let first = render(&mut m, 1, 2)[0];
                close(first, continuation + onset);
                // Original behavior: the stolen signal disappears on this frame.
                immediate_worst = immediate_worst.max((onset - previous).abs());
                tail_worst = tail_worst.max((first - previous).abs());
            }
        }
        // A half-scale sine plus an opposite-sign 1/8-scale onset gives 5/8.
        close(immediate_worst, 0.625);
        // |sin(a+h)-sin(a)| <= 2 sin(h/2). The tail adds no first-frame step.
        let bound = 0.125 + (std::f64::consts::PI / 64.0).sin() as f32;
        assert!(tail_worst <= bound + 2.0 * f32::EPSILON);
        assert!(tail_worst < immediate_worst);
        println!(
            "steal phase sweep {rate}: immediate={immediate_worst:.9}, tail={tail_worst:.9}, analytical bound={bound:.9}"
        );
    }
}

#[test]
fn steal_tail_rendered_recordings_obey_fade_and_discontinuity_bounds() {
    let samples = crate::paths::Resolver::current()
        .unwrap()
        .resolve(
            crate::paths::Resource::Samples,
            Some(std::path::Path::new(crate::kit::DEFAULT_SAMPLES)),
        )
        .unwrap();
    for rate in [44_100, 48_000] {
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        for file in crate::kit::FILES {
            let source = crate::kit::load_wav(&samples.join(file)).unwrap();
            let frames = (source.data.len() as f64 * f64::from(rate) / f64::from(source.rate))
                .ceil() as usize;
            let (mut solo, _) = rig(vec![source.clone()], rate);
            solo.command(steal_hit(0, 0, 7, -1.0));
            let reference = render(&mut solo, frames, 1);
            // Mono hard-left is half-amplitude. Reconstruct the unclipped left
            // reference, also keeping the test safely away from the final clamp.
            let reference: Vec<_> = reference.into_iter().map(|v| v * 2.0).collect();
            let end = frames - n - 1;
            let peak_cut = (1..end)
                .max_by(|a, b| reference[a - 1].abs().total_cmp(&reference[b - 1].abs()))
                .unwrap();
            let slope_cut = (1..end)
                .max_by(|a, b| {
                    (reference[*a] - reference[a - 1])
                        .abs()
                        .total_cmp(&(reference[*b] - reference[b - 1]).abs())
                })
                .unwrap();
            let immediate_worst = reference[peak_cut - 1].abs();
            let tail_worst = (reference[slope_cut] - reference[slope_cut - 1]).abs();
            // Full-fade windows must include the actual global worst cases,
            // not silently omit a louder or steeper final few milliseconds.
            assert!(reference.iter().all(|v| v.abs() <= immediate_worst));
            assert!(
                reference
                    .windows(2)
                    .all(|v| (v[1] - v[0]).abs() <= tail_worst)
            );
            // Rounding allowance comes from the two f32 multiplies/subtraction,
            // not a fitted audio tolerance. The first frame's envelope is unity.
            let epsilon = 8.0 * f32::EPSILON * immediate_worst;
            assert!(immediate_worst > 0.0, "{file} is not a nonzero fixture");
            assert!(tail_worst < immediate_worst, "{file}: natural slope >= cut");
            for cut in [1, peak_cut, slope_cut] {
                let (mut m, _) = rig(
                    vec![source.clone(), sample(vec![0.0; frames + n], rate)],
                    rate,
                );
                fill_steal_pool(&mut m, -1.0);
                render(&mut m, cut, 2);
                m.command(steal_hit(15, 1, 9, -1.0));
                let out = render(&mut m, n + 1, 2);
                for i in 0..=n {
                    let expected = reference[cut + i] * (n - i) as f32 / n as f32;
                    assert!(
                        (out[i * 2] - expected).abs() <= epsilon,
                        "{file}, {rate}, {cut}, {i}"
                    );
                }
                assert!((out[0] - reference[cut - 1]).abs() <= tail_worst + epsilon);
                assert!(render(&mut m, 4, 2).iter().all(|v| *v == 0.0));
            }
            println!(
                "steal recording {file} {rate}: worst immediate={immediate_worst:.9}, worst first tail frame={tail_worst:.9}, envelope step bound={:.9}, fade={n} frames",
                immediate_worst / n as f32
            );
        }
    }
}

#[test]
fn steal_tail_saturation_cuts_oldest_tail_and_repeated_steals_stay_bounded() {
    for rate in [44_100, 48_000] {
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        let mut sources: Vec<_> = (1..=VOICES)
            .map(|i| sample(vec![i as f32 / 4096.0; 4096], rate))
            .collect();
        sources.push(sample(vec![0.0; 4096], rate));
        sources.push(sample(vec![0.03125; 4096], rate));
        let (mut m, meters) = rig(sources, rate);
        m.command(Command::SetChoke(false));
        for slot in 0..VOICES {
            m.command(steal_hit(slot % 16, slot, slot, -1.0));
        }
        render(&mut m, 1, 2);
        for _ in 0..STEAL_TAILS {
            m.command(steal_hit(15, VOICES, VOICES, -1.0));
        }
        let before = render(&mut m, 17, 2);
        close(before[0], (1..=VOICES).sum::<usize>() as f32 / 4096.0);
        m.command(steal_hit(15, VOICES + 1, VOICES + 1, -1.0));
        let after = render(&mut m, 1, 2)[0];
        let active = (STEAL_TAILS + 2..=VOICES).sum::<usize>() as f32;
        let old_tails = (2..=STEAL_TAILS).sum::<usize>() as f32;
        close(
            after,
            0.03125
                + (active + old_tails * (n - 17) as f32 / n as f32 + (STEAL_TAILS + 1) as f32)
                    / 4096.0,
        );
        assert_eq!(meters.counts().iter().sum::<u32>(), VOICES as u32);
        for _ in 0..VOICES * 3 {
            m.command(steal_hit(15, VOICES, VOICES, -1.0));
            let out = render(&mut m, 1, 4);
            assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
            assert_eq!(meters.counts().iter().sum::<u32>(), VOICES as u32);
        }
        assert!(render(&mut m, n + 4, 2).iter().all(|v| *v == 0.0));
        m.command(Command::Panic);
        render(&mut m, n, 2);
        assert_eq!(meters.counts(), [0; 16]);
    }
}

#[test]
fn steal_tail_panic_and_group_choke_do_not_restart_or_double_step_fades() {
    for rate in [44_100, 48_000] {
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        for panic in [false, true] {
            let (mut m, meters) = rig(
                vec![
                    sample(vec![0.25; 4096], rate),
                    sample(vec![0.0; 4096], rate),
                    sample(vec![0.125; 4096], rate),
                ],
                rate,
            );
            fill_steal_pool(&mut m, -1.0);
            render(&mut m, 1, 2);
            // Start a choke/panic fade *before* stealing; moving the voice
            // must retain the remaining duration rather than restart it.
            m.command(Command::Panic);
            render(&mut m, 13, 2);
            m.command(steal_hit(15, 2, 7, -1.0));
            render(&mut m, 17, 2);
            for i in 30..=n {
                if panic {
                    m.command(Command::Panic);
                } else if i == 30 || i == 47 {
                    m.command(Command::SetChoke(true));
                    // Same group, different slot: affects active and tail once.
                    // This silent voice may itself steal, exercising overlap.
                    m.command(steal_hit(2, 1, 7, -1.0));
                }
                let old = 0.25 * (n - i) as f32 / n as f32;
                let new = 0.125 * (n - (i - 30)) as f32 / n as f32;
                close(render(&mut m, 1, 2)[0], old + new);
            }
            // Both operations leave the two original deadlines unchanged.
            render(&mut m, n + 1, 2);
            assert!(render(&mut m, 4, 2).iter().all(|v| *v == 0.0));
            m.command(Command::Panic);
            render(&mut m, n, 2);
            assert_eq!(meters.counts(), [0; 16]);
        }
    }
}

#[test]
fn steal_tail_holds_retiring_sample_rejects_load_and_returns_it_once() {
    for rate in [44_100, 48_000] {
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        let old = sample(vec![0.25; 4096], rate);
        let data = old.data.clone();
        let replacement = sample(vec![0.75; 4], rate);
        let replacement_data = replacement.data.clone();
        let (mut m, meters, mut returns) =
            rig_with_returns(vec![old, sample(vec![0.0; 4096], rate)], rate);
        fill_steal_pool(&mut m, -1.0);
        render(&mut m, 1, 2);
        m.command(steal_hit(15, 1, 9, -1.0));
        m.command(Command::Unload { slot: 0 });
        m.command(Command::Load {
            slot: 0,
            sample: replacement,
        });
        match returns.pop().unwrap() {
            Returned::Rejected(sample) => assert!(Arc::ptr_eq(&sample.data, &replacement_data)),
            _ => panic!("overwrote a tail's sample"),
        }
        m.command(steal_hit(3, 0, 7, -1.0));
        let out = render(&mut m, n - 1, 2);
        close(out[0], 0.25);
        close(out[(n - 2) * 2], 0.25 * 2.0 / n as f32);
        assert_eq!(meters.counts()[0], 0);
        assert_eq!(meters.counts()[3], 0);
        assert!(
            returns.pop().is_err(),
            "retired a sample still read by a tail"
        );
        close(render(&mut m, 1, 2)[0], 0.25 / n as f32);
        match returns.pop().unwrap() {
            Returned::Retired { slot, sample } => {
                assert_eq!(slot, 0);
                assert!(Arc::ptr_eq(&sample.data, &data));
            }
            _ => panic!("tail sample did not retire"),
        }
        assert!(render(&mut m, 2, 2).iter().all(|v| *v == 0.0));
        assert!(returns.pop().is_err(), "tail sample returned twice");
        m.command(Command::Load {
            slot: 0,
            sample: sample(vec![0.75; 4], rate),
        });
        m.command(steal_hit(3, 0, 7, -1.0));
        close(render(&mut m, 1, 2)[0], 0.75);
    }
}

#[test]
fn steal_tail_eviction_releases_its_last_sample_reference() {
    for rate in [44_100, 48_000] {
        let old = sample(vec![0.25; 4096], rate);
        let data = old.data.clone();
        let (mut m, _, mut returns) =
            rig_with_returns(vec![old, sample(vec![0.0; 4096], rate)], rate);
        fill_steal_pool(&mut m, -1.0);
        render(&mut m, 1, 2);
        m.command(steal_hit(15, 1, 9, -1.0));
        m.command(Command::Unload { slot: 0 });
        close(render(&mut m, 1, 2)[0], 0.25);
        assert!(returns.pop().is_err());
        for _ in 0..STEAL_TAILS {
            m.command(steal_hit(15, 1, 9, -1.0));
        }
        assert!(render(&mut m, 1, 2).iter().all(|v| *v == 0.0));
        match returns.pop().unwrap() {
            Returned::Retired { slot, sample } => {
                assert_eq!(slot, 0);
                assert!(Arc::ptr_eq(&sample.data, &data));
            }
            _ => panic!("evicted tail's sample was not retired"),
        }
        assert!(returns.pop().is_err());
    }
}

#[test]
fn steal_tail_group_choke_ignores_other_groups_and_sample_end_wins_over_fade() {
    for rate in [44_100, 48_000] {
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        for length in [4, 4096] {
            let (mut m, _) = rig(
                vec![
                    sample(vec![0.25; length], rate),
                    sample(vec![0.0; 4096], rate),
                    sample(vec![0.125; 4096], rate),
                ],
                rate,
            );
            fill_steal_pool(&mut m, -1.0);
            render(&mut m, 1, 2);
            m.command(steal_hit(15, 2, 9, -1.0));
            render(&mut m, 1, 2);
            m.command(Command::SetChoke(true));
            m.command(steal_hit(2, 1, 9, -1.0));
            let out = render(&mut m, n + 1, 2);
            for i in 0..=n {
                let old = if i + 2 < length && i + 1 < n {
                    0.25 * (n - i - 1) as f32 / n as f32
                } else {
                    0.0
                };
                // Only the new active voice's group is choked. The old tail
                // continues on its own earlier deadline, or its sample ends.
                let new = 0.125 * (n - i) as f32 / n as f32;
                close(out[i * 2], old + new);
            }
            assert!(render(&mut m, 2, 2).iter().all(|v| *v == 0.0));
        }
    }
}

#[test]
fn steal_tails_share_master_preclamp_peak_clip_flag_and_output_channels() {
    for rate in [44_100, 48_000] {
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        for channels in [1, 2, 4] {
            for pan in [-1.0, 1.0] {
                for sign in [-1.0, 1.0] {
                    let (mut m, meters) = rig(
                        vec![
                            sample(vec![sign * 0.75; 4096], rate),
                            sample(vec![0.0; 4096], rate),
                            sample(vec![sign * 0.5; 4096], rate),
                        ],
                        rate,
                    );
                    fill_steal_pool(&mut m, pan);
                    render(&mut m, 1, channels);
                    meters.peak.swap(0, Relaxed);
                    assert!(!meters.clipped.swap(false, Relaxed));
                    m.command(steal_hit(15, 2, 9, pan));
                    let first = render(&mut m, 1, channels);
                    close(f32::from_bits(meters.peak.swap(0, Relaxed)), 1.25);
                    assert!(meters.clipped.swap(false, Relaxed));
                    let channel = usize::from(pan > 0.0 && channels > 1);
                    close(first[channel], sign * if channels == 1 { 0.5 } else { 1.0 });
                    assert!(first.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                    assert!(first.get(2..).unwrap_or(&[]).iter().all(|v| *v == 0.0));

                    m.command(Command::SetMaster(0.0));
                    let ramp = render(&mut m, 4, channels);
                    for (i, frame) in ramp.chunks_exact(channels).enumerate() {
                        let master = (3 - i) as f32 / 4.0;
                        let signal = sign * (0.75 * (n - i - 1) as f32 / n as f32 + 0.5);
                        close(
                            frame[channel],
                            signal * master * if channels == 1 { 0.5 } else { 1.0 },
                        );
                    }
                    assert!(!meters.clipped.swap(false, Relaxed));
                    let expected_peak = (0.75 * (n - 1) as f32 / n as f32 + 0.5) * 0.75;
                    close(f32::from_bits(meters.peak.swap(0, Relaxed)), expected_peak);
                    assert!(render(&mut m, n + 4, channels).iter().all(|v| *v == 0.0));
                    assert_eq!(meters.peak.load(Relaxed), 0);
                    m.command(Command::Panic);
                    render(&mut m, n, channels);
                    assert_eq!(meters.counts(), [0; 16]);
                }
            }
        }
        let (mut m, meters) = rig(vec![sample(vec![f32::MAX; 4096], rate)], rate);
        m.command(Command::SetChoke(false));
        for _ in 0..VOICES + STEAL_TAILS + 1 {
            hit(&mut m, 1.5, 0.0, 0.0);
        }
        assert!(
            render(&mut m, 1, 4)
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 1.0)
        );
        assert_eq!(f32::from_bits(meters.peak.swap(0, Relaxed)), f32::MAX);
        m.command(Command::Panic);
        let out = render(&mut m, n + 2, 2);
        assert!(out[n * 2..].iter().all(|v| *v == 0.0));
        assert_eq!(meters.counts(), [0; 16]);
        assert!(meters.clipped.swap(false, Relaxed));
    }
}

fn steal_tail_format<T>()
where
    T: cpal::Sample + cpal::FromSample<f32>,
    f32: cpal::FromSample<T>,
{
    for rate in [44_100, 48_000] {
        let n = (f64::from(rate) / 200.0).ceil() as usize;
        for channels in [1, 2, 4] {
            let (mut m, _) = rig(
                vec![
                    sample(vec![0.3; 4096], rate),
                    sample(vec![0.0; 4096], rate),
                    sample(vec![0.2; 4096], rate),
                ],
                rate,
            );
            fill_steal_pool(&mut m, -1.0);
            render(&mut m, 1, channels);
            m.command(steal_hit(15, 2, 9, -1.0));
            let mut out = vec![T::from_sample(0.0); (n + 2) * channels];
            m.render_into(&mut out, channels, T::from_sample);
            for (i, frame) in out.chunks_exact(channels).enumerate() {
                let expected = (0.3 * n.saturating_sub(i) as f32 / n as f32 + 0.2)
                    * if channels == 1 { 0.5 } else { 1.0 };
                // One least-significant PCM16 unit bounds all supported formats.
                assert!((frame[0].to_sample::<f32>() - expected).abs() < 1.0 / 32768.0);
                assert!(frame[1..].iter().all(|v| v.to_sample::<f32>() == 0.0));
            }
        }
    }
}

#[test]
fn steal_tails_convert_all_four_device_formats() {
    steal_tail_format::<f32>();
    steal_tail_format::<i16>();
    steal_tail_format::<u16>();
    steal_tail_format::<i32>();
}
