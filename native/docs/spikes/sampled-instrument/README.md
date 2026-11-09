# Sampled-instrument backend spike (#108, first PR)

> Follow-up: the app integration keeps D14 by giving each overlapping
> same-pitch note its own RustySynth channel. See `../../sampled-instrument.md`.

## Recommendation

**No-go for direct native integration of RustySynth 1.3.8. Bounded go for
further RustySynth evaluation, not a backend approval.** Its MIT license,
dependency-free Rust implementation and working arm64 offline renderer fit
D18. However, note-off by channel and pitch releases **all** matching notes.
That violates D14, which requires releasing the oldest matching note only.
Do not wire this probe into the application or change D14 to fit the backend.

This is a standalone executable, not an unmounted application feature. It
adds no production dependency, module, project DTO, UI, audio callback, or
controller path. It does not reverse D18 or pull #96 forward. #108 stays open.

## Candidate comparison

Official sources were checked on 8 November 2026. Only RustySynth was built
or rendered here. Source-level observations for the other candidates are
not runtime evidence.

| Candidate | License, maintenance and pin | macOS arm64 viability |
| --- | --- | --- |
| **RustySynth** (SF2) | MIT. Selected exact crate **1.3.8**, published 20 October 2026; upstream release preparation revision `8aa60afb3c14be0fd53d3ef40ef44a7ff8b72a45`. Cargo.lock pins the registry checksum. No dependencies other than std in the backend. Hound is used only by this probe to write WAV. | Built and rendered on this arm64 Mac with Rust 1.99.1. No C library or device API needed. This proves standalone offline viability, not audio-callback safety or acceptable live latency. |
| **OxiSynth** (SF2, optional SF3) | **LGPL-2.1**, not permissive. Manifest 0.1.0; latest release 25 May 2025; examined repository revision `151b2be8dfcbe9b223d3454f172ffbbc22858808` (16 November 2025). | Pure safe Rust, with WASM as an explicit target. Plausible arm64 core candidate, but not built here. License obligations would need separate review before distribution, including static linking. |
| **XSynth** (SF2/SFZ subsets) | **LGPL-3.0**, not permissive. Latest release 0.4.0, 10 May 2026; examined revision `3a88abe4a8c1005a5ee84061a8be0da025960b44` (29 October 2026). | Uses SIMD and multithreading. Reviewed lint CI runs on Ubuntu/nightly, which is not arm64 macOS proof. Not built here; platform/toolchain and bounded callback behavior remain unverified. |

| Candidate | Note identity / lifecycle | Sustain, bounds, panic | Recall, assets, rendering and fallback |
| --- | --- | --- | --- |
| **RustySynth** | API accepts channel/key/velocity, not Brownnote event IDs. Overlap across different pitches/channels works. Same-channel, same-pitch note-off releases all matching voices in the pinned latest release, reproduced below. Velocity zero acts as note-off. | CC64 hold/release works in fixture. Configurable **8–256 voices**; this probe tests the minimum bound (8) with 16 simultaneous single-region notes. `note_off_all(true)` clears voices, but up to one cached block can remain audible; `reset()` also discards that block. | Reconstruct from exact SF2 bytes, settings, bank/program, master gain and event stream. No opaque project-state serializer. Offline stereo float renderer. Missing/corrupt SF2 fails explicitly; nonexistent presets silently fall back to a default. Application must validate preset availability and must retain reference audio separately. |
| **OxiSynth** | Typed MIDI note-on/off still use channel/key rather than per-note handles. FIFO behavior was **not established**. Do not infer D14 compliance from the FluidSynth lineage. | Descriptor includes a polyphony bound; MIDI/control/reset surface exists. Sustain, stealing and panic timing were **not probed**. | Font IDs and bank/program selection can be queried/reapplied. Explicit `select_program` returns `PresetNotFound`. Float rendering API exists. Full state, missing-font behavior, and fallback portability were **not probed**. Retained audio would still be Brownnote's responsibility. |
| **XSynth** | `NoteOff { key }` routes to `release_next_voice` in each key group, a promising release-one design, not proof of Brownnote ID/FIFO semantics. No Brownnote event IDs in that interface. | Source documents CC64, CC120, CC121 and CC123. `max_layers` limits a different shape from a global voice cap; a global bound and exact sustain/stealing/panic behavior remain **unverified**. | Soundfont/preset configuration and shared realtime/offline stack exist. SFZ includes and sample paths add required-asset closure work. Runtime state recall, missing assets, and retained-audio fallback were **not probed**. |

