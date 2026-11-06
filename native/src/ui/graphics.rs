use super::{caption, ink};
use brownnote_native::engine::Sample;
use rune::{Color, El, Shape, canvas, col, el, row};

pub(super) const BINS: usize = 96;
pub(super) type Envelope = [[f32; 2]; BINS];

pub(super) fn envelope(sample: &Sample) -> Envelope {
    let mut bins = [[0.0_f32; 2]; BINS];
    for (i, value) in sample.data.iter().enumerate() {
        let bin = i * BINS / sample.data.len();
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

pub(super) fn waveform(data: &Envelope, color: Color, detailed: bool) -> El {
    let (width, height) = (576.0, 60.0);
    let mut shape = Shape::new().view_box(width, height);
    if detailed {
        for x in (0..=12).map(|i| i as f32 * 48.0) {
            shape = shape.stroke(vec![[x, 0.0], [x, height]], 0.5, ink("#20322e"));
        }
        for y in [10.0, 30.0, 50.0] {
            shape = shape.stroke(vec![[0.0, y], [width, y]], 0.5, ink("#20322e"));
        }
    }
    shape = shape.stroke(
        vec![[0.0, 30.0], [width, 30.0]],
        0.7,
        color.mix(ink("#101918"), 0.65),
    );
    for (i, [low, high]) in data.iter().enumerate() {
        let x = i as f32 * 6.0 + 3.0;
        shape = shape.stroke(
            vec![[x, 30.0 - high * 27.0], [x, 30.0 - low * 27.0]],
            if detailed { 2.0 } else { 1.4 },
            color,
        );
    }
    canvas(shape).w_full().grow(1.0).min_h(4.0).a11y_hidden()
}

pub(super) fn logo() -> El {
    let mut shape = Shape::new().view_box(36.0, 36.0);
    for (i, h) in [5.0, 11.0, 18.0, 9.0, 14.0].iter().enumerate() {
        let x = 7.0 + i as f32 * 5.5;
        shape = shape.stroke(
            vec![[x, 18.0 - h / 2.0], [x, 18.0 + h / 2.0]],
            2.4,
            ink("#c8f77b"),
        );
    }
    canvas(shape)
        .size(38.0, 38.0)
        .radius(11.0)
        .gradient(ink("#344131"), ink("#19231d"))
        .border_gradient(1.0, ink("#5b7046"), ink("#2a3929"))
        .shadow(3.0, 10.0, ink("#070a09"))
        .a11y_hidden()
}

pub(super) fn led(on: bool, color: Color) -> El {
    el().size(5.0, 5.0)
        .radius(3.0)
        .fill(if on { color } else { ink("#35413d") })
        .shadow(
            0.0,
            if on { 7.0 } else { 0.0 },
            color.mix(Color::TRANSPARENT, 0.65),
        )
        .a11y_hidden()
}

pub(super) fn meter(peak: f32) -> El {
    let db = if peak > 0.0 {
        20.0 * peak.log10()
    } else {
        -60.0
    };
    let level = ((db + 48.0) / 48.0).clamp(0.0, 1.0);
    let mut segments = row().gap(3.0).h(17.0);
    for i in 0..24 {
        let on = level > i as f32 / 24.0;
        let color = if i >= 22 {
            "#fa806a"
        } else if i >= 18 {
            "#e4cb84"
        } else {
            "#b9ed7a"
        };
        segments = segments.child(
            el().grow(1.0)
                .h_full()
                .radius(1.0)
                .fill(if on { ink(color) } else { ink("#26352d") })
                .a11y_hidden(),
        );
    }
    col()
        .key("output-meter")
        .role("meter")
        .label("Output peak")
        .range_value(f64::from(db.max(-60.0)), -60.0, 0.0)
        .gap(6.0)
        .child(
            row()
                .justify_between()
                .child(caption("OUTPUT / PEAK"))
                .child(
                    caption(if peak == 0.0 {
                        "−∞ dB".into()
                    } else {
                        format!("{db:.1} dB")
                    })
                    .key("meter")
                    .a11y_value(format!("{peak:.3}"))
                    .color(ink("#c8f77b")),
                ),
        )
        .child(segments)
        .child(
            row()
                .justify_between()
                .child(caption("−48"))
                .child(caption("−24"))
                .child(caption("0 dB")),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn envelopes_preserve_transients_and_silence_without_inventing_signal() {
        for data in [vec![], vec![0.0; 1000]] {
            assert_eq!(
                envelope(&Sample {
                    data: data.into(),
                    rate: 44_100
                }),
                [[0.0; 2]; BINS]
            );
        }
        let mut data = vec![0.0; 960];
        data[9] = -0.5;
        data[955] = 1.0;
        let bins = envelope(&Sample {
            data: Arc::from(data),
            rate: 44_100,
        });
        assert_eq!(bins[0], [-0.5, 0.0]);
        assert_eq!(bins[95], [0.0, 1.0]);
        assert!(bins[1..95].iter().all(|b| *b == [0.0; 2]));
        let tiny = envelope(&Sample {
            data: Arc::from([f32::MAX, -f32::MAX]),
            rate: 8_000,
        });
        assert_eq!(tiny[0], [0.0, 1.0]);
        assert_eq!(tiny[48], [-1.0, 0.0]);
        assert!(tiny.iter().flatten().all(|v| v.is_finite()));
    }
}
