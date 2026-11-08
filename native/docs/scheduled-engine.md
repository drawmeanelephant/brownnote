# Bounded engine scheduling (#104 follow-up)

The existing exported `performance`, `engine`, and `output` modules contain
this path. No root module, dependency, UI, or application-state wiring is
required. Existing `Command::Trigger` callers retain immediate behavior.
This is not a transport, MIDI backend, take recorder, or #106 delivery.

## Producer API

`Output::scheduler()` exposes one `ScheduleSender`. `Output::schedule(event)`
returns a `ScheduleResult`, never a sound or recording acknowledgement.
Standalone mixer users move its unique sender out with `take_scheduler()`.

Resolve a semantic target to a currently loaded runtime slot and settings,
then submit a copy-only `ScheduledEvent` containing:

- The sender's stream epoch and an absolute integer stream-frame deadline.
- The untouched original take nanoseconds (D9).
- `NoteOn { id, trigger }`, or `NoteOff { id }` targeting the exact ID returned
  by the producer-side FIFO `NoteLifecycle` matcher (D14).

The sender accepts arbitrary deadline order. Equal deadlines retain arrival
order. Capacity is **256 total**, counting both ingress and callback-pending
events. Moving ingress into the callback does not free capacity. Lookahead
is inclusive at two device seconds from the latest callback cursor.

| Result/state | Behavior and observable counters |
| --- | --- |
| Accepted | `accepted` increments. No claim that a sample was played or recorded. |
| Full | Reject the new event; `full` increments. Originals remain with the caller. |
| Too future | Reject/defer for producer retry; `too_future` increments. |
| Stale epoch | Reject work from a previous stream; `stale_epoch` increments. |
| Disconnected | Receiver was destroyed; reject and increment `disconnected`. |
| Late dispatch | Use offset zero of the first block that sees it; count `late` once and add actual `late_frames`. Never rewrite the original. |
| Exact block end | Keep pending until the next nonempty block. |
| Missing/retiring sample | Cancel at callback ingestion, counting `unavailable` and `cancelled`. |
| Unload/panic | Cancel pending note-ons for the retiring slot, or all queued work for panic. Existing voice/tail fading and off-thread sample return still apply. |

Every rejected release on a connected stream requests bounded recovery.
The result sets `release_recovery_requested`; `recovery_requests` increments.
At the next render, the callback cancels queued/pending work, invokes the
existing panic fade on active voices and steal tails, and increments
`recoveries`. Multiple requests before that boundary coalesce into one panic.
The producer must observe the result and rebuild any desired future work,
not assume an accepted note-on will survive recovery.

`dispatched` counts callback event processing, not audibility. A dispatched
release may target a sample that already ended. `cancelled` counts accepted
events removed without dispatch. `outstanding` counts accepted events not yet
dispatched/cancelled. Counters are independent atomic observations, so a
concurrent snapshot is not a transactional reconciliation.

## Callback work and sample lifetime

The scheduled handoff uses its own preallocated SPSC ring, independent of the
existing lifecycle-command ring. The mixer owns a preallocated 256-entry
pending array. It drains at most 256 events per block, compares integer
deadlines, and stably removes due entries without sorting. The per-frame
fast path checks one cached minimum deadline. A saturated due batch can
perform at most 256 bounded array scans/removals, each at most 256 entries.
There are no callback locks, I/O, allocations, or deallocations.

Scheduled payloads contain no `Arc` or sample data. Unload cancels pending
slot references; ingress is checked against the current retiring state
before sample collection. A slot cannot retire while a voice or steal tail
still reads it. Slot reuse still requires receiving `Returned::Retired`.
An old scheduled hit must never play a replacement sample.

## CPAL conversion and reopen

Physical `Output` constructs its mixer with the same fresh epoch used by
`PlaybackClock`. The callback publishes CPAL's playback observation before
draining commands/rendering. Mixer stream frames and timestamp stream frames
both start at zero and advance by actual rendered frame counts.

Off callback, construct `PlaybackScheduleClock` from a `PlaybackTimestamp`
and the transport's take time at that observation's **predicted playback**
instant, not its callback invocation instant. It converts:

- CPAL-domain input instants to original take nanoseconds or stream frames.
- Immutable take nanoseconds to stream deadlines.
- Derived `MusicalPosition` at playback tempo to stream deadlines (D10),
  rounding only once, nearest frame with ties later.

Host conversion subtracts the predicted playback instant, including negative
offsets. It rejects pre-take/pre-stream values, overflow, and wrong epochs.
CPAL instants are not UI `Instant`s or MIDI-driver timestamps. A future
input adapter must establish its clock-domain relationship off callback;
this API does not invent one. Timestamp overflow/staleness must be checked
using the existing observation seam before anchoring.

Every reopen, including the same device/rate, gets a fresh epoch and empty
scheduler. Device/rate changes stop old voices (D16). Never reuse meter
frames as take time. Recompute new deadlines from immutable nanoseconds or
musical positions against the new anchor/rate. Loops remain unwrapped:
quantized exact loop end is next iteration zero, while note-off retains its
unwrapped duration. Repetition/loop transport remains future work.

## Verification and remaining integration

Run all commands through the swarm's shared validation-lock/target wrapper:

```sh
cargo test --manifest-path native/Cargo.toml performance
cargo test --manifest-path native/Cargo.toml --test scheduling
cargo test --manifest-path native/Cargo.toml --test rt_alloc
python3 native/tests/check_contracts.py performance
make native-test
python3 tests/studio_smoke.py
make test
cargo run --manifest-path native/Cargo.toml --example scheduling_probe
```

When other checkouts reuse that Cargo target, refresh this lane's
`native/src/performance.rs` modification time **inside the held lock** before
each Cargo check (`touch native/src/performance.rs`). Otherwise Cargo can
reuse a newer library artifact built from another checkout, even though
whole commands are serialized. Do not clear shared caches or create another
target directory to work around it.

The public-library scheduling suite measures rendered sample positions
against the existing checked-in nanosecond/frame CSV and independent
boundary indices at buffers 31/64/256 and rates 44.1/48 kHz. It checks
late/future/full outcomes, recovery, FIFO overlapping-note releases and fade
samples, equal-deadline order, loop-boundary/tempo conversion, repeated
same-rate/rate-change epochs, virtual output, and sample-slot reuse.
`rt_alloc` runs saturated scheduled traffic, voice stealing, targeted release,
full-release recovery, cancellation and retirement at all six configurations.
The silent `scheduling_probe` opens physical default output at 44.1/48/48 kHz,
checks real timestamp/engine epoch identity, submits converted note-on/off
deadlines, and verifies dispatch, release and stale-epoch rejection. It does
not measure physical sample presentation or latency.

No new `lib.rs`, `main.rs`, UI, Cargo dependency, or export wiring is needed.
Future input/transport consumers must supply semantic-to-slot resolution,
clock-domain anchoring, retry deferred input, and handle release recovery.
They must distinguish captured input, accepted/dispatched work, and recording.
No recording acknowledgement is added here.

Deterministic renders do not prove physical presentation latency or sound
quality. Maintainer listening, device-loss/reconnect, and physical latency
measurements remain separate gates; no external MIDI hardware was used.
