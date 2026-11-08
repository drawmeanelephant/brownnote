# Runtime paths, first #103 slice

This is a path policy and a consumer test harness, not an installable release.
Packaging, license review, signing, CI artifacts and clean-machine validation
are later work. D1 limits tested behavior to macOS on Apple silicon; D2 limits
audio hardware claims to built-in Mac audio. The synthetic tests below prove
sample signal through the mixer, not audible output or device recovery.

## Bounded resource layout

The flat resource layout is:

```text
layout/
  brownnote-native
  brownnote-showcase                 # optional
  resources/
    recordings/
      fart_dry.wav
      fart_fuzz.wav
      fart_delay.wav
      fart_chorus.wav
      fart_chain.wav
  tools/                            # optional, only for pedals
    brownnote
    bench
```

Resources are relative to the running executable's parent, not the launch
directory or the build machine. This is not yet a macOS `.app` layout.
Missing or invalid individual WAVs disable only their pads. A chosen folder
is authoritative even when incomplete: no other kit supplies missing sounds.

For development only, executables in the exact
`<checkout>/native/target/{debug,release}[/deps]/` layout use
`<checkout>/recordings`, `<checkout>/build/brownnote` and
`<checkout>/vendor/GuitarPedal/Validation/bench/bench`. The resolver requires
`native/Cargo.toml` at that runtime location. It never embeds the source
checkout path or searches arbitrary ancestors. A `resources` or `tools`
entry beside the executable selects the flat layout instead, even if that
entry is incomplete or a dangling symlink.

An arbitrary `CARGO_TARGET_DIR` is not a supported resource-search layout.
Use `--samples`, `--brownnote` and `BROWNNOTE_BENCH` for such builds.
Writable defaults never use any Cargo target directory.

## Override precedence and diagnostics

Each resource resolves independently:

1. Explicit consumer path (the existing `--samples`, `--brownnote`, or
   `--pedal-cache` value).
2. Its environment variable.
3. The bounded executable-relative layout or user location.

| Resource | Environment | Default on the tested Mac |
|---|---|---|
| Samples | `BROWNNOTE_SAMPLES` | `resources/recordings` beside executable |
| C CLI | `BROWNNOTE_BIN` | `tools/brownnote` beside executable |
| Bench | `BROWNNOTE_BENCH` | `tools/bench` beside executable |
| Pedal cache | `BROWNNOTE_CACHE` | `$HOME/Library/Caches/Brownnote/pedals` |
| Recovery | `BROWNNOTE_RECOVERY` | `$HOME/Library/Application Support/Brownnote/recovery` |

Explicit relative paths are anchored to the launch directory. Empty overrides,
missing paths, wrong path types, non-executable tools, unavailable directories
and failed writes produce `PathError` with a resource, selected path and typed
reason. A broken higher-priority override is never replaced by a lower-priority
path. An unavailable `HOME` is an error, not permission to write in the
checkout, working directory or temporary directory. Default user locations
are defined only for macOS; other platforms can supply explicit directories.

Cache/recovery resolution creates the directory and writes/removes a unique
probe to verify access. Call it off the audio thread. Recovery here is only
a writable location: this slice does not save, autosave or recover projects,
and it does not select a project directory. Project media is not cache (D4).

The C CLI always receives an absolute `--bench` argument for catalog and
render calls, preventing its cwd/vendor fallback from choosing other tooling.
An explicit bench passed to `Resolver::pedal_command` beats the environment.
There is no new native `--bench` flag in this slice; use `BROWNNOTE_BENCH`.
Default pedal tooling is validated when requested. Custom `--brownnote`
tools without a configured bench receive the bounded bench candidate and can
report their own protocol errors. Missing optional tooling does not prevent
kit loading or base-pad signal.

The reserved constants `kit::DEFAULT_SAMPLES`, `brownnote::DEFAULT_BIN` and
`paths::DEFAULT_CACHE` are internal default tokens, not filesystem paths.
They preserve the existing PathBuf caller inputs until shared option wiring
can distinguish absence explicitly. Do not supply these strings as user paths.
Empty user paths remain errors.

## Integrator wiring required

This lane intentionally does not edit integrator-owned files. Before merging:

1. Add `pub mod paths;` to `native/src/lib.rs`. There are no production
   dependency or lockfile changes.
2. In `native/src/main.rs`, replace the `Options.pedal_cache` initializer
   containing `concat!(env!("CARGO_MANIFEST_DIR"), "/target/pedal-cache")`
   with `brownnote_native::paths::DEFAULT_CACHE.into()`. Keep the existing
   explicit `--pedal-cache` assignment; render resolves it ahead of the
   environment. Samples/brownnote initializers already consume the re-exported
   default tokens and need no new flags.
3. In `native/src/ui/pedals/mod.rs`, change `default_cache()` to return
   `brownnote_native::paths::DEFAULT_CACHE.into()` and update its comment.
   Its existing `default_bin()` already uses the re-exported token.
4. No UI `State`, poll, view, main error-path, or recovery/store wiring is
   needed. The future store lane can call
   `Resolver::current()?.resolve(Resource::Recovery, override_path)`.
5. Run the actual native artifact and full native tests after that wiring.
   The first two callers already route Kit::load, catalog and render requests
   through the resolver, but an unregistered `paths` module blocks the main
   Cargo package until step 1. Existing main/UI cache defaults remain checkout
   paths until steps 2–3. Do not call either integration gate green beforehand.

## Reproduce the lane evidence

The temporary `tests/paths-harness` Cargo package path-includes the production
`engine`, `kit`, `paths` and `pedals` modules, without copying implementations
or modifying shared registration. Its two dependencies match the main
package's pinned `hound` and `rtrb`. It also runs the existing real-pedal
integration tests, now loading their source through the real kit default.
It does not build Rune, the main options parser, UI, GPU or device output.
After integration, `tests/paths.rs` also runs as a normal integration test;
the bootstrap can then be deleted.

From the repository root, with a real read-only bench available:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR="$PWD/native/target"
export BENCH=/absolute/path/to/GuitarPedal/Validation/bench/bench
export BROWNNOTE_BENCH="$BENCH"
export BROWNNOTE_BIN="$PWD/build/brownnote"
export BROWNNOTE_REQUIRE_BENCH=1
make
cargo test --manifest-path native/tests/paths-harness/Cargo.toml --test paths
cargo test --manifest-path native/tests/paths-harness/Cargo.toml --test pedals_live
cargo test --manifest-path native/tests/paths-harness/Cargo.toml --lib
cargo clippy --manifest-path native/tests/paths-harness/Cargo.toml --all-targets -- -D warnings
make native-test
```

Run Cargo commands one at a time. `native/target` must be isolated to this
checkout. For the path tests, subprocesses clear resource overrides and supply
a synthetic user home. They copy the running test executable and owned
synthetic WAV/tool resources to a second location, delete the original, then
repeat the copy/delete/launch cycle at a third location from an unrelated cwd.
They exercise the current default-token consumer inputs, catalog, render,
cache hits, recovery writes and actual base-pad mixer signal with missing
optional tools. The fake pedal tool checks the pinned bench argument; it
is not evidence of a packaged GuitarPedal render.

The existing `pedals_live` tests separately require the real CLI/bench and
cover the catalog, every routable effect, system-block refusal, audible-data
differences, echo tail and render-cache hits. These are not a relocated
packaged-tool test. Main binary CLI parsing, the actual packaged artifact,
real audio/listening, clean install, portable projects, failed saves,
device recovery and distribution/license gates remain unproven here.
