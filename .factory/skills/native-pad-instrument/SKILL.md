---
name: native-pad-instrument
description: Work on the Rust/Rune native pad instrument under native/, including the audio engine, pedal rendering, MIDI input, and the headless probes. Use for cargo, clippy, engine, or UI changes.
---

# Native pad instrument (brownnote-native)

Use this skill for anything under `native/`.

## Toolchain

Rust 1.99, edition 2024. On this Mac:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo --version   # 1.99.x
```

The plugin host is a separate workspace (`native/plugin-host`) because it
allows unsafe FFI; build it with `make native-plugin-host`, not from the app
workspace.

## Gates

```sh
make native-test              # fmt, clippy -D warnings, full test lane
make native-render-timeout    # render deadline/signal stress in fresh processes
make native-clap-live-test    # required helper + mounted live lane
make native-ladspa-workflow-test
```

`cargo test` alone skips `#[ignore = "needs a GPU adapter"]` tests; the local
`make native-test` lane includes them. A test that needs a device must be
excluded by name, never reported as passed.

## Non-negotiables

- The audio callback does no allocation, locking, I/O, logging, or `Arc`
  clone/drop. `engine_callback_with_the_live_instrument_allocates_and_frees_nothing`
  guards this.
- `unsafe` is denied in the app crate; `unwrap`/`expect` are denied outside
  tests (`native/clippy.toml`).
- A render completes off the UI thread and is swapped in as a queued `Load`.
  Never run effects inside the callback.
- Headless probes are the QA path on machines without audio hardware:
  `--check-audio`, `--list-pedals`, `--snap`, `--tree`.

## Useful probes

```sh
cargo run --release -- --no-audio
cargo run --release -- --list-pedals
cargo run --release -- --check-audio
cargo run --release -- --scene pedals --at 0.6 --tree target/pedals.json
```

Runtime paths are documented in `native/docs/paths.md`; the import and
project contracts are in `native/docs/import-contract.md` and
`native/docs/project-contract.md`.

## When a test needs hardware

Name the exact test and the reason in the PR. Add an opt-in Make target
instead of weakening the required lane.