Testing one permissively licensed implementation is more useful here than
building three production adapters. Neither LGPL
candidate was rejected as technically impossible. They are not a reason to
reverse D18 without further evidence.

### Official source record

- RustySynth: [repository](https://github.com/sinshu/rustysynth),
  [pinned manifest](https://github.com/sinshu/rustysynth/blob/8aa60afb3c14be0fd53d3ef40ef44a7ff8b72a45/rustysynth/Cargo.toml),
  [MIT license](https://github.com/sinshu/rustysynth/blob/8aa60afb3c14be0fd53d3ef40ef44a7ff8b72a45/LICENSE.txt),
  [note-off, preset fallback, reset and render](https://github.com/sinshu/rustysynth/blob/8aa60afb3c14be0fd53d3ef40ef44a7ff8b72a45/rustysynth/src/synthesizer.rs),
  [voice allocation](https://github.com/sinshu/rustysynth/blob/8aa60afb3c14be0fd53d3ef40ef44a7ff8b72a45/rustysynth/src/voice_collection.rs),
  [settings bounds](https://github.com/sinshu/rustysynth/blob/8aa60afb3c14be0fd53d3ef40ef44a7ff8b72a45/rustysynth/src/synthesizer_settings.rs),
  [published API](https://docs.rs/rustysynth/1.3.8/rustysynth/struct.Synthesizer.html).
- OxiSynth: [repository](https://github.com/PolyMeilex/OxiSynth),
  [pinned license metadata](https://github.com/PolyMeilex/OxiSynth/blob/151b2be8dfcbe9b223d3454f172ffbbc22858808/oxisynth/Cargo.toml),
  [descriptor](https://github.com/PolyMeilex/OxiSynth/blob/151b2be8dfcbe9b223d3454f172ffbbc22858808/oxisynth/src/lib.rs),
  [MIDI/program API](https://github.com/PolyMeilex/OxiSynth/blob/151b2be8dfcbe9b223d3454f172ffbbc22858808/oxisynth/src/api/midi.rs).
- XSynth: [repository and format limits](https://github.com/BlackMIDIDevs/xsynth),
  [license](https://github.com/BlackMIDIDevs/xsynth/blob/3a88abe4a8c1005a5ee84061a8be0da025960b44/LICENSE),
  [channel events](https://github.com/BlackMIDIDevs/xsynth/blob/3a88abe4a8c1005a5ee84061a8be0da025960b44/core/src/channel/mod.rs),
  [release-next implementation](https://github.com/BlackMIDIDevs/xsynth/blob/3a88abe4a8c1005a5ee84061a8be0da025960b44/core/src/channel/key.rs),
  [CI](https://github.com/BlackMIDIDevs/xsynth/blob/3a88abe4a8c1005a5ee84061a8be0da025960b44/.github/workflows/check.yml).

## Instrument rights are separate from code licenses

The probe's only other Rust dependency is
[Hound 3.5.1](https://docs.rs/crate/hound/3.5.1), licensed under Apache-2.0.
It writes WAV files. Python's standard library independently decodes them.
No dependency source or executable is bundled in this PR.

**Bundled mechanics fixture:** `fixtures/mechanics.sf2` is an original
integer triangle sample in a minimal one-preset SF2. `generate_fixtures.py`
generates its bytes without an external recording or sample library. It,
the generator, event fixture, and resulting mechanics WAVs use Brownnote's
[MIT license](../../../../LICENSE). No third-party instrument is bundled.
SHA256SUMS pins the SF2 and event bytes. The fixture has one mono looped
region, fast attack, a 250 ms release, root key A4 and no exclusive class.
It is **not a useful musical reference instrument** or a format conformance
suite.

**Proposed useful reference:** user-supplied **TimGM6mb.sf2**, bank 0,
program 0 (acoustic piano), from a lawfully obtained original distribution.
[MuseScore's official handbook](https://musescore.org/en/handbook/2/soundfonts-and-sfz-files)
identifies Tim Brechbill's 5.7 MB GM bank and its **GPL version 2** license.
That asset license is not the MIT backend license. No real reference asset
was supplied to or used by this probe. No asset hash, working piano render,
redistribution permission beyond that published license, or listening
approval is claimed. This PR does not download, bundle or redistribute it.
Before integration, record the actual file hash, origin, accompanying
license/attribution and required assets; confirm the intended use and any
redistribution obligations. A different user-supplied licensed SF2 can be
chosen explicitly instead.

This missing real-reference run is an open gate, not replaced by the
generated triangle. A manual render can use the CLI below with that file,
but the mechanics assertions in `verify.py` apply **only** to the pinned
triangle, not arbitrary SF2 instruments.

## Reproduce

From the repository root, use the same whole-check lock and target as the
other swarm lanes. `check.py` holds the lock across the entire check,
sets two Cargo jobs, and touches only the local crate entry-point mtimes
to prevent stale cross-worktree reuse. It does not modify their contents.

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export BROWNNOTE_REQUIRE_BENCH=1
export BROWNNOTE_BIN=/Users/tbuddy/dev/brownnote/t3-51ff88eb/build/brownnote
export BROWNNOTE_BENCH=/private/tmp/brownnote-pr-review.iD6XSs/vendor/GuitarPedal/Validation/bench/bench
export BENCH="$BROWNNOTE_BENCH"
python3 native/docs/spikes/sampled-instrument/check.py \
  --lock-file /Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock \
  --target-dir /Users/tbuddy/dev/brownnote/t3-51ff88eb/native/target \
  --save-evidence --native-gates
```

Those absolute paths describe this validation environment, not bundled
runtime requirements. A different checkout can pass a shared lock/target
of its own. Do not run two independent targets for this swarm.

The runner checks regenerated fixture bytes, `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings`, runs the real probe
twice in separate processes, decodes each WAV through Python's independent
`wave`/PCM implementation, checks asset failures and detached retained WAV
decoding, and optionally runs `make native-test` without skipping ignored
tests. It creates a fresh `out/run-*` directory each time and preserves
the render, trace, saved config and measurements there. `--save-evidence`
refreshes the committed compact evidence. After changing the generator,
add `--regenerate-fixtures` to rebuild the pinned fixture before checking
its bytes. No standalone test requires MIDI hardware,
an audio device, a network service, or a listener.

To render a user-supplied instrument manually, use the same outer lock
wrapper (from this actual worktree):

```sh
cd /Users/tbuddy/dev/brownnote/t3-b2359cac
PATH="/opt/homebrew/opt/rustup/bin:$PATH" python3 -c \
'import fcntl,os,subprocess,sys; f=open("/Users/tbuddy/dev/brownnote/.brownnote-swarm-validation.lock","a"); fcntl.flock(f,fcntl.LOCK_EX); os.environ.update(CARGO_TARGET_DIR="/Users/tbuddy/dev/brownnote/t3-51ff88eb/native/target",CARGO_BUILD_JOBS="2"); [os.utime(p,None) for p in ("native/src/lib.rs","native/src/main.rs")]; sys.exit(subprocess.call(sys.argv[1:]))' \
  cargo run --locked \
  --manifest-path native/docs/spikes/sampled-instrument/Cargo.toml -- \
  /absolute/path/to/user-supplied/TimGM6mb.sf2 \
  native/docs/spikes/sampled-instrument/fixtures/config.txt \
  native/docs/spikes/sampled-instrument/fixtures/events.tsv \
  native/docs/spikes/sampled-instrument/out/user-reference
```

Loading/parsing SF2 is offline, not real-time callback work. This is not an
untrusted-file sandbox. Review asset size, parsing and allocation bounds
before adding an import surface.

## Evidence and its limits

The fixture event IDs remain in `trace.tsv`; they are intentionally **not**
pretended to be backend note handles. The renderer applies ordered events
between renders of arbitrary lengths. Most events align to 64-frame
boundaries, with panic and reset deliberately placed **1 frame into a
block**, leaving the largest possible unread cached block.
Effects are disabled, so silence is not confused with a reverb tail.
This does not prove Brownnote's scheduler or sample-accurate event handling.

`verify.py` uses an oracle derived from D14 and the known fixture signal,
not the backend's voice-count getter:

- Check zero PCM before note-on, an onset within 128 frames, and exact
  terminal silence in both channels for every scenario.
- Check two different fundamentals while notes overlap, then only the held
  pitch after the other releases.
- Contrast the repeated-pitch case against the same event stream without
  its first note-off. The first stream becomes silent despite the second
  note still being held. That is an explicitly recorded **D14 blocker**,
  not a passing FIFO acceptance test.
- Check velocity changes the decoded RMS by more than a conservative
  factor of two, velocity zero releases, CC64=127 holds and CC64=63 does not.
  Nonzero decisions use a two-PCM-step floor, not human audibility.
- Project onto 16 known fundamentals with a Hann window during steady
  playback. Eight must remain above 10% of the strongest fundamental
  after submitting 16 single-region notes. Their first triangle harmonics
  are outside this frequency range. This is evidence for the **fixture's**
  voice cap, not every layered/stereo soundfont or every polyphony setting.
- Check panic's worst within-block position reaches exact silence within
  64 frames; reset discards cached audio immediately.
- Compare exact PCM from the reset frame onward after reapplying settings,
  reconstruction from
  saved configuration and a fresh instance. Repeat all WAVs in a second
  fresh process. This is reset/config recall with effects off, **not**
  persistence of live envelopes, arbitrary CC state, effect tails or
  bit-identical output across different CPUs/backend versions.
- Check a missing preset produces the same default sound, which future
  application code must detect rather than silently accept.

`evidence/measurements.json` records the decoded results, complete-WAV
SHA256 values and check names. `evidence/trace.tsv` preserves actual event
dispatch. `evidence/*.wav.gz` preserve every actual WAV, with deterministic
gzip headers. They are mechanics audio only. Decompress one for inspection:

```sh
python3 -c 'import gzip,pathlib; p=pathlib.Path("native/docs/spikes/sampled-instrument/evidence/repeat.wav.gz"); pathlib.Path("/tmp/brownnote-repeat.wav").write_bytes(gzip.decompress(p.read_bytes()))'
```

The [validation record](VALIDATION.md) lists the commands, native gates,
initial verifier correction, and untested workflow requirements.

The detached WAV check only establishes that a rendered file can decode
without the SF2 or backend. It is **not** retained-project-media integration,
save/quit/reopen/relocate coverage, or a missing-dependency application UI.
No GPU, controller, listening, or physical-hardware claim follows from it.

## Integration handoff

**Wiring/dependency requests for this PR: none.** Keep the nested manifest
and lock separate from `native/Cargo.toml` and `native/Cargo.lock`. The
existing native gate does not discover this nested crate; run `check.py`
explicitly as above. No global CI/release wiring is requested in this spike.

Before any production wiring, resolve these gates in subsequent work:

1. Prove D14-compliant release-one identity using an existing backend API
   or a narrowly reviewed upstream change. Do not merely count host event
   IDs while sending pitch-wide note-offs. Separate-channel behavior here
   is diagnostic, not a general channel-allocation design.
2. Obtain and pin a useful real reference asset with its own license;
   render the intended range and obtain the maintainer's listening check.
3. Define complete instrument configuration and asset closure with #100,
   including explicit preset validation and missing-dependency reporting.
   Preserve sound as retained project media under D3/D4.
4. Prove note identity, sustain, panic, timing, bounded voices/allocations,
   overload and device/reset behavior through #104/#105's actual native
   entry points, not this event reader. Select sample/block configuration
   with real callback measurements; this spike makes no latency/CPU claim.
5. Integrate #106 recording and native save/quit/reopen/relocate, including
   retained-audio playback with the backend or SF2 unavailable. Then prove
   #109/#110 editable-note mapping/fallback and #111's full workflow.

No physical MIDI hardware is available (D2). Virtual IAC/keyboard work and
maintainer listening can be future evidence, never physical-controller
proof. M0 CLAP/LADSPA research remains deferred under D20 while M1 closes.
