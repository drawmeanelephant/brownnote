# LADSPA target and offline render spike (#97)

## Recommendation

**Proceed with a bounded first scope.** The first LADSPA target is the
**CAPS `Plate` reverb** (C\* Audio Plugin Suite 0.9.26, LADSPA ID 1779) on
**macOS arm64**. Use it for offline pad renders only. The user builds or
installs the plugin. Brownnote does not bundle it.

This spike proves that Plate builds for arm64 from pinned upstream source
with a two-file compiler patch. A standalone host loads it and renders a
real Brownnote pad sample offline. The render is reproducible and its output
is checked independently. LADSPA is not deferred. #114 can start against the
scope below. #97 closed on this evidence; the maintainer listening pass is an
optional follow-up and is not a close gate (see [Open gates](#open-gates)).

This is a standalone probe. It adds no production module, dependency, UI
or project format. It does not change `native/Cargo.toml` or any
integrator-owned file.

## Why this plugin

| Question | Answer |
| --- | --- |
| Musical use | A plate reverb on drum and pad hits: a short, bright and dense stereo tail. The current GuitarPedal pipeline has a mono `Reverb` (Room, Damp) and renders mono PCM16 with a fixed 2 s tail. Plate adds a decorrelated **stereo** output (left/right tail correlation −0.08 in the reference) and a tail that runs past 2 s at its defaults. Whether it sounds better is the maintainer's call (optional listening follow-up below). |
| Controls | Four 0–1 controls, all with LADSPA bounds and defaults: `bandwidth` (mid), `tail` (high), `damping` (mid), `blend` (low). They map directly to pad pedal knobs. |
| Layout | 1 audio input (mono) and 2 audio outputs (stereo). No `run_adding`, no `deactivate`. Declares `HARD_RT_CAPABLE`, not `INPLACE_BROKEN`, not `REALTIME`. |
| Source | [quitte.de/dsp/caps.html](https://quitte.de/dsp/caps.html), version 0.9.26 (October 2018, latest release). Tarball SHA256 `e7496c5b…f98c9`, byte-identical to Debian's `caps_0.9.26.orig.tar.bz2`. Pinned in [`pin.json`](pin.json). |
| License | **GPL-3.0-or-later** (`COPYING`, source headers, Debian `copyright`). |
| macOS arm64 | **No upstream or Homebrew binary.** Upstream `configure.py` sets `-arch i386 -arch x86_64` on macOS. Debian ships Linux arm64 packages, which do not run on macOS. Built here with Apple clang 21 after [`caps-clang-arm64.patch`](caps-clang-arm64.patch). |
| Other CAPS plugins | The same library holds 27 descriptors (`evidence/caps-descriptors.json`): `Saturate`, `Compress`, `PhaserII`, `ChorusI`, `AutoFilter` and others. Only Plate is evaluated here. |

### The arm64 build failure and patch

Unmodified 0.9.26 fails with Apple clang on arm64:

```
./dsp/v4f.h:158:12: error: no matching function for call to 'v4f_map'
```

Clang rejects `__builtin_sinf` and `__builtin_cosf` as template arguments,
and GCC accepts them. That is why Debian's arm64 build needs no patch. The
patch replaces the seven uses with `sinf` and `cosf`, which compute the same
functions. `build_caps.py --expect-unpatched-failure` reproduces the failure
on every check run before it applies the patch. The x86 FPU truncation code
in `dsp/FPTruncateMode.h` is already a no-op on non-i386 targets.

The build uses upstream's `-O3 -ffast-math` flags with
`make CC=c++ _LDFLAGS=-bundle caps.so`. It produces a `Mach-O 64-bit bundle
arm64`. Two clean builds on this machine produced the same SHA256,
`83b72440…4957`. The build is not proven byte-identical across Xcode
versions.

## Licensing constraints

- Brownnote is MIT. CAPS is GPL-3.0-or-later. Loading a plugin in-process
  with `dlopen` makes host and plugin one running program. The FSF treats
  that as a combined work **when the two are distributed together**. MIT is
  GPL-compatible, so this is not a blocker. Still, any build that ships
  `caps.so` must ship the corresponding source, this patch and the GPL-3
  text, and must be reviewed as a distribution decision.
- **First scope: do not bundle.** Brownnote documents the pinned build or
  install and loads whatever the user has. Nothing in this PR downloads into
  the repository. The tarball and library live in the ignored `.work/`.
- Rendered audio is program output. Brownnote can retain it as project media
  under D4 without bundling the plugin. This is a reading of the licenses,
  not legal advice. The maintainer decides.
- `fixtures/fixture_plugins.c` is original Brownnote code under MIT. It
  repeats the LADSPA 1.1 ABI structs and does not include the
  LGPL-2.1 SDK header.

## CLAP comparison

These are source-level facts only. They were checked on 9 November 2026 and
nothing was built or run. CLAP evidence belongs to #96, and none is
substituted here.

| | CAPS Plate (LADSPA) | Airwindows Consolidated (CLAP) |
| --- | --- | --- |
| Effect | One plate reverb, four controls | Hundreds of effects, including reverbs such as `MatrixVerb`, `Chamber` and `DubPlate` |
| License | GPL-3.0+ | Airwindows DSP and the wrapper source are MIT. The upstream README says the distributed CLAP/VST3/AU build is combined with JUCE and the VST3 SDK, which brings in GPL-3 |
| macOS arm64 | Built from source here (patched) | Upstream nightly builds are universal macOS binaries ([baconpaul/airwin2rack](https://github.com/baconpaul/airwin2rack), main `316a048`) |
| Host work | dlopen, one C entry point, plain structs. No state, tail, latency or GUI extensions. This probe needs no Rust dependency | Needs a CLAP host: plugin factory, params, audio ports and lifecycle. State and tail extensions are available |
| Recall | Library plus `UniqueID`/`Label` plus control values. There is no plugin version field, so recall must also pin the library hash | Plugin ID, version and opaque state |
| Tail | Undeclared. The host must measure it, as here | `clap.tail` can declare it |

LADSPA costs very little host code and has no state blob. Its weaknesses
are that tails are undeclared and versions are unknown. Either format gets
the same bundling question for this effect. Decide format priority once #96
has real CLAP evidence. This spike does not depend on it.

## Proposed first supported scope for #114

1. **Platform:** macOS arm64 only (D1). Linux availability is not shown,
   because native CI is macOS-only.
2. **Discovery:** `LADSPA_PATH`, colon-separated. When it is unset, use
   `~/Library/Audio/Plug-Ins/LADSPA` and then
   `/Library/Audio/Plug-Ins/LADSPA`, the directories macOS hosts such as
   Audacity use. The SDK names no default. Identify a plugin by library
   file name, `UniqueID` and `Label`, and record the library SHA256 in
   the project.
3. **Plugin:** CAPS `Plate`, plus any other descriptor that passes the same
   validation, behind an allow-list that starts with Plate.
4. **Layout:** 1–2 audio inputs and 1–2 audio outputs. Duplicate a mono
   source to two inputs, average a stereo source into one input, and keep
   the plugin's own output channel count. Reject any other layout.
5. **Output:** keep float32 at the plugin's channel count. The existing pedal
   path's mono PCM16 conversion would discard the stereo tail.
6. **Tail:** measured, not fixed. Render until 50 ms in a row stay below
   −90 dBFS, and fail when a cap is exceeded (30 s here). Plate's default
   tail on the pad is **3.39 s**, which exceeds the existing 2 s pedal tail.
7. **Failures:** a render either publishes completely or not at all.
8. **Out of scope:** live or real-time LADSPA, LADSPA instruments, bundling,
   a plugin browser beyond the allow-list, and Linux or Windows.

## What the probe does

`src/` is a dependency-free Rust binary. `src/ladspa.rs` holds the ABI,
loader, validation and hint resolution. `src/wav.rs` holds a WAV reader and
writer. `src/main.rs` holds the command line and render.

- `list <library>` prints every descriptor, its ports and raw hints, and
  the resolved defaults, or the reason the descriptor is invalid.
- `render <library> <label> <input.wav> <out-dir> [--block N]
  [--max-tail S] [--run-adding] [--set NAME=VALUE]` renders one file and
  publishes `render.wav` (float32) and `render.json`. The JSON records the
  library path, `LADSPA_PATH`, the search dirs, the host, the full
  descriptor, each control's value and source, the control outputs, the
  channel map, the block size, the tail measurement and the peaks. The probe
  writes into a hidden staging directory and renames it into place. Every
  failure exits 1 and leaves nothing.

Host rules the probe applies:

- **Validation.** Each port must be exactly one of input or output and
  exactly one of control or audio. Bits outside the four LADSPA port
  defines are masked and recorded, not rejected. CAPS sets a private
  `0x10` "group" bit on `tail` and `blend`. A host that rejects unknown
  bits would refuse CAPS. Required functions are checked. Descriptor and
  port counts are capped.
- **Hints.** `SAMPLE_RATE` scales the bounds by the actual rate.
  LOW/MIDDLE/HIGH use 75/25, 50/50 and 25/75 weights, geometric when
  `LOGARITHMIC`. `INTEGER` rounds. `TOGGLED` admits only 0 and 1. Overrides
  outside the hints are rejected, not clamped. Inverted bounds, a log
  default with a bound ≤ 0, and a default outside its bounds are errors.
- **Lifecycle.** Instantiate at the input's rate. Connect **every** port,
  including control outputs. Activate, then run. Always deactivate (when
  present) and clean up, including after a failure. Input and output
  buffers are separate, so `INPLACE_BROKEN` is always honored. CAPS defers
  its real activation to the first `run()`.
- **Output.** Every output sample must be finite, or the render fails.
  Upstream builds with `-ffast-math`, so the plugin's own NaN handling is
  not trusted.

## Evidence

From `check.py --save-evidence` on this machine (arm64, macOS, Apple clang
21.0.0, Rust 1.99.1). All of it is in [`evidence/`](evidence/):

- **Reference render:** `recordings/fart_dry.wav` (mono PCM16, 44.1 kHz,
  79,380 frames) through Plate at its defaults. The output is 2 channels,
  228,936 frames, peak 0.602, and the tail is **3.391 s**. RMS over the
  first 0.5 s of the tail is −39.2 dBFS. The float32 render's SHA256 is
  `e2e04bc4…c9c4`. The `plate_pad.listen-pcm16.wav` file is a playable
  PCM16 copy for listening. `plate_pad.render.json` holds the full record.
- **Reproducibility:** a fresh process, a 64-frame block (against 256) and
  discovery through `LADSPA_PATH` each produce **byte-identical** output.
- **Dry/wet oracle:** `blend=0` returns exactly the input samples on both
  channels with no tail. `blend=1` moves further from the dry signal than
  the default does.
- **Sample-rate behavior:** a 100 ms burst has a 2.791 s tail at both
  44.1 kHz and 48 kHz (ratio 1.000). Plate scales its delay lines by the
  sample rate.
- **Hint resolution:** the fixture's `hints` plugin echoes its controls.
  At 44.1 kHz the results are `SAMPLE_RATE` max 0.5·fs = 22050,
  `LOGARITHMIC` middle √(20·20000) = 632.456, `INTEGER` low
  round(2.75) = 3 and `TOGGLED` = 1. `run_adding` gives the same audio as
  `run`, and overrides arrive.
- **Failures:** each of these exits 1, publishes no output and leaves no
  staging directory: a missing library path, a library missing from
  `LADSPA_PATH`, a corrupt file, a dylib with no `ladspa_descriptor`, a
  missing label, a port that is both input and output, inverted bounds, no
  audio output, three outputs, NaN output at frame 1000, a tail that never
  decays (1 s cap), an override above its hint, an unknown control,
  `--run-adding` on a plugin without it, and non-WAV input. An existing
  output directory is refused and left untouched. The lifecycle log shows
  `deactivate` and `cleanup` after the NaN and endless-tail failures.
- `measurements.json` holds the decoded results, the CAPS build record
  (source, patch and library hashes, and the unpatched failure) and every
  failure message.

`verify.py` decodes the float WAVs with its own RIFF parser. It derives the
expected defaults from the LADSPA SDK hint rules by hand, not from the
probe's resolver.

## Reproduce

From the repository root. The first run downloads the pinned tarball into
`.work/`, about 180 KB, and later runs reuse it after checking its hash.
The check uses the swarm's shared lock and target with two Cargo jobs:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
python3 native/docs/spikes/ladspa/check.py \
  --lock-file /Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock \
  --target-dir /Users/tbuddy/dev/brownnote/shared-target \
  --save-evidence --native-gates
```

The check runs, in order: the unpatched-failure build, the patched build,
the fixture plugins (`cc -Werror`), `cargo fmt --check` and
`cargo clippy --locked --all-targets -D warnings` on the probe, every render
and failure case, and `verify.py`. `--native-gates` adds `make native-test`.
Each run goes to a fresh `out/run-*` directory. Those absolute paths
describe this validation machine, so substitute your own lock and target.

To render your own file through any installed LADSPA plugin:

```sh
LADSPA_PATH=$HOME/Library/Audio/Plug-Ins/LADSPA \
  "$CARGO_TARGET_DIR/debug/brownnote-ladspa-probe" render caps.so Plate in.wav out-dir --set tail=0.5
```

Loading a library runs its code in the probe process. This is not a
sandbox. Load only libraries you trust.

## Open gates

- **Listening (maintainer, optional).** Play
  `evidence/plate_pad.listen-pcm16.wav` next to `recordings/fart_dry.wav`
  and confirm that the plate is a sound you want for pads and that the 3.4 s
  default tail is not excessive. An agent cannot hear, so no listening claim
  is made. The maintainer decision recorded on #97 makes this a quality
  follow-up rather than a close gate.
- **#114 production work.** The allow-list and offline adapter landed in
  #164; app registration, audition, project recall (library hash, ID, label
  and controls), retained media (D4) and missing-plugin reporting are
  tracked on #114.
- **Not shown:** Linux, Intel macOS, other Xcode versions, bundling,
  real-time use, and any CAPS descriptor other than Plate.

## Integration handoff

**Wiring and dependency requests: none.** The nested manifest and lock stay
separate from `native/Cargo.toml` and `native/Cargo.lock`. The existing
native gate does not discover this crate, so run `check.py` explicitly.
The issue's suggested `native/tests/ladspa_spike.rs` is not used. It would
put `unsafe` FFI (the native crate denies `unsafe_code`) and a network
fetch of GPL source into `make native-test`. #114 should add its own
behavior tests at the production boundary.
