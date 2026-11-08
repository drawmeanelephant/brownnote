# Workflow fixture v1, not a native project

This neutral score and synthetic audio recipe support the first PR for
[#111](https://github.com/drawmeanelephant/brownnote/issues/111).
**Passing `workflow.rs` means fixture integrity and expectation consistency
only. It is not E2E proof, input injection, or a native-to-DAW run.**

`score-v1.json` is a test worksheet, not a competing document/event model.
Its cue labels identify rows for comparison, not production event IDs.
Nanoseconds are a human-readable reference unit here, not a choice for #104.
PR120/123's contracts are not frozen; no implementation is copied from them.
Map this score to the owners' public APIs only after those APIs land.
The runtime convergence driver is intentionally absent, not ignored or
pretending to pass.

## Musical rationale and exact reference

Two bars of 4/4 at **120 BPM** last **4,000,000,000 ns**. This is an explicit
fixture tempo, not a change to D11's 116 BPM default. One beat is 500 ms;
a sixteenth is 125 ms. Deliberately off-grid starts make restoration observable
without a quantization tie. All offsets are relative to the same loop start,
including the overdub's second-bar entries. Play one initial pass and one
overdub pass; the overdub must not replace the first pass.

| Cue | Take | Destination | Original start (ms) | Span (ms) | Velocity | Full 1/16 start (ms) |
|---|---|---|---:|---:|---:|---:|
| pad-a | base | custom pad | 60 | 1000 | 48 | 0 |
| pad-b | base | same custom pad | 185 | 1000 | 112 | 125 |
| note-a | base | instrument, ch 1, pitch 60 | 510 | 1000 | 80 | 500 |
| note-b | base | instrument, ch 1, pitch 60 | 760 | 1000 | 96 | 750 |
| pad-c | overdub | same custom pad | 2060 | 1000 | 72 | 2000 |
| note-c | overdub | instrument, ch 1, pitch 67 | 2510 | 1000 | 88 | 2500 |

Pad span means **dry source length**, not a note-off, gated pad duration,
or predicted pedal-render length. The first two dry spans overlap for 875 ms.
Disable choke so both hits survive; retain their distinct velocities.
Require **Rat Sketch → Tape Echo**, in that order, using catalog defaults.
Future evidence must record the resolved pots/mix and rendered frame count,
including the tail. This first slice cannot specify the output length or
claim sound equivalence before the public workflow renders it.

Instrument spans are exact note durations, independent of release tails.
The two channel-1 pitch-60 note-ons overlap for 750 ms. D14 FIFO note-offs
at 1510 and 1760 ms end note-a and note-b respectively; the held pitch-67
note ends at 3510 ms. There are **four base events + two overdub events =
six events**, three pad hits and three notes, with six distinct identities
once the production owner assigns them.

**#108/D18 remains unresolved:** backend and instrument asset are null.
Do not silently select SoundCraft's sampler, an SF2/SFZ engine, a plugin,
or an instrument recording. #108 must record its chosen backend, version,
license, asset hash/availability, and reference settings before runtime
instrument validation can proceed.

## Reversible expectations, for a future public-API driver

1. Record both passes with quantization off; compare all six score rows.
2. Enable full-strength per-take 1/16 quantization for both takes. Only the
   effective onsets change to the last table column. Durations, velocities,
   cue identities, and original offsets remain unchanged.
3. Move note-c forward one sixteenth to **2625 ms** as one edit/undo step.
4. Delete pad-b: **five** effective events remain. Undo deletion: six return,
   with note-c still at 2625 ms. Undo move: note-c returns to 2500 ms.
5. Disable quantization: all starts return exactly to the original column.
   The fixture stores the explicit restored list, not just a tolerance.
6. Shorten to one bar: only pad-a, pad-b, note-a, note-b are visible.
   Restore two bars: all six return, without deleting second-bar data.
7. Save the current edited state, quit, reopen and relocate using #100.
   Compare take membership, identities, velocities, durations, original and
   effective positions, and enabled edit settings. D15 does not persist undo
   history: do not require undoing a pre-quit edit after reopening.

The integrity tests check these worksheet values and relationships only.
They do not implement or exercise any edit, take, FIFO, or persistence API.

## Reproduce synthetic media

From the repository root, choose a **new** output path in a scratch directory:

```sh
mkdir -p native/target/workflow-manual
python3 native/tests/fixtures/workflow/generate_audio.py \
  native/target/workflow-manual/workflow-pad-v1.wav
```

The generator uses Python's standard library and refuses to overwrite a file.
It makes exactly 48,000 mono PCM16 frames at 48 kHz (one second), a 200 Hz
square wave with 5 ms linear edge fades, peak magnitude 4096/32768.
For frame `i`, magnitude is
`4096 * min(i, 47999 - i, 240) // 240`; sign is positive for
`i % 240 < 120`, negative otherwise. No random seed, platform sine rounding,
user media, downloads, or instrument/backend selection is involved.
Synthetic audio and score use the repository's MIT license; no third-party
recording or sample license is needed.

`workflow.rs` invokes this generator into its own temporary directory, reads
the WAV with hound, checks every PCM frame against the reference recipe,
checks repeatability, and checks that overwrite attempts fail. Python 3 is
required; absence is a failure with a named prerequisite, not a silent skip.

## Future mapping, not wiring

| Owner | Public boundary needed by #111 |
|---|---|
| #98 / #104 | Map cue labels to stable event IDs, media references, and the public timebase; exact nanosecond round trips where representable |
| #99 | Import generated WAV and assign the custom pad without an external media dependency |
| #100 | Save/recover/reopen/relocate the project, preserving #98's document and media ownership |
| #105 | Deterministic timestamped public input injection plus separate computer-keyboard and IAC runs |
| #106 | Arm, record, stop, replay, overdub, loop length and take inspection |
| #107 | Quantize toggle, move/delete, undo and effective/original event inspection |
| #108 | Resolve D18 and play/record/recall the chosen real sampled instrument |
| #109 | Export this same captured performance, per-hit retained audio, editable notes and identity manifest |
| #110 | Re-pin/re-probe SoundCraft, import that bundle, target edits and save/reopen |
| #103 | Repeat from the actual packaged artifact outside the source checkout |

No shared wiring or dependency is requested for this slice. Future adapters
belong at these public boundaries, not in a second model or renderer here.
At exact native boundaries use zero tolerance for score offsets, velocities
and note lengths. For sample/tick conversions the owner must publish units,
rounding, numeric maximum error and observed error per event before claiming
success; do not infer a production tolerance from this grid-friendly score.
See the [manual evidence skeleton](../../../docs/workflow-111-first-slice-runbook-v1.md).
