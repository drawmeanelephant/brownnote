# Brownnote native pads

A standalone, 16-pad sample instrument built with Rust and Rune.
Play the included recordings, shape the next hit, and stop everything with Esc.

![Brownnote native pad bank](pads.png)

The hardware-style panel has illuminated, color-coded pads, a selected-sample
display, and a segmented output meter. Waveforms are peak-preserving,
normalized previews of the actual WAVs, not live scopes or spectra. They are
computed once when the kit loads, off the audio thread. Pad lights follow
actual voice counts, with a short input flash even on muted hits. The output
meter shows the engine's peak in dBFS with a decaying release; it is not a
calibrated loudness meter. The UI stops requesting animation frames at rest.

## Start playing

From the repository root:

```sh
make native
native/target/release/brownnote-native
# Equivalent:
cargo run --release --manifest-path native/Cargo.toml
```

On this Mac, if `cargo` is not found:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
```

The app loads the five WAVs in the repository's `recordings/` directory.
It does not need Python, GuitarPedal, kujamba, or a server. The default path
is fixed at build time relative to `CARGO_MANIFEST_DIR`; when moving the
binary elsewhere, supply your sample directory:

```sh
native/target/release/brownnote-native --samples /path/to/recordings
native/target/release/brownnote-native --no-audio
```

`--no-audio` runs the same mixer silently on the app clock. Missing or invalid
samples disable only the affected pads. A missing folder leaves the window
usable with a banner explaining `--samples`. Audio failures fall back to the
silent mixer and expose **RETRY AUDIO**.

### Controls

- Play `1234` / `QWER` / `ASDF` / `ZXCV`, matching the four grid rows.
  Keyboard hits use full velocity; held keys do not repeat.
- Click higher on a pad for a louder hit (top 100%, bottom 40%).
  Sound starts on pointer down, not release.
- A hit selects that pad. **GAIN** (0–150%), **PAN** and **PITCH**
  (−12 to +12 semitones) affect its **next** hit, not voices already playing.
- **MASTER** (0–100%, initially 80%) changes all output with a one-block ramp.
  Focus a slider and use arrows or Home/End; Escape cancels a slider drag.
- **CHOKE**, on initially, fades existing voices of the same sample file over
  5 ms on a new hit, even across different pads. Turn it off to stack hits.
- **PANIC** or **Esc** fades all voices over 5 ms to exact silence.
  Cmd/Ctrl/Alt shortcuts pass through; Cmd-Q still quits.
- The voice badge is the engine's actual voice count. The quick flash is
  immediate input feedback. PEAK decays after a hit.

Pitch changes playback speed and duration using linear interpolation; this
is not time stretching or a bandlimited pitch shifter. Pan is constant power.
The engine hard-clamps output to ±1. There are at most 32 voices; when full,
the oldest stops immediately, which can click. Settings reset on launch.

| Keys | Pads |
|---|---|
| 1 · 2 · 3 · 4 | DRY · DRY −5 · SEWER (−12) · SQUEAK (+7) |
| Q · W · E · R | FUZZ · FUZZ +7 · FUZZ OCT (+12) · SLUDGE (−7) |
| A · S · D · F | DELAY · DELAY −5 · CHORUS · CHORUS +5 |
| Z · X · C · V | CHAIN · THUD (−12) · CHORUS −7 · DELAY OCT (+12) |

Custom folders must contain `fart_dry.wav`, `fart_fuzz.wav`, `fart_delay.wav`,
`fart_chorus.wav`, and `fart_chain.wav`. `demo_brownnote.wav` is not a pad.
WAVs may be 16-bit PCM or 32-bit float, mono or stereo (averaged to mono),
8–192 kHz, from one frame through 30 seconds. Invalid or non-finite samples
produce file-specific errors. All files load before the audio stream starts.

## Build and platform

Tested on macOS 27.3, Apple silicon, rustc 1.99.0. Other platforms are untested.
Rust edition 2024, minimum Rust 1.99. No project toolchain override is installed.

Exact direct dependencies:

- Rune git revision `cbaf637dfe5fcf3ec8b3f1c3ad27246b69742fbf`,
  default features, from <https://github.com/anomalyco/rune>.
- rune-fx from the same revision, and wgpu 30 for the compositor hook's types
  (features come from Rune's own wgpu). Only the look showcase draws with them.
- cpal `=0.18.2`, rtrb `=0.4.1`, hound `=3.5.1`.
- `Cargo.lock` fixes transitive versions; serde_json is test-only.

Linux build packages (unverified): `libxkbcommon-dev` and `libwayland-dev`
for Rune, `libasound2-dev` for cpal. Headless rendering still requires a GPU
adapter (Metal on the tested Mac).

The default device/config is used. Supported output formats: F32, I16, U16,
I32. Stereo goes to channels 0 and 1, additional channels get silence, and a
mono device receives `(L + R) / 2`. No device picker or latency claim.

## Verify

From the repository root:

```sh
make native-test
make test  # Existing C workflow, independent of the native app.
```

For individual Rust checks and binary probes:

```sh
cd native
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Audible, no window: pad 0 for two seconds, exit 0 only for frames and signal.
cargo run --release -- --check-audio

