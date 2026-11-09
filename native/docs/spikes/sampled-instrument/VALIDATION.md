# Validation record

The standalone checks use the exact crate versions in this directory's
Cargo.lock. The host is arm64 macOS with Rust 1.99.1. All builds and tests
use `/Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock`,
`/Users/tbuddy/dev/brownnote/t3-51ff88eb/native/target`, and two Cargo jobs.
The commands below run from `/Users/tbuddy/dev/brownnote/t3-b2359cac`.

## Commands

The full invocation is in [Reproduce](README.md#reproduce). These are the
check commands, run with the documented PATH and real-bench environment:

```sh
python3 native/docs/spikes/sampled-instrument/check.py \
  --lock-file /Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock \
  --target-dir /Users/tbuddy/dev/brownnote/t3-51ff88eb/native/target \
  --save-evidence --native-gates
```

After moving panic and reset to the worst cached-block position, the
fixture and evidence update uses:

```sh
python3 native/docs/spikes/sampled-instrument/check.py \
  --lock-file /Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock \
  --target-dir /Users/tbuddy/dev/brownnote/t3-51ff88eb/native/target \
  --regenerate-fixtures --save-evidence
```

The runner prints every Cargo invocation and the fresh output directory.
`cargo run --locked` exercises the actual candidate library, not a fake
renderer. `verify.py` checks the resulting WAVs independently.

## Results

- The standalone run covers 16 named scenarios and 62 behavioral checks,
  including an explicit characterization of the D14 failure.
- Every scenario renders 72,000 stereo PCM16 frames at 48 kHz.
- Two fresh processes produce byte-identical WAVs for all scenarios.
- Missing and corrupt SF2 inputs fail before output creation.
- Saved configuration reconstructs exact PCM. Reset and configuration
  reapplication reproduce exact PCM from the reset frame onward.
- All scenarios have exact pre-note and final silence. The independent
  spectra detect eight fundamentals after 16 single-region notes with a
  configured eight-voice limit.
- `make native-test` passes formatting, Clippy with warnings denied, and
  **256 tests**, with zero failed, ignored or filtered tests. It includes
  the real-bench `pedals_live` tests. The native crate recompiles from this
  worktree. Checkout-local project-store, scheduler and realtime-allocation
  test names appear in the run, including
  `historical_v1_opens_exact_bytes_and_unsupported_versions_never_migrate_or_overwrite`,
  `performance_fifo_note_off_targets_overlaps_at_the_exact_sample`, and
  `scheduled_callback_saturation_releases_and_retirement_allocate_and_free_nothing`.

The exact decoded measurements, WAV hashes and event dispatch are in
[`evidence/`](evidence/). The PR and implementation handoff identify the
tested commit. No upstream performance benchmark, callback latency,
CPU budget or broad SF2 compatibility result is claimed.

## Initial failure and correction

The first verifier compared the entire reset scenario against a fresh
scenario. That incorrectly included the deliberate pre-reset note and
changed controls. The comparison now starts at the reset event, frame
9,600, and compares both channels through the end. It retains an exact
full-file comparison for reconstruction from saved configuration.
The corrected check passes without changing the backend or loosening
the PCM equality requirement.

## Skipped and unresolved gates

`make test` was not run because all changes are under `native/`. No native
test in `make native-test` was skipped. There is no separate GPU gate for
this offline probe, and it makes no new GPU claim.

No useful real reference SF2 was supplied or rendered. The proposed
user-supplied TimGM6mb piano remains unpinned and untested. No listening
check or physical MIDI controller test was possible. No native instrument
controller, recording, project save/reopen/relocation, retained fallback,
or editable export support was added or tested. Existing native tests
remain baseline coverage, not sampled-instrument application coverage.

The D14 failure and silent missing-preset fallback remain findings in the
third-party backend, not hidden passing application gates. #108 remains
open. The [integration handoff](README.md#integration-handoff) lists the
requirements for later work. This PR requests no shared-file wiring.
