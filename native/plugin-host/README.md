# Offline plugin helper

`brownnote-plugin-host` hosts **one plugin instance per process** for the
native app's offline renderer
([`native/src/pedals/offline`](../src/pedals/offline)). It speaks the wire
protocol in [`PROTOCOL.md`](../src/pedals/offline/PROTOCOL.md) on stdin and
stdout. The renderer owns block scheduling, latency compensation, tail
measurement, finite checks, deadlines and cancellation. The helper only
loads, configures and runs the plugin.

This is a separate Cargo workspace because the native crate denies `unsafe`
code. All plugin FFI lives in [`host/src/clap.rs`](host/src/clap.rs), ported
from the #96 probe and trimmed to offline effect rendering.

```sh
brownnote-plugin-host clap   # one CLAP effect session on stdin/stdout
```

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
  fault fixture and the pinned effect.
