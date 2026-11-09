//! Live recording transport: arm/record/stop, loop replay and capture.
//!
//! Capture runs on the UI thread from the same entry points that sound the
//! performance (keyboard, pointer and MIDI dispatch) and writes into the one
//! `State.takes` registry. Every recorded time is integer nanoseconds from its
//! pass start (D9); nothing is quantized at capture. A pass always starts on a
//! loop boundary and records one loop cycle, so overdubs line up with replay.
//! Replay schedules committed, effective notes through the existing bounded
//! scheduler (`Output::schedule`), never through ad-hoc triggers.
//!
//! One-shot pads ignore release (as live), so they are stored as zero-length
//! notes and replayed without a note-off. Held-instrument notes keep their
//! real FIFO durations (D14) but are not sounded: no held-instrument audio
//! backend is mounted in the app.
use super::{State, caption, ink};
use brownnote_native::{
    engine::Command,
    output::Output,
    performance::{
        Channel, EventId, Note, NoteKey, NoteOn, OriginalTime, Pitch, SourceId, StreamEpoch,
        TakeId, Target, Velocity,
        scheduler::{ScheduleOutcome, ScheduledAction, ScheduledEvent, Trigger},
    },
    project::PadId,
    takes::{CountIn, PassMode, PassOptions, Takes},
};
use rune::{El, TextStyle, el, row, text};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

/// Logical sources for one-shot pad capture. One-shots open and close at the
/// same instant, so they get their own identity space and can never end a held
/// note of a MIDI device through the FIFO matcher.
pub const KEYBOARD_SOURCE: SourceId = SourceId(u128::MAX);
pub const MIDI_PAD_SOURCE: SourceId = SourceId(u128::MAX - 1);
/// Replay scheduling horizon; far inside the scheduler's 2 s admission window.
const LOOKAHEAD_NANOS: u64 = 500_000_000;
const NANOS: u128 = 1_000_000_000;

#[derive(Clone, Copy, Debug)]
struct Pass {
    id: TakeId,
    /// App-clock nanoseconds of the loop boundary this pass starts on.
    start: u64,
    end: u64,
    started: bool,
    /// Past its cycle (or stopped): no new notes, waiting for held ends.
    closing: bool,
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    epoch: StreamEpoch,
    app: u64,
    frame: u64,
    rate: u32,
}

/// Cumulative, independent observations shown in the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub recorded: u64,
    pub held_recorded: u64,
    /// Played while armed/closing: sounded live, not added to a pass.
    pub outside_pass: u64,
    pub scheduled: u64,
    /// Replay events the scheduler refused (full, too far, stale stream).
    pub schedule_lost: u64,
}

#[derive(Default)]
pub struct Transport {
    pub options: PassOptions,
    /// App-clock nanoseconds of loop cycle zero while playing.
    playing: Option<u64>,
    pass: Option<Pass>,
    anchor: Option<Anchor>,
    scheduled: BTreeSet<(u64, EventId)>,
    /// MIDI backend clock to app clock, per connection source.
    midi_anchor: Option<(SourceId, u64, u64)>,
    /// Controller held-note IDs captured into the current pass.
    held: BTreeMap<EventId, u64>,
    pub counters: Counters,
    pub notice: Option<String>,
    pub error: Option<String>,
    /// Why the open pass is closing early (stop, panic), kept for its notice.
    reason: Option<String>,
    /// Committed held notes without a sounding backend (gauge, per poll).
    unsounded: usize,
    shown: Option<(u64, u64)>,
}

impl Transport {
    pub fn playing(&self) -> bool {
        self.playing.is_some()
    }
    pub fn active(&self) -> bool {
        self.playing.is_some() || self.pass.is_some()
    }
}

pub fn app_nanos(s: &State) -> u64 {
    (s.now.max(0.0) * 1e9).min(u64::MAX as f64) as u64
}

fn loop_nanos(takes: &Takes) -> u64 {
    let quarters = u128::from(takes.loop_bars().bars()) * 4;
    let nanos = quarters * 60 * NANOS * 1000 / u128::from(takes.tempo().milli_bpm());
    u64::try_from(nanos).unwrap_or(u64::MAX).max(1)
}

fn rate(output: &Output) -> u32 {
    output
        .opened()
        .map_or(44_100, |opened| opened.configuration.stream.sample_rate)
}

