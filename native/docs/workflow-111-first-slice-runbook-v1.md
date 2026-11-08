# #111 manual evidence runbook, first-slice v1

**Status: skeleton, NOT EXECUTED. No native-to-SoundCraft convergence proven.**

This is the fixture/runbook first PR allowed by the 8 November 2026
[review adjustments](https://github.com/drawmeanelephant/brownnote/issues/111).
Real convergence waits for Wave 4 and the public workflows from
#99, #100 and #105–#110. Do not close #111 from fixture tests.
No readiness percentage is meaningful here.

## Inputs and ownership

- Use the [v1 fixture reference](../tests/fixtures/workflow/README.md) and
  `score-v1.json`, not a separately authored MIDI file or a fabricated session.
  Only generated synthetic media enters this procedure.
- Keep #108/D18 **unresolved** until its owner records the real sampled
  instrument/backend, exact version, asset hash, license and configuration.
  SoundCraft's sampler is not an implicit native backend.
- Await #98/#104 public contracts; PR120/123 were not frozen for this slice.
  Record the mapping from fixture cue to production event ID and take ID.
- Use only the public owner APIs listed in the fixture reference. If one is
  missing, stop that stage and report **BLOCKED**, with owning issue. Do not
  add a private serializer, scheduler, exporter or alternate project model.
- This slice requests **no integrator wiring or dependencies**. Cargo
  autodiscovers `tests/workflow.rs` using existing dev dependencies.

## Evidence record (fill per run, never pre-mark PASS)

| Field | Value |
|---|---|
| Date, operator, run ID | NOT RECORDED |
| Brownnote full commit and artifact SHA-256 | NOT RECORDED |
| Source checkout or packaged artifact; #103 artifact/version | NOT RECORDED |
| macOS version, architecture, public API revisions | NOT RECORDED |
| Fixture version and score/WAV SHA-256 | NOT RECORDED |
| #108 instrument/backend/version/license/asset hash/settings | UNRESOLVED |
| Brownnote resolved pedal order/pots/mix; source/render frames | NOT RECORDED |
| Input mode: deterministic / computer keyboard / IAC / real controller | NOT RUN |
| Output mode/device and sample rate | NOT RECORDED |
| Target app tag/full commit/app+CLI versions/artifact hashes | NOT RE-PINNED |
| Time units/rounding and per-event conversion maximum/observed error | NOT RECORDED |
| Logs, public inspections, per-hit audio and reference-bounce paths/hashes | NOT RECORDED |
| Maintainer listening observations | NOT RUN |

Store evidence in a run-specific scratch directory outside personal projects.
Preserve sanitized failures and exit codes. Never publish user paths, device
serials, credentials or personal media. Use PASS / FAIL / BLOCKED / NOT RUN
with a reason for each gate, not an overall green summary over missing rows.

## Procedure, awaiting public workflows

1. **Fixture integrity only.** Generate the WAV following the fixture README.
   Run `cargo test --manifest-path native/Cargo.toml --test workflow` with
   `CARGO_BUILD_JOBS=1` and a worktree-local `CARGO_TARGET_DIR`. This checks
   the score, expectations and deterministic media, not application behavior.
2. **Import and sound setup (#99/#108).** Through public native workflows,
   import the custom WAV into a project pad, turn choke off, create
   Rat Sketch → Tape Echo and capture resolved parameters. Resolve the D18
   gate and load that instrument. Capture copied media and retained renders;
   clearing an evictable cache must not remove the only copy of a sound.
3. **Capture two passes (#105/#106).** At 120 BPM, two bars, quantization,
   metronome and count-in off, capture the four base cues then the two
   second-bar overdub cues. Use the public deterministic input seam when
   available. Inspect six identities, three pad velocities, three note
   pitches/velocities, original starts, held durations and FIFO note-offs.
   Capture intermediate base count 4 and overdub count 2 as well as total 6.
   Confirm simultaneous same-pad voices survive and both pitch-60 notes end.
4. **Reversible edits (#107).** Execute each numbered expectation in the
   fixture reference. Inspect after quantize-on, move, delete, undo-delete,
   undo-move and quantize-off. At every step compare immutable originals,
   effective positions, durations, velocities and identities, not just count.
   Shorten/restore the loop to prove second-bar events were hidden, not lost.
5. **Persist and relocate (#100).** Save the restored two-take state, quit
   the process, reopen and compare all six events. Separately save/reopen
   an edited/quantized state and compare both original and effective data.
   Copy the entire project to a new directory outside the checkout, then
   make only the run's original synthetic sources/project unavailable.
   Reopen the copy and check media availability and playback. Never move or
   delete unrelated files. D15 does not require cross-quit undo history.
   Run owner-provided interrupted-save/recovery cases and verify the last
   complete saved project and explicit recovery choice survive.
6. **Neutral export (#109).** Export this captured project, not the fixture
   JSON. Inspect three individually identifiable pad hits, three editable
   notes, original/effective timing correspondence, velocities, durations,
   manifest identity links, and retained audio for unreproducible sound.
   Record exact converted positions and justified sample/tick tolerances.
   Per-hit tail lengths must come from the real render, not the dry span.
   A stem alone cannot replace per-hit clips and notes.
7. **Target handoff (#110, D19).** Re-pin and re-probe before import (see
   below). Import #109's output via #110's public adapter. Compare all six
   events, including overlapping full-length hits and same-pitch notes.
   Move pad-a to **250 ms** and delete pad-b (two pad clips remain, plus
   three notes). Edit note-c to pitch **69**, start **2750 ms**, duration
   **750 ms**, velocity **90**. Save, actually quit, reopen, relocate the
   whole session and make its original run-specific source copy unavailable.
   Reinspect those edits and all unchanged events/media. Produce a reference
   bounce where needed and retain its hash and measured differences.
   Do not claim faithful stacking from imported MIDI data alone.
8. **Fallback matrix.** On disposable copies, separately remove one imported
   source, one retained render, the selected instrument asset, its backend,
   and any required plugin (only if the resolved workflow uses one).
   Record explicit diagnosis, preserved editable data, retained-audio
   availability, recovery/relink behavior, and no corrupt save/crash/hang.
   If no plugin is used, record that fact with the selected configuration,
   not a fabricated plugin pass. Missing backend/media gates remain required.
9. **Software and physical evidence are separate.** Repeat the interactive
   workflow from the computer keyboard and from a named macOS IAC virtual
   port. Keyboard hits currently have full velocity; record that constraint,
   not invented 48/112 captures. Use public deterministic/IAC injection for
   exact velocities/times and keep human timing observations separate.
   Record human input deltas rather than changing the reference score.
   D2 provides no controller/interface/synth. Real-controller evidence stays
   **OPEN / needs-hardware**. A maintainer must separately listen for both
   hits, velocity contrast, pedal tail, held-note releases, missing/stuck
   notes and retained sound before/after target edits and reopens.
   The maintainer listening gate also stays **OPEN / needs-hardware** as
   required by #111's review adjustments. No agent claims to hear audio.
10. **Packaged repetition (#103).** Repeat import, capture, edits,
    save/reopen/relocation, export, target edits, fallback checks and listening
    from the exact packaged artifact outside the development checkout.
    Record its checksum and bundled/source availability. A source run does
    not prove package relocation, signing or first-launch behavior.

## D19: bounded prior research, not this workflow

[PR122](https://github.com/drawmeanelephant/brownnote/pull/122), branch
`t3/soundcraft-go-no-go-spike`, head
`f8a7411022cc84d1f4d88f464d2bfdb5d3992673`, found a bounded editable
handoff at SoundCraft **v0.18.0** (upstream commit
`5ff012ab88490cdfc635ee304c1019885e1b880d`).
Use its stable
[reproduction and pre-#110 re-pin procedure](https://github.com/drawmeanelephant/brownnote/blob/f8a7411022cc84d1f4d88f464d2bfdb5d3992673/native/docs/spikes/soundcraft/REPRODUCE.md)
and [limitations](https://github.com/drawmeanelephant/brownnote/blob/f8a7411022cc84d1f4d88f464d2bfdb5d3992673/native/docs/spikes/soundcraft/README.md).
Do not copy its large evidence capture or treat synthetic imports as native
convergence.

That probe required separate tracks for overlapping audio, did not prove
faithful gated same-pitch sampler stacking, and found SMF re-export trims
an overlapping note. Retain per-hit audio where target playback differs.
Re-pin/re-probe the released app, command schema, hashes, overlaps, edits,
save/reopen/relocation and negative controls before #110. If no-go, invoke
D19's #109 neutral-bundle fallback in another open DAW, not a silent switch.

Screenshots and `session.audio_health` alone prove neither signal nor sound.
A no-audio probe, a bounce or `--check-audio` is not a maintainer listening
check or a real-controller run.

## Repository checks and evidence boundaries

Use one Cargo process at a time, `CARGO_BUILD_JOBS=1`, a worktree-local target,
your own `build/brownnote`, and read-only access to the real bench. Set
`BENCH`, `BROWNNOTE_BENCH`, `BROWNNOTE_REQUIRE_BENCH=1`, `BROWNNOTE_BIN`.
Never share a writable target or clean another checkout.

- Direct fixture checks: command in step 1, no ignored placeholders.
- Applicable native gate: `BROWNNOTE_REQUIRE_BENCH=1 make native-test`
  (fmt, clippy, all tests including existing GPU/headless tests).
- Existing C regression gate: `make test` when touching outside `native/`;
  also run at actual #111 convergence per its brief.
- Actual convergence additionally requires
  `cargo run --release --manifest-path native/Cargo.toml -- --check-audio`
  and `cargo test --manifest-path native/Cargo.toml --test headless -- --ignored`,
  plus the future owner-provided workflow/adapter targets. Do not invent
  commands for APIs that do not yet exist.

**First-slice unexecuted gates:** all native input/take/edit/persistence/export
and target-app stages above; D18 instrument selection; missing media/backend/
plugin recovery; packaged-artifact repetition; IAC and interactive keyboard
runs; real-controller/interface/synth checks; maintainer listening and
native-to-target sound comparisons. Existing native tests do not close them.
