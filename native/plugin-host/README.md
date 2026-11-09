# Offline plugin helper

`brownnote-plugin-host` hosts **one plugin instance per process** for the
native app's offline renderer
([`native/src/pedals/offline`](../src/pedals/offline)). It speaks the wire
protocol in [`PROTOCOL.md`](../src/pedals/offline/PROTOCOL.md) on stdin and
stdout. The renderer owns block scheduling, latency compensation, tail
measurement, finite checks, deadlines and cancellation. The helper only
loads, configures and runs the plugin.

This is a separate Cargo workspace because the native crate denies `unsafe`
code. All plugin FFI lives in `host/src/clap.rs` (ported from the #96 probe and
trimmed to offline effect rendering) and `host/src/ladspa.rs` (the #97
probe's ABI, loader and hint rules).

```sh
brownnote-plugin-host clap                              # one CLAP effect session on stdin/stdout
brownnote-plugin-host ladspa [--allow UniqueID:Label]   # one LADSPA effect session
```

## How the app uses both helpers

The native app registers a helper for each backend in
`ui::pedals::plugin::default_helpers`. For each backend its environment
variable wins (`BROWNNOTE_CLAP_HELPER`, `BROWNNOTE_LADSPA_HELPER`);
otherwise a `brownnote-plugin-host` next to the running executable is used.
The helper runs with `clap` or `ladspa` as its argument.

A `.clap` bundle is a CLAP plugin. A bare shared library is ambiguous, so
when the LADSPA helper is configured the app probes it as LADSPA first. If
that probe cannot find or load the plugin (`unknown-plugin-id`,
`bundle-missing`, `dlopen-failed`, `no-entry`), the app probes the same
library as CLAP, so a CLAP `.so` is not misrouted. A refused LADSPA feature,
such as the allow-list, keeps its message.

Add a LADSPA plugin by path and ID, for example the provisioned `caps.so`
with `1779:Plate`; a CLAP plugin ID looks like
`com.airwindows.consolidated`. The pad renders the chain offline, retains
the result, and reopens it without the plugin.

## What the CLAP helper does

Steps in order:

1. Resolve a `.clap` bundle to `Contents/MacOS/<first file>`, or use a plain
   shared library directly. Hash that executable (SHA-256).
2. Load it, check the CLAP version, and find the requested plugin ID.
3. Check the plugin's audio ports. The helper requires main input and output
   ports with exactly the requested channel count; extra input ports get
   silence. A plugin that declares a hard realtime requirement, or refuses
   `CLAP_RENDER_OFFLINE`, is rejected as `unsupported`.
4. Apply the state blob, then the parameters (each must exist and be within
   its range). Activate, query latency and tail, and start processing.
5. Process blocks on one thread, then stop, save the state, deactivate and
   exit.

Plugin output written to stdout is redirected to stderr, so it cannot
corrupt the protocol.

## What the LADSPA helper does

The helper loads only allow-listed plugins: the built-in list holds CAPS
`1779:Plate` (#97's selected target) and `--allow UniqueID:Label` adds to
it (the tests use that for the fault fixture). Loading a library runs its
code, so the allow-list is the trust boundary; anything else is refused as
`unsupported` before its file is opened. Steps in order:

1. Resolve the location: a path containing `/` is used as given; a bare
   file name is searched in `LADSPA_PATH`, or the user and system LADSPA
   folders when that is unset. Hash the library file (SHA-256).
2. Load it, find the descriptor with the requested `UniqueID:Label`, and
   validate it: exactly one direction and one kind per port (private
   descriptor bits are masked, not rejected), required functions present,
   bounded port and descriptor counts.
3. Reject `LADSPA_PROPERTY_REALTIME` plugins (an offline render of one
   would not be reproducible), any state blob (LADSPA 1.1 has no state),
   and layouts the plugin cannot do: 1 or 2 audio inputs, and exactly the
   render's channel count in outputs. A mono render is duplicated into two
   inputs; a stereo render is averaged into one.
4. Resolve each control input's bounds and default from its port hint at
   the render's rate (`SAMPLE_RATE` scales, LOW/MIDDLE/HIGH interpolate,
   geometric when `LOGARITHMIC`, `INTEGER` rounds, `TOGGLED` is 0 or 1;
   a missing default is the lower bound). Each given parameter must be a
   control input port index within those rules; out-of-hint values are
   rejected, never clamped.
5. Instantiate, connect **every** port (control outputs included), activate,
   and process blocks with `run` on one thread. `run_adding` is never
   called, and input and output buffers are always separate allocations, so
   `INPLACE_BROKEN` plugins are always safe. On end, deactivate (when
   present) and clean up, including on every failure path (the instance's
   `Drop`).

LADSPA 1.1 declares no latency, tail or plugin version. The helper reports
latency 0 and no declared tail (the renderer measures the tail), and the
plugin identity is `backend: "ladspa"`, `id: "UniqueID:Label"`,
`abi: "ladspa 1.1"`, `version: "unversioned"` with the library hash as
`artifact`, so a changed library is a changed identity.

### The pinned CAPS Plate

[`caps/provision.py`](caps/provision.py) fetches, verifies, patches and
builds the pinned CAPS 0.9.26 library (#97's pin, including the arm64 clang
patch), and refuses to produce a library whose SHA-256 differs from
[`caps/pin.json`](caps/pin.json):

```sh
python3 native/plugin-host/caps/provision.py --expect-unpatched-failure
python3 native/plugin-host/caps/provision.py --install ~/Library/Audio/Plug-Ins/LADSPA
```

CAPS is GPL-3.0-or-later and Brownnote is MIT: the plugin is not bundled,
the user (or CI, via this script) builds it, and Brownnote loads whatever
was provisioned. Rendered audio is program output and is retained as
project media (D4). This is a reading of the licenses, not legal advice.

`native/tests/ladspa_offline.rs` runs the fault fixture and the pinned
Plate through the public `pedals::offline::{probe, render}`. The mounted
app workflow is `make native-ladspa-workflow-test`, which adds `1779:Plate`
from the provisioned `caps.so`, edits it, auditions, saves, reopens without
the library, relinks and exports:

```sh
BROWNNOTE_LADSPA_HELPER=.../brownnote-plugin-host \\
BROWNNOTE_LADSPA_FIXTURE=.../libbrownnote_ladspa_fault_fixture.dylib \\
BROWNNOTE_REQUIRE_LADSPA=1 cargo test --manifest-path native/Cargo.toml --test ladspa_offline
# plus the pinned Plate (cannot skip with BROWNNOTE_REQUIRE_LADSPA_PLATE=1):
BROWNNOTE_LADSPA_PLATE=native/plugin-host/caps/.work/patched/caps-0.9.26/caps.so \\
BROWNNOTE_REQUIRE_LADSPA_PLATE=1 cargo test --manifest-path native/Cargo.toml --test ladspa_offline
```

The Plate test reproduces #97's reference render of
`recordings/fart_dry.wav` sample for sample (3.391 s measured tail at
-90 dBFS, 50 ms hold), checks defaults at 44.1 and 48 kHz, exact
`blend = 0` dry output, rate-proportional tails, byte-identical repeats
across processes, block sizes and `LADSPA_PATH` discovery, and typed
refusals (missing/corrupt/non-LADSPA library, unknown ID, malformed
descriptors, realtime-only, state, wrong layout, non-finite output, hang,
crash, cancel, tail cap). The fault fixture's crash raises `SIGSEGV` with
the default disposition; it never touches an invalid pointer.

## Tests

`make native-test` builds this workspace (fmt, clippy `-D warnings`,
`--locked` build). It then runs `native/tests/clap_offline.rs` against the
helper and the [`fault-fixture`](fault-fixture) plugin, with
`BROWNNOTE_REQUIRE_CLAP=1` so those tests cannot skip. The fixture provides a
passthrough plus plugins that crash, hang, emit NaN, or fail `init` or
`activate`.

The pinned real effect is opt-in, because it is downloaded rather than
bundled:

```sh
BROWNNOTE_CLAP_EFFECT="/path/to/Airwindows Consolidated.clap" make native-clap-effect-test
```

The bundle is Airwindows Consolidated 1.2026.313, executable SHA-256
`92152f95a7053ecd65c4e5f356e719c6b82a7dfe4083bc12848f5c755077ad4d`. #96's
`native/docs/spikes/clap/prepare.py` fetches and verifies it into an ignored
`.work/plugins/` directory. It is not redistributed. The test checks:

- The bundle's identity.
- That the render reproduces the #96 probe's in-process render sample for
  sample.
- That the measured tail exceeds the declared 2 s tail.
- That a fresh helper process, and a different block size, give the same
  output.
- That a parameter change alters the sound.
- That reloading the saved state still changes the output, which is why
  rendered audio has to be retained.

## Not covered

- Plugin GUIs, host extensions (thread-check, log, params rescan) and
  plugin-initiated restarts. Requests are ignored.
- Note ports, MIDI, live or realtime use. This helper is offline only.
- Platforms other than macOS on Apple silicon (D1). Plugins other than the
  fault fixtures, the pinned CLAP effect and the pinned CAPS Plate.
- LADSPA instruments, live LADSPA hosting, and any LADSPA plugin outside
  the allow-list.