# Silent, actual binary + virtual mixer + Rune GPU renderer:
cargo run --release -- --script scripts/smoke.json --at 0.6 --tree target/playing.json
cargo run --release -- --snap target/default.png
cargo run --release -- --scene playing --at 0.08 --snap target/playing.png
cargo run --release -- --scene missing-samples --snap target/missing.png
```

Engine tests cover both 44.1 and 48 kHz. A counting allocator checks the
callback's command-drain/render path for zero allocations and frees.
Rune Runtime tests exercise pointer, keys, sliders and missing assets.
They also check selection, meter decay, muted-hit feedback, and the minimum
820×600 layout. Waveform tests cover silence, transients and extreme samples.
The headless integration test launches the real binary, checks voice badges
before and after panic, and checks missing-folder errors. It is not skipped
automatically if a GPU is absent. Screenshots alone do not prove audio playback.

## Look showcase

`brownnote-showcase` draws the same instrument in three looks (Bunker, Space
Program, Afterhours) with an effect lab, tiers and measurement switches:
`cargo run --release --bin brownnote-showcase`. See [SHOWCASE.md](SHOWCASE.md)
for commands, the recommendation, the theme and control API, and measurements.

## Relationship to the studio

The web studio (`make studio`) stays the full workflow: sequencer, desk,
library, loop station, rendering and export. This native app uses the same
recordings and key layout, but has no shared state and no Python server.
The existing C CLI, Python studio and their tests are unchanged.

## Boundaries and next steps

The audio callback drains a bounded 256-command wait-free queue, mixes a
preallocated 32-voice pool, converts directly into the device buffer and
updates atomic meters. It does no allocation, locks, I/O, logging, or Arc
clone/drop. UI queue overflow drops the command and reports a count.
The virtual driver runs the same mixer, with at most 0.25 seconds rendered
per poll; it is a silent fallback, not a wall-clock playback guarantee.
Recovering or retrying a failed device starts a fresh voice pool.

**Effects rendering is deferred.** A future `rune::Latest` job or worker thread
could run `build/brownnote --effect NAME < dry.wav > build/native/<pad>-<effect>.wav`
or `scripts/presets.sh fx`, then load the rendered WAV off the audio thread.
A new swap command would carry its immutable Arc to the mixer, and a return
queue would send the old Arc back to the UI so the callback never frees it.
That swap/return protocol is not implemented here. Existing effect tools are
not reimplemented, and there are no placeholder effect controls.

Also deferred: sequencer, desk/mixer, library, loop station, recording,
NINJAM, MIDI, patch persistence, sample browsing, hot reload, a device picker,
web/WASM, mobile, native CI (needs a GPU runner), and a docs-site page.