fn velocity_of(value: f32) -> Velocity {
    let scaled = (value.clamp(0.0, 1.0) * 127.0).round() as u8;
    Velocity::try_from(scaled.max(1)).unwrap_or_else(|_| unreachable!("1..=127"))
}

/// Backend MIDI time to the app clock. The controller's per-connection clock
/// only keeps exact *differences* between packets, so the first packet seen
/// from a source is anchored at the poll that received it.
pub fn midi_time(s: &mut State, source: SourceId, time: OriginalTime) -> u64 {
    let now = app_nanos(s);
    let t = &mut s.transport;
    match t.midi_anchor {
        Some((anchored, backend, app)) if anchored == source && time.nanos() >= backend => {
            app.saturating_add(time.nanos() - backend)
        }
        _ => {
            t.midi_anchor = Some((source, time.nanos(), now));
            now
        }
    }
}

/// The open pass and this event's take-relative time, starting the pass when
/// its boundary has been reached.
fn open_pass(t: &mut Transport, takes: &mut Takes, at: u64) -> Option<(TakeId, OriginalTime)> {
    let pass = t.pass.as_mut()?;
    if pass.closing || at < pass.start || at >= pass.end {
        t.counters.outside_pass += 1;
        return None;
    }
    if !pass.started {
        if let Err(error) = takes.start(pass.id) {
            t.error = Some(format!("Recording could not start: {error}"));
            return None;
        }
        pass.started = true;
    }
    Some((pass.id, OriginalTime::from_nanos(at - pass.start)))
}

/// A one-shot pad hit (keyboard, pointer or MIDI) at app time `at`.
pub fn capture_pad(s: &mut State, pad: PadId, velocity: Velocity, source: SourceId, at: u64) {
    let (t, takes) = (&mut s.transport, &mut s.takes);
    let Some((pass, time)) = open_pass(t, takes, at) else {
        return;
    };
    let key = NoteKey {
        source,
        channel: Channel::try_from(0).unwrap_or_else(|_| unreachable!("channel 0")),
        pitch: Pitch::try_from(u8::from(pad)).unwrap_or_else(|_| unreachable!("pad < 16")),
    };
    let result = takes
        .note_on(pass, Target::Pad(pad), key, velocity, time)
        .and_then(|_| takes.note_off(pass, key, time));
    match result {
        Ok(_) => t.counters.recorded += 1,
        Err(error) => t.error = Some(format!("Hit not recorded: {error}")),
    }
}

pub fn capture_trigger(s: &mut State, pad: usize, velocity: f32) {
    if s.transport.pass.is_none() {
        return;
    }
    let at = app_nanos(s);
    capture_pad(
        s,
        super::media::pad_id(pad),
        velocity_of(velocity),
        KEYBOARD_SOURCE,
        at,
    );
}

pub fn held_start(s: &mut State, on: &NoteOn, at: u64) {
    let (t, takes) = (&mut s.transport, &mut s.takes);
    let Some((pass, time)) = open_pass(t, takes, at) else {
        return;
    };
    match takes.note_on(pass, on.target, on.key, on.velocity, time) {
        Ok(_) => {
            t.held.insert(on.id, time.nanos());
            t.counters.recorded += 1;
            t.counters.held_recorded += 1;
        }
        Err(error) => t.error = Some(format!("Held note not recorded: {error}")),
    }
}

/// Ends only notes that started inside the current pass; both sides are FIFO
/// per key, so the controller's ended note is the pass's oldest of that key.
pub fn held_end(s: &mut State, note: &Note, at: u64) {
    let (t, takes) = (&mut s.transport, &mut s.takes);
    let Some(start) = t.held.remove(&note.on().id) else {
        return;
    };
    let Some(pass) = t.pass else {
        return;
    };
    let time = OriginalTime::from_nanos(at.saturating_sub(pass.start).max(start));
    if let Err(error) = takes.note_off(pass.id, note.on().key, time) {
        t.error = Some(format!("Held note end not recorded: {error}"));
    }
}

