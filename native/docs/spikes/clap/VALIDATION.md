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
build of the probe, renders 16 scenarios plus two state sequences and six
fresh-process state loads, repeats three renders in a fresh process, and
runs `verify.py`.

## Result

- **42 of 42 checks pass**, including 7 `characterize:` checks that pin
  known plugin behavior (two instrument defects, tail and silence limits,
  state-to-audio mismatch).
- Fresh-process repeats of the instrument lifecycle, the 4 s effect burst
  and the 30 s tail render are byte-identical.
- Executable SHA-256: synth `33cd3729…973b` (built at the pinned revision
  with this toolchain), effect `92152f95…ad4d` (matches the pin).
- Paced-thread timing for this run: instrument p99 22 µs per 128-frame
  block, effect p99 101 µs per 256-frame block, no late blocks, worst
  lateness 1.9 ms. Load average at the end was 6.6. An earlier run on the
  same machine under heavier load saw up to 95 ms lateness with the same
  PCM output; lateness is OS scheduling, not a plugin or device measure.

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

## Not run or not covered

- `make native-test` and `make test`: this PR changes nothing the native
  crate or the C tool builds. See the PR description for the run made on
  the final branch state.
- No listening check, audio device, realtime-priority thread or hardware.
- Failure, hang and isolation evidence is the second PR.
