//! Input-to-submit timing for windowed runs with `--probe`: the arrival time of a key or pad
//! press (`Tx::input_time`, the OS event's timestamp on macOS) against the first drawn frame's
//! submit time, and the display refresh it aimed at when a display link paced it. Photons come
//! later than either; this measures what the app controls. It also reports the pacing of drawn
//! frames each second, without a trace's own cost on the main thread.

use rune::time::Instant;
use rune::{FrameEncoded, FrameOutcome, Tx};
use std::cell::{Cell, RefCell};

thread_local! {
    static PENDING: Cell<Option<Instant>> = const { Cell::new(None) };
    static SUBMIT: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
    static PRESENT: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
    static PACING: RefCell<Pacing> = const { RefCell::new(Pacing { first: None, last: None, gaps: Vec::new() }) };
}

struct Pacing {
    first: Option<Instant>,
    last: Option<Instant>,
    /// Gaps between this second's drawn frames' display-link targets, in ms.
    gaps: Vec<f64>,
}

/// Note an input whose feedback the next drawn frame shows. Inputs between frames keep the
/// earliest, which is what a player waits on.
pub fn input(tx: &Tx) {
    if let Some(at) = tx.input_time() {
        PENDING.with(|p| {
            if p.get().is_none() {
                p.set(Some(at));
            }
        });
    }
}

/// `App::on_frame_encoded` hook.
pub fn frame(f: &FrameEncoded) {
    if f.outcome() != FrameOutcome::Drawn {
        return;
    }
    if let Some(target) = f.presents_at() {
        pacing(target);
    }
    let Some(input) = PENDING.with(Cell::take) else {
        return;
    };
    let ms = |t: Instant| t.saturating_duration_since(input).as_secs_f64() * 1000.0;
    let submit = ms(f.at());
    let present = f.presents_at().map(ms);
    let n = SUBMIT.with(|v| {
        let mut v = v.borrow_mut();
        v.push(submit);
        v.len()
    });
    if let Some(present) = present {
        PRESENT.with(|v| v.borrow_mut().push(present));
    }
    let (s50, s95) = SUBMIT.with(|v| quantiles(&v.borrow()));
    let (p50, p95) = PRESENT.with(|v| quantiles(&v.borrow()));
    eprintln!(
        "latency #{n}: input→submit {submit:.2} ms, input→display-link target {} | submit p50/p95 {s50:.2}/{s95:.2} ms, target p50/p95 {p50:.2}/{p95:.2} ms",
        present.map_or("n/a (unpaced frame)".to_owned(), |p| format!("{p:.2} ms")),
    );
}

/// Once a second: how far apart the drawn frames' targets were. A gap of two refreshes or more
/// is a refresh the window had nothing new for, or missed.
fn pacing(target: Instant) {
    PACING.with(|p| {
        let mut p = p.borrow_mut();
        let first = *p.first.get_or_insert(target);
        if let Some(last) = p.last.replace(target) {
            let gap = target.saturating_duration_since(last).as_secs_f64() * 1000.0;
            p.gaps.push(gap);
        }
        let since = target.saturating_duration_since(first).as_secs_f64();
        if p.gaps.len() >= 2 && since >= 1.0 {
            let (p50, _) = quantiles(&p.gaps);
            let long = p.gaps.iter().filter(|g| **g > 1.5 * p50).count();
            eprintln!(
                "pacing: {} frames over {since:.2} s, target gap p50 {p50:.2} ms, {long} gaps of two refreshes or more",
                p.gaps.len()
            );
            p.gaps.clear();
            p.first = Some(target);
        }
    });
}

fn quantiles(values: &[f64]) -> (f64, f64) {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let at = |p: f64| brownnote_native::look::fx::probe::percentile(&v, p);
    (at(0.5), at(0.95))
}
