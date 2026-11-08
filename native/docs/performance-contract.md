# Performance contract, pure first slice (#104)

This contract depends on #98's `AssetId` and `PadId`; land #98 first.
It is not an audio scheduler or a recorder, and owns no JSON serialization.

## Identity and originals

An `EventId` is a project-stable take ID plus a per-take sequence. The owning
registry must supply unique take/source/instrument IDs and, on reopen,
continue sequences above all saved IDs. Each note-on is distinct. A target
references a pad, encoded sample content, or instrument, never a mixer slot.

Capture original integer nanoseconds from take start. Keep each onset and
completed note's end immutable. A lifecycle matcher pairs note-off FIFO by
logical source, zero-based MIDI channel and pitch, across take boundaries.
It returns the exact targeted event ID. Duplicate IDs and off-before-on
timestamps fail without consuming held notes. Unmatched off is explicit.
The matcher is producer-side code with allocation, never callback code.

Normalize MIDI note-on velocity zero to note-off before constructing `Velocity`.
Source reconnect identity is registry-owned, not an enumerated port index.
Sustain/controller interpretation and source-disconnect releases belong to
the input/recorder slices; this contract does not claim sustain support.

## Tempo, quantize and loops

Tempo is positive integer milli-BPM, default 116 BPM. A take captures its
recording tempo. `TempoControl` prevents tempo edits while **any** take is
armed or recording, including overlapping arms. Finishing one take does not
unlock another. No automation or mid-take tempo change.

For original time `n`, recording tempo `r`, and playback tempo `p`, the exact
derived time is `n * r / p`. Thus post-record tempo edits keep musical
position (D10). Derived musical units are exact integer `n * r`; there are
60,000,000,000,000 units per quarter. This is not MIDI PPQ. Device sample rate
does not enter the original or musical position.

Quantize is off by default, per take. Sixteenth snaps the onset at full
strength to the nearest quarter-beat, with ties going later. Shift note-off
by the same musical delta, preserving duration and identity. Flams can share
a derived position but never merge. Turning quantize off derives every
original onset/end exactly at the recording tempo, including integer extremes.
Nanoseconds and device frames are rounded only at their output boundary,
nearest integer with ties later; frame conversion does not double-round
through integer nanoseconds. Overflow is an error, never saturation.

Loops are 1, 2, 4 or 8 bars in 4/4. Positions are unwrapped: a held note can
cross a boundary and still receive its own FIFO off. Exact loop end belongs
to the next iteration at zero. Evaluate shortened-loop visibility against
original musical positions before quantization. Shortening hides, never
deletes. A visible note snapped to loop end plays at the next iteration's
zero; its duration remains unwrapped. Loop scheduling/repetition is later work.

## Clock and bounded scheduler policy (later integration)

`ClockAnchor` maps a monotonic host timestamp to take nanoseconds off callback.
It permits timestamps before the anchor when still after take start. Reject
timestamps before take start, overflow, and stale stream epochs. #101 supplies
the CPAL playback/host anchor; stream reopen/device change needs a fresh epoch
and anchor, not reuse of reset meter frame counts.

The additive scheduled-trigger path will carry event identity, stream epoch
and an absolute integer frame deadline, alongside existing immediate triggers.
The producer converts/sorts events off callback into preallocated bounded
storage, with stable arrival order for equal deadlines. Capacity is 256 and
lookahead is at most two device seconds; farther events remain with the
producer and are retried later, never inserted into the callback queue.

Admission laws in this slice are pure decisions, not a queue implementation:

- Full: reject the new event, increment a full/drop counter, and return
  rejection to the caller. Never claim it was sounded/recorded successfully.
- Too future: return to the producer for later submission; count separately,
  without changing the original timestamp.
- Late: admit only if space exists; dispatch at offset zero and increment the
  late counter once at dispatch, recording lateness separately from originals.
- Ready: use the bounded integer offset within the half-open callback block.
  An event exactly at block end remains pending for the next block.
- Stream epoch mismatch: reject stale work; never play it against a new clock.

Queue admission is not a sound acknowledgement. Recording/sounded feedback
must distinguish captured input, accepted dispatch and rejected dispatch.
Rejected note-off must trigger a reported bounded release/panic recovery,
not silently leave a held voice. Callback integration must use no locks,
sorting, allocation, destruction of shared samples, logging or I/O.

Only scalar admission/block-placement laws and fixtures land here. Actual
queues, counters, CPAL consumption, variable-buffer audio rendering and
scheduled-traffic allocator proof wait for #101 and #102 and remain #104
close gates. Pure block-offset equality is not an audio render measurement.

## Library integration and validation

`native/src/lib.rs` exports `performance` alongside #98's `project` module.
It uses #98's regular serde/hash dependencies. No additional dependencies,
State fields, poll calls, view slots or engine/output wiring are needed.

`native/tests/performance.rs` imports the actual exported modules with
`use brownnote_native::{performance, project};` and `use performance::*;`.
Run the focused real-manifest test and `make native-test` against the exported
artifact. The focused command is:

```sh
python3 native/tests/check_contracts.py performance
```

It runs the actual native manifest and honors `CARGO_TARGET_DIR`, without a
bootstrap manifest or path-included modules. Checked-in clock fixtures have independently
specified 44.1/48-kHz frame positions. Pure tests cover FIFO overlaps, flams,
quantize reversibility, multi-take tempo locks, loop boundaries, clock epochs,
integer overflow and offsets with callback sizes 31, 64 and 256.
