//! Copy-only events and a single-producer bounded handoff. No sample ownership.
use super::{EventId, LOOKAHEAD_SECONDS, OriginalTime, SCHEDULE_CAPACITY, StreamEpoch};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed},
};

static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

impl StreamEpoch {
    pub fn fresh() -> Self {
        Self(NEXT_EPOCH.fetch_add(1, Relaxed))
    }
}

/// Runtime slot/settings resolved by the producer, never a stored target.
#[derive(Clone, Copy, Debug)]
pub struct Trigger {
    pub pad: usize,
    pub slot: usize,
    pub group: usize,
    pub velocity: f32,
    pub gain: f32,
    pub pan: f32,
    pub semitones: f32,
}

#[derive(Clone, Copy, Debug)]
pub enum ScheduledAction {
    NoteOn {
        id: EventId,
        trigger: Trigger,
    },
    /// A held note for the mounted sampled instrument; its `NoteOff` ends it.
    HeldOn {
        id: EventId,
        pitch: u8,
        velocity: u8,
    },
    /// Exact ID returned by the producer-side FIFO NoteLifecycle matcher.
    NoteOff {
        id: EventId,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ScheduledEvent {
    pub epoch: StreamEpoch,
    pub frame: u64,
    /// Preserved capture time, not rewritten for lateness or device rate.
    pub original_time: OriginalTime,
    pub action: ScheduledAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleOutcome {
    Accepted,
    TooFuture,
    Full,
    StaleEpoch,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleResult {
    pub outcome: ScheduleOutcome,
    /// A rejected release requested panic and pending-event cancellation.
    pub release_recovery_requested: bool,
}

/// Independent atomic observations, not a transaction or sound/recording ACK.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScheduleCounters {
    pub accepted: u64,
    pub full: u64,
    pub too_future: u64,
    pub stale_epoch: u64,
    pub disconnected: u64,
    pub dispatched: u64,
    pub late: u64,
    pub late_frames: u64,
    pub cancelled: u64,
    pub unavailable: u64,
    pub recovery_requests: u64,
    pub recoveries: u64,
    pub outstanding: usize,
}

#[derive(Default)]
struct Shared {
    frame: AtomicU64,
    outstanding: AtomicUsize,
    connected: AtomicBool,
    panic: AtomicBool,
    accepted: AtomicU64,
    full: AtomicU64,
    too_future: AtomicU64,
    stale_epoch: AtomicU64,
    disconnected: AtomicU64,
    dispatched: AtomicU64,
    late: AtomicU64,
    late_frames: AtomicU64,
    cancelled: AtomicU64,
    unavailable: AtomicU64,
    recovery_requests: AtomicU64,
    recoveries: AtomicU64,
}

pub struct ScheduleSender {
    epoch: StreamEpoch,
    rate: u32,
    sender: rtrb::Producer<ScheduledEvent>,
    shared: Arc<Shared>,
}

pub(crate) struct ScheduleReceiver {
    receiver: rtrb::Consumer<ScheduledEvent>,
    shared: Arc<Shared>,
    pending: Box<[Option<ScheduledEvent>]>,
    len: usize,
    next: Option<u64>,
}

pub(crate) fn channel(epoch: StreamEpoch, rate: u32) -> (ScheduleSender, ScheduleReceiver) {
    let (sender, receiver) = rtrb::RingBuffer::new(SCHEDULE_CAPACITY);
    let shared = Arc::new(Shared::default());
    shared.connected.store(true, Relaxed);
    (
        ScheduleSender {
            epoch,
            rate,
            sender,
            shared: shared.clone(),
        },
        ScheduleReceiver {
            receiver,
            shared,
            pending: vec![None; SCHEDULE_CAPACITY].into_boxed_slice(),
            len: 0,
            next: None,
        },
    )
}

impl ScheduleSender {
    pub fn epoch(&self) -> StreamEpoch {
        self.epoch
    }

    /// Latest callback cursor. Stale observations can conservatively defer
    /// future submissions; actual late frames are counted only at dispatch.
    pub fn frame(&self) -> u64 {
        self.shared.frame.load(Relaxed)
    }

    pub fn counters(&self) -> ScheduleCounters {
        let s = &self.shared;
        ScheduleCounters {
            accepted: s.accepted.load(Relaxed),
            full: s.full.load(Relaxed),
            too_future: s.too_future.load(Relaxed),
            stale_epoch: s.stale_epoch.load(Relaxed),
            disconnected: s.disconnected.load(Relaxed),
            dispatched: s.dispatched.load(Relaxed),
            late: s.late.load(Relaxed),
            late_frames: s.late_frames.load(Relaxed),
            cancelled: s.cancelled.load(Relaxed),
            unavailable: s.unavailable.load(Relaxed),
            recovery_requests: s.recovery_requests.load(Relaxed),
            recoveries: s.recoveries.load(Relaxed),
            outstanding: s.outstanding.load(Relaxed),
        }
    }

    /// Producer-thread operation. One capacity budget covers ingress AND
    /// callback-pending events. Arbitrary deadline order is supported; equal
    /// deadlines retain submission order without callback sorting.
    pub fn try_schedule(&mut self, event: ScheduledEvent) -> ScheduleResult {
        let s = &self.shared;
        let outcome = if !s.connected.load(Relaxed) {
            s.disconnected.fetch_add(1, Relaxed);
            ScheduleOutcome::Disconnected
        } else if event.epoch != self.epoch {
            s.stale_epoch.fetch_add(1, Relaxed);
            ScheduleOutcome::StaleEpoch
        } else if s.outstanding.load(Relaxed) >= SCHEDULE_CAPACITY {
            s.full.fetch_add(1, Relaxed);
            ScheduleOutcome::Full
        } else if event.frame.saturating_sub(self.frame())
            > u64::from(self.rate) * LOOKAHEAD_SECONDS
        {
            s.too_future.fetch_add(1, Relaxed);
            ScheduleOutcome::TooFuture
        } else {
            // Only this producer increments; the callback only decrements.
            s.outstanding.fetch_add(1, Relaxed);
            match self.sender.push(event) {
                Ok(()) => {
                    s.accepted.fetch_add(1, Relaxed);
                    ScheduleOutcome::Accepted
                }
                Err(_) => {
                    s.outstanding.fetch_sub(1, Relaxed);
                    s.full.fetch_add(1, Relaxed);
                    ScheduleOutcome::Full
                }
            }
        };
        let recovery = outcome != ScheduleOutcome::Accepted
            && matches!(event.action, ScheduledAction::NoteOff { .. })
            && outcome != ScheduleOutcome::Disconnected;
        if recovery {
            s.recovery_requests.fetch_add(1, Relaxed);
            s.panic.store(true, Relaxed);
        }
        ScheduleResult {
            outcome,
            release_recovery_requested: recovery,
        }
    }
}

impl ScheduleReceiver {
    pub fn cursor(&self, frame: u64) {
        self.shared.frame.store(frame, Relaxed);
    }

    pub fn drain(&mut self, valid: impl Fn(&ScheduledEvent) -> bool) -> bool {
        for _ in 0..SCHEDULE_CAPACITY {
            let Ok(event) = self.receiver.pop() else {
                break;
            };
            if !valid(&event) {
                self.shared.unavailable.fetch_add(1, Relaxed);
                self.shared.cancelled.fetch_add(1, Relaxed);
                self.shared.outstanding.fetch_sub(1, Relaxed);
                continue;
            }
            self.pending[self.len] = Some(event);
            self.len += 1;
            self.next = Some(self.next.map_or(event.frame, |next| next.min(event.frame)));
        }
        if self.shared.panic.swap(false, Relaxed) {
            self.cancel_all();
            self.shared.recoveries.fetch_add(1, Relaxed);
            true
        } else {
            false
        }
    }

    fn remove(&mut self, index: usize) -> Option<ScheduledEvent> {
        let event = self.pending[index];
        self.pending.copy_within(index + 1..self.len, index);
        self.len -= 1;
        self.pending[self.len] = None;
        self.shared.outstanding.fetch_sub(1, Relaxed);
        event
    }

    /// Scan only at deadlines. Stable compaction is bounded by 256 events,
    /// even for a whole queue due at one frame; there is no deadline sorting.
    pub fn pop_due(&mut self, frame: u64) -> Option<ScheduledEvent> {
        if self.next.is_none_or(|next| next > frame) {
            return None;
        }
        let mut next = None;
        for i in 0..self.len {
            let event = self.pending[i]?;
            if event.frame <= frame {
                let event = self.remove(i)?;
                self.shared.dispatched.fetch_add(1, Relaxed);
                if event.frame < frame {
                    self.shared.late.fetch_add(1, Relaxed);
                    self.shared
                        .late_frames
                        .fetch_add(frame - event.frame, Relaxed);
                }
                return Some(event);
            }
            next = Some(next.map_or(event.frame, |n: u64| n.min(event.frame)));
        }
        self.next = next;
        None
    }

    pub fn unavailable(&self) {
        self.shared.unavailable.fetch_add(1, Relaxed);
    }

    pub fn cancel_slot(&mut self, slot: usize) {
        let mut i = 0;
        while i < self.len {
            if matches!(self.pending[i], Some(ScheduledEvent {
                action: ScheduledAction::NoteOn { trigger: Trigger { slot: s, .. }, .. }, ..
            }) if s == slot)
            {
                self.remove(i);
                self.shared.cancelled.fetch_add(1, Relaxed);
            } else {
                i += 1;
            }
        }
    }

    pub fn cancel_all(&mut self) {
        self.pending[..self.len].fill(None);
        self.shared.cancelled.fetch_add(self.len as u64, Relaxed);
        self.shared.outstanding.fetch_sub(self.len, Relaxed);
        self.len = 0;
        self.next = None;
    }

    pub fn cancel_queued(&mut self) {
        self.cancel_all();
        for _ in 0..SCHEDULE_CAPACITY {
            if self.receiver.pop().is_err() {
                break;
            }
            self.shared.cancelled.fetch_add(1, Relaxed);
            self.shared.outstanding.fetch_sub(1, Relaxed);
        }
    }
}

impl Drop for ScheduleReceiver {
    fn drop(&mut self) {
        self.shared.connected.store(false, Relaxed);
        self.cancel_queued();
    }
}