/// REC: arm the next pass, or close the current one early.
pub fn record(s: &mut State) {
    if s.transport.pass.is_some_and(|pass| pass.started) {
        close(s, "Recording stopped");
        return;
    }
    if let Some(pass) = s.transport.pass {
        let _ = s.takes.cancel(pass.id);
        s.transport.pass = None;
        s.transport.notice = Some("Armed pass cancelled".into());
        return;
    }
    let now = app_nanos(s);
    let length = loop_nanos(&s.takes);
    let id = match s.takes.arm(s.transport.options) {
        Ok(id) => id,
        Err(error) => {
            s.transport.error = Some(format!("Cannot arm: {error}"));
            return;
        }
    };
    let t = &mut s.transport;
    let start = match t.playing {
        // Overdub on the running loop from its next boundary.
        Some(origin) => origin + now.saturating_sub(origin).div_ceil(length) * length,
        None => {
            let bar = length / u64::from(s.takes.loop_bars().bars());
            let start = now
                + match t.options.count_in {
                    CountIn::Off => 0,
                    CountIn::OneBar => bar,
                };
            start_playing(t, start);
            start
        }
    };
    let mut pass = Pass {
        id,
        start,
        end: start + length,
        started: false,
        closing: false,
    };
    if start <= now {
        match s.takes.start(id) {
            Ok(()) => pass.started = true,
            Err(error) => t.error = Some(format!("Recording could not start: {error}")),
        }
    }
    let t = &mut s.transport;
    t.pass = Some(pass);
    t.held.clear();
    t.reason = None;
    t.error = None;
    t.notice = None;
}

fn start_playing(t: &mut Transport, origin: u64) {
    t.playing = Some(origin);
    t.anchor = None;
    t.scheduled.clear();
}

/// PLAY / STOP.
pub fn play(s: &mut State) {
    if s.transport.playing.is_some() {
        stop(s, "Stopped");
    } else {
        let now = app_nanos(s);
        start_playing(&mut s.transport, now);
        s.transport.notice = None;
    }
}

/// Stop replay. Pending scheduled hits and sounding voices are cancelled by
/// the engine's own panic; an open pass closes (or is discarded if armed).
pub fn stop(s: &mut State, reason: &str) -> bool {
    let queued = s.output.send(Command::Panic);
    let t = &mut s.transport;
    t.playing = None;
    t.anchor = None;
    t.scheduled.clear();
    if t.pass.is_some_and(|pass| !pass.started) {
        if let Some(pass) = t.pass.take() {
            let _ = s.takes.cancel(pass.id);
        }
        t.notice = Some(format!("{reason}; armed pass cancelled"));
    } else if t.pass.is_some() {
        close(s, reason);
    } else {
        t.notice = Some(reason.into());
    }
    queued
}

/// App panic (Escape, PANIC button, MIDI panic CC): silence and stop replay.
pub fn panic(s: &mut State) -> bool {
    stop(s, "Panic")
}

fn close(s: &mut State, reason: &str) {
    if let Some(pass) = &mut s.transport.pass {
        pass.closing = true;
    }
    s.transport.reason = Some(reason.into());
    finish(s);
}

/// Commit a closing pass once no held note is open. Never invents note ends.
fn finish(s: &mut State) {
    let Some(pass) = s.transport.pass.filter(|pass| pass.closing) else {
        return;
    };
    let held = s.takes.pass(pass.id).map_or(0, |p| p.held_count());
    let t = &mut s.transport;
    let reason = |t: &Transport, message: String| match &t.reason {
        Some(reason) => format!("{reason}; {message}"),
        None => message,
    };
    if held > 0 {
        t.notice = Some(reason(t, format!("waiting for {held} held note(s) to end")));
        return;
    }
    let notes = s
        .takes
        .pass(pass.id)
        .map_or(0, |p| p.completed_notes().len());
    if notes == 0 {
        let _ = s.takes.cancel(pass.id);
        t.notice = Some(reason(t, "nothing played, empty pass discarded".into()));
    } else {
        match s.takes.finish(pass.id) {
            Ok(()) => {
                t.notice = Some(reason(
                    t,
                    format!("take {} recorded: {notes} note(s)", pass.id.0),
                ))
            }
            Err(error) => {
                let _ = s.takes.cancel(pass.id);
                t.error = Some(format!("Pass discarded: {error}"));
            }
        }
    }
    t.pass = None;
    t.held.clear();
    t.reason = None;
}

/// Discard the open pass, held notes included; committed takes are untouched.
pub fn cancel(s: &mut State) {
    if let Some(pass) = s.transport.pass.take() {
        let _ = s.takes.cancel(pass.id);
        s.transport.held.clear();
        s.transport.reason = None;
        s.transport.notice = Some("Pass discarded".into());
    }
}

