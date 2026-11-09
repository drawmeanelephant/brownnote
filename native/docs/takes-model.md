# Pure takes model (#106, first slice)

This is an in-memory producer-side model, not a recorder mounted in the app.
It has no transport, MIDI backend, scheduler changes, UI, save schema, editable
export, or sound acknowledgement. Refs #106; the issue stays open.

## Public boundary

The library exports this model as **`brownnote_native::takes`**. No dependency,
State field, main module, UI, or engine wiring is needed for this pure slice.
The public integration test imports the real library, not a path bootstrap.

- `Takes::new(loop_bars, first_take)` chooses a D11 loop before capture.
  `LoopBars` accepts only 1/2/4/8 bars in 4/4. The tempo starts at 116 BPM.
  The project owner supplies a globally unused `TakeId` range. Accepted arms,
  including cancelled passes, consume one ID; rejected operations consume none.
  There is no reopen/ID-registry import API in this slice.
- `arm(PassOptions)` creates a tentative pass and locks tempo.
  `start(id)` changes armed to recording. Default options are overdub, no
  count-in, and metronome off (D12). One-bar count-in and metronome are intent
  metadata only; no ticks or recording-start times are generated here.
- `note_on(id, target, key, velocity, original_time)` returns an existing
  `EventId` with a monotonically increasing per-take sequence. Zero-based MIDI
  channel, pitch and velocity use the performance contract's validated types.
  `note_off(id, key, original_time)` uses its `NoteLifecycle` FIFO and returns
  the exact ended ID. Flams, simultaneous hits, repeated pitches, source and
  channel identities, semantic targets, velocities and overlapping ends survive.
  Original unwrapped nanoseconds are never rounded or quantized (D9, D14).
- Each pass has its own take-relative clock. The future input/transport owner
  must route releases to the owning pass, including across loop wraps, and
  convert timestamps into that pass's clock. This model does not infer
  cross-take time origins, sustain, panic releases, or audio tails.
- `finish(id)` commits a nonempty pass with no held notes and selects it.
  Empty, armed-only, or held-note finish fails atomically and keeps the pass
  locked for retry/cancel. No note ends are fabricated. `cancel(id)` explicitly
  discards tentative capture only, including held notes, and unlocks that pass.
- Replace must be chosen anew for each pass. At arm it snapshots all live
  committed takes, including muted ones. Only a successful finish marks those
  still live as `Removal::ReplacedBy(id)`. New overdubs and takes already removed
  at arm are outside that snapshot. Later user removals keep `Removal::User`.
  Cancel/failure never removes prior takes. Originals remain inspectable.
- `select`, `set_muted`, `remove`, and `restore` operate on committed IDs.
  Selection is not solo. Removing the selected take clears selection; restoring
  does not select, unmute, or implicitly undo another replacement. Removing a
  replacement does not auto-restore its predecessors. Restoration is explicit.
  Removed takes reject selection/mute/removal; restoring a live take fails.
- `takes()` and `take(id).notes()` include all recoverable originals.
  `audible_notes()` filters mute, removal, and original-onset loop visibility;
  it is neither chronological scheduling nor proof of audibility.
  Exact loop end is outside the half-open visible interval. Shortening hides,
  never deletes, and held ends remain unwrapped beyond the loop (D11).
- `set_tempo` uses `TempoControl`: any armed/recording pass prevents edits (D10).
  Every committed `TakeTiming` retains its recording tempo; views derive musical
  positions for later tempo changes without rewriting originals. Quantization
  stays off; reversible edits are #107's scope (D13). Loop changes also reject
  while any pass is active, so recording uses its preselected loop length.

## Verification

Always use the swarm's shared whole-check lock, Cargo target and two jobs.
Run `python3 native/tests/check_takes.py`. It checks the actual exported
library's fixture names, its public-caller test, and the full native gate.
The former source-only harness has been removed. The equivalent commands are:

```sh
cargo test --manifest-path native/Cargo.toml --lib takes::tests
cargo test --manifest-path native/Cargo.toml --test takes
make native-test
```

The checked-in overdub trace specifies 12 independent original notes over
three passes, including 15 ms same-pitch flams and same-grid collisions. The
visibility fixture specifies both sides of rational 116 BPM and exact 120 BPM
loop boundaries. Tests cover restoration, replacement snapshots, cancel and
failed-finish atomicity, overlapping tempo locks, immutable timing, and integer
boundaries. These do not measure rendered PCM, physical audio, or listening.
Save/reopen, actual input capture/replay, held-note stop/panic and tails, UI,
and editable export remain later issue gates.
