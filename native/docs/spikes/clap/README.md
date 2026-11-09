# CLAP host spike (#96)

This is a standalone probe. It is not wired into the application and adds
no production dependency, module, UI, project DTO or audio callback. It
hosts two real, pinned CLAP plugins through a minimal host written directly
against the CLAP 1.2 ABI, and records what they actually do.

It covers both review-adjustment deliverables: **one instrument and one
effect, note lifecycle, sample offsets, offline and realtime-mode render,
and state save and restore**, then **load failure, crash, hang and
non-finite output, in-process against a helper process, and a bounded host
recommendation**.

## Findings

All numbers come from [`evidence/measurements.json`](evidence/measurements.json),
produced by `check.py` and checked by `verify.py` (59 checks).

**One host core can drive both plugins.** The same loader, lifecycle,
event, parameter and state code renders the instrument and the effect, in
offline and realtime render modes, on the main thread or on a paced audio
thread. Nothing in the two contracts forced a separate host. What differs is
policy around the core, listed below.

**Note events are sample accurate, and note IDs work, but plugins can still
leave a voice stuck.**

- The first note sounds at exactly its fixture frame (1000) after exact
  silence. Removing note 2 changes the output first at exactly its frame
  (5003), which is not a block boundary.
- Block sizes 256, 128 and 37 render identical PCM. A paced realtime-mode
  thread at 128 frames renders the same PCM as offline.
- Every note ID gets exactly one `CLAP_EVENT_NOTE_END` with its key. The
  100 ms release ends 4816–4848 frames after note-off; a 500 ms release ends
  24047 frames after. Output is exactly zero after the last note end.
- A note-off with a note ID releases only that voice. With two same-key
  voices (IDs 1 and 3), releasing ID 1 leaves ID 3 sounding; a wildcard
  note-off (ID -1) ends both at the same frame. That is D14's
  release-one behavior, provided the host sends note IDs.
- Velocity 1.0 against 0.25 scales RMS by 1.996 (the plugin applies
  `sqrt(velocity)`; expected 2.0).
- **Plugin defect, characterized:** if a same-key note-off arrives while
  another same-key voice is still releasing, the plugin re-releases the old
  voice and the new one sticks forever. No note end is reported for it. A
  host that tracks note ends can detect this; it cannot prevent it.
- **Plugin defect, characterized:** with a 0 ms attack the plugin skips its
  release and cuts the voice at the note-off frame.
- Note-end frames depend on the block size by up to about 64 frames (18816
  at 256 frames, 18832 at 37). Audio does not; voice bookkeeping does.

**Offline effect render works, but the tail contract cannot be trusted.**

- Airwindows Consolidated (default processor Galactic, a reverb) exposes
  `render`, reports no hard realtime requirement, accepts both render modes,
  and reports 0 latency. Offline and realtime modes, block sizes 256 and
  512, and the paced thread all produce identical PCM. Bypass passes the
  known input through with at most 1.5e-8 error.
- It reports a 2 s tail (96000 frames). The measured decay after a 250 ms
  burst reaches -60 dBFS after 6.8 s, -80 dBFS after 9.8 s and -100 dBFS
  after 12.8 s. Output never reaches exact zero (floor around -154 dBFS),
  and `process` never returns `TAIL` or `SLEEP`. An offline renderer has to
  find the end of a tail by measuring silence against a threshold and a
  cap, not by trusting `clap.tail` or the process status.

**State round-trips, but restored state is not restored sound.**

- Instrument: the saved blob reloads byte for byte; the mutated parameter
  returns to its value; in-place restore and fresh-process loads reproduce
  the reference and mutated renders exactly. The gain change scales RMS by
  2.022, as the parameter range predicts.
- Effect: the blob and parameter values round-trip, and loading a blob is
  repeatable across processes. But loading even the unchanged default state
  changes the reverb's output (max sample difference 0.086 against a
  default instance). Reproducing a rendered sound requires retained audio
  (D4), not just the plugin state.

**Timing.** Plugin `process` cost is far below the block budget (instrument
p99 23 µs, effect p99 107 µs against 2.7 ms and 5.3 ms budgets). The paced
thread is an ordinary-priority thread on a shared build machine. In the
recorded run no block was late (worst 2.4 ms); an earlier run under
heavier load saw up to 95 ms with identical PCM. Those figures measure OS
scheduling, not plugin cost or a CoreAudio deadline. No audio device was
opened and nothing was listened to.

## Failure boundaries

A purpose-built fault fixture ([`fault-fixture/`](fault-fixture/)) exposes
six CLAP plugins: a passthrough, and plugins that segfault, hang forever,
emit NaN and infinity, fail `init`, or fail `activate`. Each fault fires on
the eighth processed block of an instance (or at init or activate). Every
fault plugin also has an 8-byte state, so restarts can be checked for state
restore. The same 0.5 s sine (94 blocks of 256 frames) runs through each.