pub fn cycle_loop(s: &mut State) {
    let next = match s.takes.loop_bars().bars() {
        1 => 2,
        2 => 4,
        4 => 8,
        _ => 1,
    };
    let result = brownnote_native::performance::LoopBars::try_from(next)
        .map_err(|error| error.to_string())
        .and_then(|bars| s.takes.set_loop_bars(bars).map_err(|e| e.to_string()));
    match result {
        Ok(()) => {
            // A new cycle length restarts the loop at the next scheduling.
            if s.transport.playing.is_some() {
                let now = app_nanos(s);
                start_playing(&mut s.transport, now);
            }
            s.transport.error = None;
        }
        Err(_) => s.transport.error = Some("Loop length is locked while recording".into()),
    }
}

pub fn toggle_mode(s: &mut State) {
    let options = &mut s.transport.options;
    options.mode = match options.mode {
        PassMode::Overdub => PassMode::Replace,
        PassMode::Replace => PassMode::Overdub,
    };
}

pub fn toggle_count_in(s: &mut State) {
    let options = &mut s.transport.options;
    options.count_in = match options.count_in {
        CountIn::Off => CountIn::OneBar,
        CountIn::OneBar => CountIn::Off,
    };
}

/// Reset after a project replaces the registry. Replay stops; nothing records.
pub fn reset(s: &mut State) {
    let options = s.transport.options;
    s.transport = Transport {
        options,
        ..Transport::default()
    };
}

/// Call after the output advanced, so the frame cursor matches `clock`.
pub fn poll(s: &mut State) -> bool {
    let now = app_nanos(s);
    let before = (s.transport.counters, s.transport.pass.map(|p| p.started));
    if let Some(pass) = &mut s.transport.pass {
        if !pass.started && now >= pass.start {
            match s.takes.start(pass.id) {
                Ok(()) => pass.started = true,
                Err(error) => s.transport.error = Some(format!("{error}")),
            }
        }
        if let Some(pass) = &mut s.transport.pass
            && pass.started
            && !pass.closing
            && now >= pass.end
        {
            pass.closing = true;
        }
    }
    let was = s.transport.pass.is_some();
    finish(s);
    schedule(s, now);
    let beat = position(s, now).map(|(bar, beat, _)| (bar, beat));
    let shown = std::mem::replace(&mut s.transport.shown, beat) != beat;
    shown
        || was != s.transport.pass.is_some()
        || before != (s.transport.counters, s.transport.pass.map(|p| p.started))
}

fn schedule(s: &mut State, now: u64) {
    let Some(origin) = s.transport.playing else {
        return;
    };
    let epoch = s.output.scheduler().epoch();
    let frame = s.output.scheduler().frame();
    let rate = rate(&s.output);
    let anchor = match s.transport.anchor {
        Some(anchor) if anchor.epoch == epoch => anchor,
        // A new stream (device change, project open) re-anchors on its own clock.
        _ => {
            let anchor = Anchor {
                epoch,
                app: now,
                frame,
                rate,
            };
            s.transport.anchor = Some(anchor);
            s.transport.scheduled.clear();
            anchor
        }
    };
    let notes = match s.edit.model.effective_notes(&s.takes) {
        Ok(notes) => notes,
        Err(error) => {
            s.transport.error = Some(format!("Replay: {error}"));
            return;
        }
    };
    let length = loop_nanos(&s.takes);
    let bars = s.takes.loop_bars();
    let tempo = s.takes.tempo();
    let from = now.max(origin);
    let horizon = now + LOOKAHEAD_NANOS;
    let floor = (
        now.saturating_sub(LOOKAHEAD_NANOS),
        EventId {
            take: TakeId(0),
            sequence: 0,
        },
    );
    s.transport.scheduled = s.transport.scheduled.split_off(&floor);
    let mut unsounded = 0;
    for note in notes {
        let Target::Pad(pad) = note.target else {
            unsounded += 1;
            continue;
        };
        let Ok(offset) = note
            .start
            .split_loop(bars)
            .and_then(|position| position.within.at_tempo_nanos(tempo))
        else {
            continue;
        };
        let first = (from - origin) / length;
        let last = horizon.saturating_sub(origin) / length;
        for cycle in first..=last {
            let at = origin + cycle * length + offset;
            if at < from || at >= horizon || !s.transport.scheduled.insert((at, note.id)) {
                continue;
            }
            let Some((slot, group)) = s.assets.playback(pad) else {
                continue;
            };
            let settings = s.settings[pad.index()];
            let delta = u128::from(at.saturating_sub(anchor.app)) * u128::from(anchor.rate) / NANOS;
            let original = s
                .takes
                .take(note.id.take)
                .and_then(|take| take.notes().iter().find(|n| n.on().id == note.id))
                .map_or(OriginalTime::default(), |n| n.on().original_time);
            let result = s.output.schedule(ScheduledEvent {
                epoch,
                frame: anchor.frame + u64::try_from(delta).unwrap_or(u64::MAX / 2),
                original_time: original,
                action: ScheduledAction::NoteOn {
                    id: note.id,
                    trigger: Trigger {
                        pad: pad.index(),
                        slot,
                        group,
                        velocity: f32::from(note.velocity.value()) / 127.0,
                        gain: settings.gain,
                        pan: settings.pan,
                        semitones: settings.pitch,
                    },
                },
            });
            if result.outcome == ScheduleOutcome::Accepted {
                s.transport.counters.scheduled += 1;
            } else {
                s.transport.counters.schedule_lost += 1;
            }
        }
    }
    s.transport.unsounded = unsounded;
}

