# Takes model and recording transport (#106)

The takes model is an in-memory producer-side registry. The app's recording
transport (below) is its only writer, and committed takes save with the
project. It has no editable export or sound acknowledgement. Refs #106.

## Public boundary

The library exports this model as **`brownnote_native::takes`**. No dependency,
State field, main module, UI, or engine wiring is needed for this pure slice.
The public integration test imports the real library, not a path bootstrap.

- `Takes::new(loop_bars, first_take)` chooses a D11 loop before capture.
  `LoopBars` accepts only 1/2/4/8 bars in 4/4. The tempo starts at 116 BPM.
  The project owner supplies a globally unused `TakeId` range. Accepted arms,
  including cancelled passes, consume one ID; rejected operations consume none.
  Reopen goes through `TakesDocument::to_takes` (see "Save and reopen").
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

## App recording transport (#105/#106 integration)

`ui/transport.rs` mounts a compact strip above the pads: `● REC`, `▶ PLAY`,
`LOOP 1/2/4/8`, `OVERDUB/REPLACE`, `COUNT-IN OFF/1`, `DISCARD` while a pass is
open, and a status line (`transport-status`). It drives this model; it adds no
second registry.

- REC from stopped starts the loop and records one loop cycle from now, or
  after a one-bar count-in. REC while playing arms the next loop boundary, so
  overdubs align with replay. A pass closes at its cycle end, on a second REC,
  on STOP, or on panic (Escape, the PANIC button, a mapped MIDI panic CC).
  A started pass commits what was played; an armed one is cancelled; an empty
  one is discarded. `DISCARD` cancels the open pass, held notes included.
- Keyboard, pointer and MIDI one-shot hits are captured at the app clock as
  zero-length pad notes under reserved one-shot sources (`u128::MAX`,
  `u128::MAX - 1`), so they never end a device's held note. Velocities 1–127
  and flams keep their exact take-relative nanoseconds (D9).
- MIDI held-instrument starts/ends come from the controller's FIFO lifecycle
  and keep their real durations (D14). MIDI backend times are mapped to the app
  clock by anchoring each connection's first packet. A pass whose cycle ends
  with keys down waits (`CLOSING … HELD`) and never invents note ends. A
  disconnect or refresh reconciliation ends them at the reconcile time.
- Replay sends the edit layer's effective notes (D13/D15) through
  `Output::schedule` with a 500 ms lookahead, re-anchoring when the output
  stream changes. The status shows sends, the mixer's own dispatched count and
  any refused events (`LOSS`). STOP/panic sends the engine panic, which also
  cancels pending scheduled hits.
- Held notes sound and replay through the sampled instrument when one is
  loaded (see `sampled-instrument.md`). Without one, they are captured, saved
  and shown with their durations, and the status reports
  `N HELD NOT SOUNDED (NO INSTRUMENT LOADED)`. No metronome or count-in click
  is generated.

`--scene transport-midi` runs a virtual MIDI input (fixture packets through the
real controller and dispatch path): overlapping held notes on one key, and a
1/127 velocity flam on a mapped pad. `scripts/transport.json` records, replays
and panics a keyboard take. Both run in `tests/headless.rs`. This is
deterministic software evidence only. A named-controller hardware check is
still required before #105 can close, and listening checks remain manual.

## Save and reopen

Committed takes and the current edit layer are saved in the project document
(see `project-contract.md`). Armed or recording passes never save:
`Snapshot::with_takes` returns `Error::PassActive`, and the app refuses save or
open until the pass stops. Reopen rebuilds every note through `NoteLifecycle`
and continues take IDs above every saved ID.
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
The transport is covered by `ui::transport::tests`, the project round trip by
`ui::projects::tests` and `tests/project_takes.rs`, and the real binary by
`tests/headless.rs`. Held-instrument sound, audio tails, editable export,
named-controller hardware and listening remain later gates.
