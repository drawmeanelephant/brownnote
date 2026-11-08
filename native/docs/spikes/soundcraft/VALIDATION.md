# SoundCraft spike validation

## Completed research checks

- `prepare.py` passes. The latest release, full commit, downloaded checksums,
  extracted executable checksums, versions, and command schema match the pin.
- `test_fixtures.py` passes all four tests. Independent readers accept the
  deterministic fixtures and reject semantic mutations despite updated hashes.
- `check_fixtures.py` passes on the committed fixture directory before import.
- `probe.py` passes against the real GUI. Four owned GUI processes exit zero.
  Two positive session opens report no missing media. The final negative
  control reports missing media and unhealthy audio.
- `check_evidence.py` passes on the published artifacts, including 483 command
  descriptions and the expected choke-parameter rejection.
- The probe uses `mido 1.3.3` and `soundfile 0.13.1`. The full Python dependency
  set is in `evidence/python-requirements.txt`. The input WAV reader is Python's
  `wave`. Target float WAVs use independent libsndfile decoding.

## Repository checks

The base is `e429719df22b9d0eb58d5f952f5626841004aaf7`.
Only research documentation, scripts, evidence, and fixtures change.

`make` builds this worktree's own `build/brownnote`.
`make test` passes 34,528 unit checks and 27 workflow checks.
Two existing C workflow cases skip:

- The kujamba/zcn round trip lacks its required tools.
- The 4 GiB wrap case requires `BROWNNOTE_HEAVY=1` and about 12 GiB of I/O.

The real GuitarPedal bench is the integrator-provided executable at upstream
`1d2d4f02f6e21b79062844c21310f0595885c311`. It is used read-only.
No source or build state in the integrator's directory is modified.

The first `make native-test` invocation was interrupted during the session.
It is not a passing result. The required-bench rerun uses:

```sh
PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export PATH
export CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR="$PWD/native/docs/spikes/soundcraft/.work/cargo-target"
export BENCH="/path/to/integrator-provided/bench"
export BROWNNOTE_BENCH="$BENCH"
export BROWNNOTE_BIN="$PWD/build/brownnote"
export BROWNNOTE_REQUIRE_BENCH=1
make native-test
```

The required-bench rerun exits zero. Formatting and Clippy pass.
All 109 native tests pass, with zero failures and zero ignored tests.
This includes three GPU tests, three real-bench tests, and the callback
allocator test at both sample rates. The sanitized full check output is in
[`evidence/native-test.txt`](evidence/native-test.txt).

## Development failures and research bounds

An early target-media check failed because Python's `wave` reader does not
support SoundCraft's float WAV subtype. The final probe uses libsndfile rather
than interpreting private session data or dropping the media check.
An early SMF export assertion used clip-relative positions. The pinned export
contract writes global positions, so the final independent check includes the
48,000-sample clip offset, equal to 1,920 ticks at this fixture's tempo.
Both corrected checks pass in the final capture.

The unsupported `choke_group` parameter and removed-media probe are deliberate
negative controls. They are not hidden failures.

No real-device output, listening, external hardware, large-session scaling,
Brownnote-to-SoundCraft production export, plugin parity, or native instrument
equivalence was tested. No claim is made that an agent heard sound.
The maintainer's manual steps and D19 recheck are in `REPRODUCE.md`.
No integrator module exports, UI fields, poll calls, or dependencies are needed.
