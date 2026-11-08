# Import/runtime integration validation

Refs #99. #99 remains open.

## Artifact and scope

The integration preserves prepared base
`5e2de37d0f53d4da9456b4ab47afb4678c8348ba` and merges published service head
`ff654c24f4b709ec57681746c13134d5a5e8644b`, without rewriting either history.
It mounts the service and one registry in the actual pad application. It adds
no import UI, project schema fields or second project writer.

See [Import contract](import-contract.md) for session ownership, retained-media
publication, generations, retirement and the store-adoption boundary.

## Final local gates, 8 November 2026

All commands ran in this checkout, with its own `native/target` and
`build/brownnote`. Cargo build concurrency was one; test concurrency remained
at its normal setting. The read-only external GuitarPedal bench revision was
`1d2d4f02f6e21b79062844c21310f0595885c311`.

| Gate | Terminal status | Result |
|---|---:|---|
| `make` | 0 | Built the local C CLI |
| `cargo test --manifest-path native/Cargo.toml --test import_assets` | 0 | 15 real-library tests |
| `cargo test --manifest-path native/Cargo.toml kit::` | 0 | 4 kit tests |
| `cargo test --manifest-path native/Cargo.toml engine::` | 0 | 33 engine tests |
| `cargo test --manifest-path native/Cargo.toml --test rt_alloc` | 0 | 2 zero-allocation/free callback tests |
| `cargo test --manifest-path native/Cargo.toml --bin brownnote-native` | 0 | 60 mounted application tests, including 10 new import/runtime tests |
| `make native-test`, after final code change | 0 | Format, warning-free all-target Clippy, 236 native tests; zero ignored or failed |
| `make test`, after final code change | 0 | 34,528 WAV checks and 27 workflow checks; 2 optional skips |

The strict native run included GPU/headless binary tests, the original
relocated-application test, all three real-bench tests with
`BROWNNOTE_REQUIRE_BENCH=1`, the comprehensive synced-filesystem fault matrix,
and RT checks. No per-render deadline, assertion, tolerance or test concurrency
was relaxed.

The C skips were the unavailable kujamba/zcn round trip and the opt-in 4 GiB
wrap test. Neither was counted as executed.

### Earlier failures and corrections

- One compile call was interrupted and had no passing status. Subsequent
  compilation, Clippy and complete native gates finished successfully.
- Initial mounted-test failures exposed fixture assumptions: decoded-only
  sample mutation no longer supplied encoded ownership, key events needed the
  normal event/poll frames, and the full-slot fixture changed pad selection.
  Fixtures now provide valid encoded WAVs and preserve original PCM assertions.
- The first strict attempt missed the pre-existing process-timeout test's
  SIGTERM marker. The exact test passed in isolation and in later complete
  runs, with its original deadline and assertions unchanged.
- The next strict run caught a genuine relocated-kit choke regression.
  Bootstrap now preserves intentional per-kit-file choke groups independently
  of content-addressed slot sharing. The original test passed unchanged.
- Final inspection found a deferred pedal admission that did not report its
  visible state change. The mounted queue-refusal test now also verifies the
  real poll's redraw result and visible READY text. The final complete native
  and C gates above ran after this correction.

## Reproduce

From the repository root, point `BENCH` at the existing bench, not a writable
shared build target:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export CARGO_BUILD_JOBS=1
export BENCH=/absolute/path/to/GuitarPedal/Validation/bench/bench
export BROWNNOTE_BENCH="$BENCH"
export BROWNNOTE_BIN="$PWD/build/brownnote"
export BROWNNOTE_REQUIRE_BENCH=1
make
cargo test --manifest-path native/Cargo.toml --test import_assets
cargo test --manifest-path native/Cargo.toml kit::
cargo test --manifest-path native/Cargo.toml engine::
cargo test --manifest-path native/Cargo.toml --test rt_alloc
cargo test --manifest-path native/Cargo.toml --bin brownnote-native
make native-test
make test
```

## Remaining gates

- Keep PR129 draft against `main` while the parent repairs ancestor delivery
  provenance. Parent owns exact-head GitHub checks, independent review,
  readiness and merge. These local results are not a new-head CI claim.
- A separate slice adds keyboard-accessible typed-path/drag-and-drop import.
- The real application's save/reopen/relocation UI, store-owner coordination,
  history and recovery remain later integration. Tests exercise actual store
  adoption, not an application save interaction.
- No listening, physical-controller or external-device claim is made.