**Load failures are typed and leave the project's state alone.** A missing
bundle, a non-Mach-O file, a real dylib without `clap_entry`, an unknown
plugin ID, a failing `init` and a failing `activate` each stop with a
distinct error (`bundle-missing`, `dlopen-failed`, `no-clap-entry`,
`unknown-plugin-id`, `init-failed`, `activate-failed`), exit code 3, no
output file, and an unchanged state file that was queued for loading.

**In-process, a faulty plugin takes the host down or leaks a thread.**

- Crash: the whole host process dies with SIGSEGV after block 8. No output
  and no summary are written. In the app this would be the whole app,
  including any unsaved project.
- Hang: a watchdog on the main thread notices the missing block in about
  200 ms, but nothing can reclaim the audio thread or destroy the plugin
  while it is stuck inside `process`. The probe has to exit the process.
- Non-finite output: the render completes and 43904 NaN or infinite
  samples reach the host buffer. CLAP gives no signal; only a host-side
  scan catches it.

**In a helper process, the host survives every fault.** The helper is the
same probe binary, hosting one plugin instance and serving one block per
request over pipes. The parent waits for each reply with a deadline.

- Crash: the parent sees the helper exit with signal 11 within 0.1 ms,
  starts a new helper, reactivates it, and restores the last state blob it
  saved (2 restarts, each restoring 8 bytes). The third crash spends the
  restart budget and disables the plugin. All 94 blocks are written; the 70
  blocks the plugin did not produce are dry input.
- Hang: each hang is cut off at the 200 ms deadline (measured 210–211 ms),
  the helper is killed and restarted, and the output is still full length.
- Non-finite output: the parent scans each returned block, disables the
  plugin at block 8, and substitutes dry input. No NaN reaches the output.
- A failing `init` disables the plugin before the first block; the render
  is all dry input.
- The helper transport is lossless: the Airwindows effect at 256 frames
  renders byte-identical PCM through the helper and in-process.

**Isolation cost is measurable but small for these plugins.** Per 256-frame
block, the effect takes 5.0 µs (p50) in-process and 14.5 µs (p50) through
the helper, with p99 22.0 µs and worst 33.1 µs; the budget is 5.3 ms. An
empty passthrough round trip costs 6.9 µs (p50) and 10.1 µs (p99) at 64
frames. Starting a helper and loading a plugin takes 2 ms (fixture) to
9 ms (Airwindows); a restart takes 2–6 ms. Timings vary from run to run;
each run records its own. These are ordinary-priority
processes and blocking pipes on a loaded build machine, so they bound the
cost of isolation, not its realtime safety.

## Bounded host recommendation

The recommendation covers what this probe proved, and only that.

1. **One host core, one FFI crate.** The same loader, lifecycle, event,
   parameter, state and render code drove the instrument, the effect and
   all six fault plugins. Put it in one separate crate that alone allows
   `unsafe`, as `host/src/clap.rs` does here.
2. **Load plugins in a helper process, not in the app.** In-process hosting
   cannot survive a crash and cannot recover from a hang. The helper
   boundary turned both into recoverable events at a cost of tens of
   microseconds per block. One helper per plugin instance keeps the blast
   radius to one track.
3. **Start with offline effect render.** The host renders through the
   helper, faster than realtime, into retained audio (D4). Per block it
   applies a deadline, a restart budget with state restore from the last
   saved blob, and a non-finite scan; any lost block becomes dry input and
   the render reports which blocks were substituted. The end of a tail is
   found by measuring silence against a threshold and a cap, because
   `clap.tail` underreports and `process` never returns `TAIL` or `SLEEP`.
4. **Keep plugin state as an opaque blob next to rendered audio.** Saved
   blobs reload exactly, but reloading the effect's state does not reproduce
   its earlier sound. Retained audio, not the blob, is what plays back.
5. **Send note IDs and track note ends.** Note IDs give D14's release-one
   behavior. Note-end tracking lets the host detect, but not prevent, a
   plugin's stuck voice.
6. **Defer live instruments on the audio callback.** Note timing and render
   modes are proven offline and on a paced thread. A live instrument needs
   a realtime-safe transport (shared memory and a wait-free handshake, not
   pipes), an audio-callback deadline policy, and an allocation-free host
   path. None of that is measured here.

Out of scope until proven separately: plugin GUIs, plugin-initiated
callbacks and restarts, MIDI dialects, note expressions, polyphonic
modulation, transport, and any claim that a project reopens or recalls
correctly with a plugin missing or disabled. That last claim needs #100's
store and is not made here.

## Pinned plugins
Both are fetched or built by [`prepare.py`](prepare.py) into the ignored
`.work/` directory. Nothing is installed system-wide or redistributed. Full
pins are in [`pin.json`](pin.json).

