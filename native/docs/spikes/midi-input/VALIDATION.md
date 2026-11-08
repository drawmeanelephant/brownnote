# MIDI slice validation

## Integrated checks, 8 November 2026

The library exports `midi`, and the fixture target imports that export.
The checker uses the normal worktree-local Cargo target by default.

- `python3 native/tests/check_midi.py` passes all nine fixtures with its
  default arguments. This is no longer path-bootstrap evidence.
- `make native-test` passes all 193 tests, including GPU tests, the required
  real bench, storage faults, and allocation/free checks.
- `make test` passes all 34,528 WAV checks and 27 executed workflows. The
  existing kujamba round trip and opt-in 4 GiB checks skip.

The full run uses default test parallelism and the original render timeouts.
It does not skip either UI test that failed in the original lane run.
The queue-admission test now uses its existing worker gate before the queue
fills, as merged in #130. The earlier failures below remain part of the
record, not passing checks.

This integration includes #125's verified path changes and the committed
project store. Production MIDI input and hardware checks remain open.

## Original lane checks

Base: `origin/main` at `2421a95314812ebaa1d5afbd5a828157860f9c72`.

### Evidence

- The owned CoreMIDI probe passed on macOS arm64. It received all four expected
  messages and observed input close, source removal, and same-name recreation.
- Existing input ports: zero. Existing IAC-name matches: zero.
- [Saved callbacks](probe-evidence.json) are real software MIDI evidence.
- `python3 native/tests/check_midi.py` fails on missing `pub mod midi;`.
  This is an **export blocker**, not a green feature check.

- `make test` passed: 34,528 WAV unit checks and 27 regression checks.
  The two skips are the kujamba/zcn round trip and the opt-in 4 GiB I/O case.
  The real GuitarPedal bench was read-only at revision
  `1d2d4f02f6e21b79062844c21310f0595885c311`.
- The first `make test` invocation supplied only `BENCH`, omitting the CLI's
  `BROWNNOTE_BENCH`. It failed six bench-discovery checks. The corrected run
  set both variables and passed. No test or production code changed for it.
- Both Python reproduction scripts passed syntax checks.
- The final standalone probe passed live checks, formatting, and Clippy with
  `-D warnings`. Its first Clippy run rejected redundant `drop(close())`
  calls. Removing those redundant drops fixed the lint, and the final live
  run still observed close and cleanup. The saved evidence is that final run.

- `python3 native/tests/check_midi.py --bootstrap` passed all nine fixtures
  against the actual native manifest. The exhaustive note-on test covers all
  wire channels, pitches, and velocities. This run uses path inclusion for
  MIDI only, with the actual exported project and performance types.

- The first `make native-test` run passed formatting and Clippy, then failed
  `ui_pedals_tests::a_load_the_queue_refused_is_landed_once_it_drains` at
  `native/src/ui_pedals_tests.rs:561` (slot 5, expected 1).
  The same test passed in isolation without any code changes.
  Its unchanged setup runs UI frames after starting an immediate fake render,
  before filling the queue. A render can land during those frames.
  The existing `Recorder::hold()` provides a way to fence that setup.
  No UI, engine, or test timing was changed in this lane.

- The repeated default `make native-test` again passed formatting, Clippy,
  and all 80 library tests. The main-binary target failed the same queue-full
  test, plus
  `ui_pedals_tests::a_voice_keeps_its_sample_across_a_swap_and_its_slot_retires_after_it_ends`
  at line 260 (slot 7, expected 6). These production and UI-test files match
  the base exactly. The default native gate remains **failed**, with no retry
  under reduced test parallelism or skipped assertions counted as a pass.

- Separate integration runs reached the tests that the failed main target
  prevented the default gate from running. All three GPU headless tests,
  nine MIDI bootstrap fixtures, six output-selection tests, and two output
  timing tests passed.
- The mandatory real-bench target passed catalog enumeration and all-effect
  routing/refusal checks, but
  `live_renders_change_the_sound_ring_out_and_then_hit_the_cache` failed at
  `native/tests/pedals_live.rs:172` with the existing ten-second render timeout.
  Its isolated rerun failed the same way, without a timeout increase.
  Both the test and `native/src/pedals/render.rs` match the base exactly.

- The remaining integration targets passed separately: 12 performance,
  nine project, two real-time allocator, and four workflow fixtures.
  No Cargo invocation overlapped another in this checkout.

Those original failures blocked the draft. The integrated checks above
resolve the export and local validation blockers. GitHub checks must also
pass on the published integration head before merge.

## Unproven gates

- Production selected input, callback handoff, loss reporting, source epochs,
  reconnect, queue-full recovery, and continued input.
- Held controller's sustain, all-notes-off, panic, and disconnect releases.
  The first PR checks mapping intents, not their runtime execution.
- Project mapping persistence, instrument integration, and recorded durations.
- IAC production path. No IAC bus existed and no global settings were changed.
- Named physical controller. None is available (D2).
- Hardware timing, listening, future MIDI output, and external gear.

No screenshots, mocked queues, or virtual MIDI result closes these gates.
#105 remains open.