/// (bar, beat, count-in) of the running loop, both one-based.
fn position(s: &State, now: u64) -> Option<(u64, u64, bool)> {
    let origin = s.transport.playing?;
    let length = loop_nanos(&s.takes);
    let beats = u128::from(s.takes.loop_bars().bars()) * 4;
    // Beat indices from exact ratios: a truncated beat length drifts at bar ends.
    let beat_of = |nanos: u64| u64::try_from(u128::from(nanos) * beats / u128::from(length));
    if now < origin {
        let remaining = u128::from(origin - now) * beats;
        let count = u64::try_from(remaining.div_ceil(u128::from(length))).ok()?;
        return Some((1, count, true));
    }
    let index = beat_of((now - origin) % length).ok()?;
    Some((index / 4 + 1, index % 4 + 1, false))
}

/// The status line: phase, loop position, take summary and loss.
pub fn status(s: &State) -> String {
    let t = &s.transport;
    let now = app_nanos(s);
    let mut parts = Vec::new();
    match (t.pass, position(s, now)) {
        (Some(pass), _) if pass.closing => {
            let held = s.takes.pass(pass.id).map_or(0, |p| p.held_count());
            parts.push(format!("CLOSING T{} · {held} HELD", pass.id.0));
        }
        (Some(pass), Some((_, beats, true))) => {
            parts.push(format!("COUNT-IN {beats} · T{}", pass.id.0))
        }
        (Some(pass), _) if !pass.started || now < pass.start => {
            parts.push(format!("ARMED T{} · NEXT LOOP", pass.id.0))
        }
        (Some(pass), _) => {
            let notes = s
                .takes
                .pass(pass.id)
                .map_or(0, |p| p.completed_notes().len());
            parts.push(format!("REC T{} · {notes} NOTES", pass.id.0));
        }
        (None, Some(_)) => parts.push("PLAYING".into()),
        (None, None) => parts.push("STOPPED".into()),
    }
    if let Some((bar, beat, false)) = position(s, now) {
        parts.push(format!("BAR {bar}.{beat}"));
    }
    let live: Vec<_> = s.takes.takes().filter(|t| t.removal().is_none()).collect();
    let notes: usize = live.iter().map(|t| t.notes().len()).sum();
    parts.push(format!("{} TAKES · {notes} NOTES", live.len()));
    // The mixer's own count of scheduled events it reached, not our sends.
    let engine = s.output.schedule_counters();
    parts.push(format!(
        "REPLAY {} SENT · {} PLAYED",
        t.counters.scheduled, engine.dispatched
    ));
    if t.unsounded > 0 {
        parts.push(format!(
            "{} HELD NOT SOUNDED (NO INSTRUMENT BACKEND)",
            t.unsounded
        ));
    }
    if t.counters.schedule_lost > 0 {
        parts.push(format!("LOSS {}", t.counters.schedule_lost));
    }
    if let Some(take) = s.takes.selected().and_then(|id| s.takes.take(id)) {
        let held: Vec<String> = take
            .notes()
            .iter()
            .filter(|note| !matches!(note.on().target, Target::Pad(_)))
            .map(|note| format!("{}ms", note.duration_nanos() / 1_000_000))
            .collect();
        parts.push(format!(
            "SEL T{}: {} NOTES{}",
            take.id().0,
            take.notes().len(),
            if held.is_empty() {
                String::new()
            } else {
                format!(" · HELD {}", held.join("/"))
            }
        ));
    }
    if let Some(error) = &t.error {
        parts.push(error.clone());
    } else if let Some(notice) = &t.notice {
        parts.push(notice.clone());
    }
    parts.join(" · ")
}