| Role | Plugin | Pin | License |
| --- | --- | --- | --- |
| Instrument | Poly Mod Synth 0.1.0 (`com.moist-plugins-gmbh.poly-mod-synth`), a polyphonic saw synth with note IDs, velocity, attack/release and note-end events | Built from [nih-plug](https://github.com/robbert-vdh/nih-plug) `de421011f41a6d10fc8c7a6084e4f4dee0143683` with `cargo build --locked --release -p poly_mod_synth`. The binary hash depends on the toolchain and is recorded per run. | ISC source. nih-plug's default features also export VST3, so the built binary includes GPLv3 code. |
| Effect | Airwindows Consolidated 1.2026.313 (`com.airwindows.consolidated`), default processor Galactic | Release dmg from the rolling `DAWPlugin` tag at [airwin2rack](https://github.com/baconpaul/airwin2rack) `316a0485b28516347e325645a39c13c623c02962`; dmg SHA-256 `048454ad…3d6c`, executable SHA-256 `92152f95…ad4d`, Developer ID team `547U7G7MZC`. Expanded with `pkgutil`, not installed. | MIT source; the upstream README notes that the JUCE/VST3 DAW build may include GPL3 assets. |

The `DAWPlugin` tag is rolling. If the pinned dmg disappears or its hash
changes, `prepare.py` stops; re-pin explicitly rather than accepting a new
binary.

The tested platform is macOS on Apple silicon (arm64), Rust 1.99.1.

## How the probe works

- [`host/src/clap.rs`](host/src/clap.rs) is the whole FFI boundary: loader,
  factory, lifecycle, audio ports, note ports, params, state streams,
  render, tail and latency, using `clap-sys` 0.5.0 and `libloading` 0.9.0.
  All `unsafe` code in the probe is in this file. The native crate denies
  unsafe code, so a production host needs a separate crate or module with
  that exception.
- [`host/src/main.rs`](host/src/main.rs) has three commands: `describe`,
  `render` (offline or realtime mode, optional paced audio thread, state
  load and save) and `state` (save, mutate, restore within one instance).
- [`host/src/isolate.rs`](host/src/isolate.rs) adds `inproc` (one plugin on
  an audio thread with a main-thread watchdog), `isolate` (the same plugin
  in a helper process with a deadline, restart budget, state restore, dry
  substitution and a non-finite scan) and `helper` (the helper side).
- [`fault-fixture/`](fault-fixture/) is the fault plugin. `check.py` builds
  it with the probe and wraps it in a bundle under `.work/plugins/`.
- [`fixtures/`](fixtures/) holds the event and input scenarios as
  tab-separated frames. `verify.py` derives its expectations from these
  fixtures and the CLAP contract. It decodes WAVs with its own reader,
  measures onsets and silence directly, and uses a Goertzel filter at the
  fixture's note frequencies. Known plugin defects are recorded as
  `characterize:` checks, so a change in behavior fails the check.
- `check.py` runs everything twice where determinism matters: three renders
  repeat in a fresh process and must match byte for byte.

## Reproduce

From the repository root, holding the swarm's whole-check lock and shared
target, with two Cargo jobs:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
python3 native/docs/spikes/clap/check.py \
  --lock-file /Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock \
  --target-dir /Users/tbuddy/dev/brownnote/shared-target \
  --save-evidence
```

Those absolute paths describe this validation environment. Another checkout
passes its own lock and target. The first run downloads about 50 MB and
clones nih-plug into `.work/`; later runs reuse both after re-checking their
hashes and revision. Each run writes a fresh `out/run-*` directory.

`--save-evidence` refreshes [`evidence/`](evidence/): measurements, traces,
`describe` output, state blobs, failure summaries, and six compressed WAVs. Every other WAV is
recorded by SHA-256 in `measurements.json`. To listen to one:

```sh
python3 -c 'import gzip,pathlib; p=pathlib.Path("native/docs/spikes/clap/evidence/synth/life-256.wav.gz"); pathlib.Path("/tmp/clap-life.wav").write_bytes(gzip.decompress(p.read_bytes()))'
```

## Not shown here

- No audio device, CoreAudio callback, realtime-priority thread or
  listening check. The paced thread proves that realtime mode and threading
  do not change the output; it says nothing about deadlines.
- No allocation or lock audit of the host's process path. The probe
  allocates per block. A production audio-thread host needs the `rt_alloc`
  discipline the native engine already has.
- No plugin GUI, transport, MIDI dialect, note expressions, polyphonic
  modulation, voice-info use, or plugin-initiated restart and callback
  handling. The probe counts host requests but services none.
- Faults come from a fixture written to fail on cue. Real plugins can fail
  in ways it does not model: corrupting host memory before crashing,
  hanging in `activate` or `init`, or failing only on some state blobs.
  The helper applies its deadline to load, init and activate as well, but
  only a hang in `process` is tested.
- No shared-memory transport, realtime-priority helper, or audio-callback
  deadline for the helper. The isolation costs are pipe round trips.
- No project save, reopen or recall. Nothing here touches #100's store.
- Two plugins on one platform. No compatibility claim beyond them.

## Integration handoff

**Wiring and dependency requests: none.** The nested workspace keeps its
own `Cargo.toml` and `Cargo.lock`, separate from `native/Cargo.toml` and
`native/Cargo.lock`. `make native-test` does not build it; run `check.py`.

No physical hardware gate applies (#96 needs none). The M1 software
groundwork is merged; this probe does not depend on any M1 code.
