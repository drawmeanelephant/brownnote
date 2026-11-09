# Validation record

Host: arm64 macOS on Apple silicon, Rust 1.99.1. Builds used the shared
whole-check lock `/Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock`,
the shared target `/Users/tbuddy/dev/brownnote/shared-target`, and two Cargo
jobs. Commands ran from this worktree's repository root.

## Command

```sh
PATH=/opt/homebrew/opt/rustup/bin:$PATH python3 native/docs/spikes/clap/check.py \
  --lock-file /Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock \
  --target-dir /Users/tbuddy/dev/brownnote/shared-target --save-evidence
```

It verifies both plugin pins (`prepare.py`), runs `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings` and a locked release
build of the probe and the fault fixture, renders 16 scenarios plus two
state sequences and six fresh-process state loads, runs six load failures,
four in-process fault runs (each in its own process) and seven helper-process
runs, repeats three renders in a fresh process, and runs `verify.py`.

## Result

- **59 of 59 checks pass**, including 7 `characterize:` checks that pin
  known plugin behavior (two instrument defects, tail and silence limits,
  state-to-audio mismatch) and 17 failure-boundary checks.
- Load failures: six typed errors, exit 3, no output, state file unchanged.
- In-process: crash kills the host (SIGSEGV, 8 blocks); hang detected in
  201 ms with the thread and plugin lost; 43904 non-finite samples pass
  through.
- Helper process: three crashes detected within 0.1 ms, two restarts with
  state restored, then disabled, 70 dry blocks; hangs cut off at
  210–211 ms; non-finite output disabled at block 8; transport PCM
  byte-identical to in-process for the real effect.
- Fresh-process repeats of the instrument lifecycle, the 4 s effect burst
  and the 30 s tail render are byte-identical.
- Executable SHA-256: synth `33cd3729…973b` (built at the pinned revision
  with this toolchain), effect `92152f95…ad4d` (matches the pin).
- Paced-thread timing for this run: instrument p99 23 µs per 128-frame
  block, effect p99 107 µs per 256-frame block, no late blocks, worst
  lateness 2.4 ms. Load average at the end was 2.2. An earlier run on the
  same machine under heavier load saw up to 95 ms lateness with the same
  PCM output; lateness is OS scheduling, not a plugin or device measure.
- Isolation cost for this run: effect per 256-frame block 5.0 µs (p50)
  in-process, 14.5 µs (p50) and 22.0 µs (p99) through the helper; helper
  start 2–9 ms, restart 2–6 ms.

## Corrections during development

- The first instrument fixtures used a 0 ms attack. The plugin then skips
  its release and cuts voices at note-off, so a release-time state
  mutation rendered identical audio. The fixtures now use a 1 ms attack;
  the 0 ms case is kept as a characterization.
- The first state mutation (Release) exposed the stuck same-key voice
  defect. The state test now mutates Gain; the defect has its own fixture.
- The first D14 contrast threshold (10x energy at 220 Hz) was too strict
  for a 960-sample Goertzel window with an E4 saw present: measured 9.1x.
  It is now 5x, with the values recorded in the measurements.
- The first helper transport dropped bytes that a pipe read delivered
  past the end of one reply, and failed every helper run. The parent now
  keeps those bytes for the next reply.
- The first "not a CLAP" case used `/usr/lib/libz.1.dylib`, which exists
  only in the dyld shared cache on this macOS, so it tested a missing
  bundle instead. `check.py` now compiles a small dylib without
  `clap_entry`.
- `cargo fmt --manifest-path` cannot find targets in the two-member
  virtual workspace from outside it; `check.py` runs `cargo fmt --all`
  in the workspace directory.

## Not run or not covered

- `make native-test` and `make test`: these PRs change nothing the native
  crate or the C tool builds. See the PR description for the run made on
  the final branch state.
- No listening check, audio device, realtime-priority thread or hardware.
- Faults come from a purpose-built fixture, not from real plugin bugs.
- Isolation costs are pipe round trips between ordinary-priority
  processes, not a realtime transport.
- No project save, reopen or recall with a plugin missing or disabled
  (needs #100).