fn pill(key: &str, label: &str, on: bool) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .focusable(true)
        .checked(on)
        .h(16.0)
        .px(6.0)
        .center()
        .radius(3.0)
        .fill(if on { ink("#5a2f2a") } else { ink("#1b251f") })
        .border(1.0, if on { ink("#ff8e7a") } else { ink("#3b4c42") })
        .child(text(label).font(TextStyle::new(9.5, 700.0)).color(if on {
            ink("#ffe1d8")
        } else {
            ink("#c0cbbf")
        }))
}

/// The compact transport strip mounted above the pads.
pub fn strip(s: &State) -> El {
    let t = &s.transport;
    let recording = t.pass.is_some();
    let status = status(s);
    let mut strip = row()
        .key("transport")
        .role("toolbar")
        .label("Recording transport")
        .gap(4.0)
        .center_y()
        .grow(1.0)
        .min_w(0.0)
        .child(
            pill(
                "transport-record",
                if recording { "■ REC" } else { "● REC" },
                recording,
            )
            .role("switch")
            .help("Record one loop cycle from the next loop boundary · press again to stop")
            .on_press(record),
        )
        .child(
            pill(
                "transport-play",
                if t.playing() { "■ STOP" } else { "▶ PLAY" },
                t.playing(),
            )
            .role("switch")
            .on_press(play),
        )
        .child(
            pill(
                "transport-loop",
                &format!("LOOP {}", s.takes.loop_bars().bars()),
                false,
            )
            .help("Loop length in 4/4 bars (1, 2, 4, 8)")
            .on_press(cycle_loop),
        )
        .child(
            pill(
                "transport-mode",
                match t.options.mode {
                    PassMode::Overdub => "OVERDUB",
                    PassMode::Replace => "REPLACE",
                },
                t.options.mode == PassMode::Replace,
            )
            .role("switch")
            .on_press(toggle_mode),
        )
        .child(
            pill(
                "transport-count-in",
                match t.options.count_in {
                    CountIn::Off => "COUNT-IN OFF",
                    CountIn::OneBar => "COUNT-IN 1",
                },
                t.options.count_in == CountIn::OneBar,
            )
            .role("switch")
            .on_press(toggle_count_in),
        );
    if recording {
        strip = strip.child(pill("transport-cancel", "DISCARD", false).on_press(cancel));
    }
    strip.child(
        caption(&status)
            .key("transport-status")
            .role("status")
            .label(&status)
            .min_w(0.0)
            .grow(1.0)
            .truncate()
            .color(if t.error.is_some() {
                ink("#ffad92")
            } else {
                ink("#c0cbbf")
            }),
    )
}

/// Startup scenes for the real binary (see `tests/headless.rs`).
pub fn scene_midi(s: &mut State) {
    // ch 2 / note 40 → pad 4 one-shot; ch 1 / note 60 → held instrument 1.
    let mappings = r#"{"version":1,"mappings":[
        {"type":"note","channel":1,"pitch":40,"target":{"type":"one_shot","pad":4}},
        {"type":"note","channel":0,"pitch":60,"target":{"type":"held","instrument":"1"}}
    ]}"#;
    if let Ok(bars) = brownnote_native::performance::LoopBars::try_from(1) {
        let _ = s.takes.set_loop_bars(bars);
    }
    let midi = s.midi.clone();
    super::midi::fixture(
        &midi,
        SourceId(4242),
        mappings,
        &[
            (0, &[0x90, 60, 100]),
            (100_000, &[0x90, 60, 90]),
            (200_000, &[0x91, 40, 1]),
            (215_000, &[0x91, 40, 127]),
            (500_000, &[0x80, 60, 0]),
            (900_000, &[0x80, 60, 0]),
        ],
    );
    record(s);
}
