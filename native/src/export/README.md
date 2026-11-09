# Neutral MIDI/audio writers

`adapter::write_project_bundle` exports a current `Document`, `Takes`, and
`EditModel`. `write_saved_project_bundle` reads the kit/media closure with
`Store::inspect_committed`, then uses the supplied takes and edits. The store
does not yet persist takes or edits. Neither entry point invents a saved
performance or consults a render cache, instrument backend, or plugin.

The integrator can call either entry point off the audio/UI callback with a
`BundleRequest`. Its tempo must equal `Takes::tempo()`. Take clocks overlay at
the request's single origin, and musical positions convert at the current
tempo. The adapter consumes `EditModel::effective_notes`, including per-take
quantization, nudge, velocity, deletion, mute, removal, and loop visibility.

## Files and joins

- `manifest.json`: version 3, with read-only v1/v2 support. Real 128-bit take
  and instrument IDs use decimal strings; v1's numeric IDs remain readable.
  The added audio/MIDI joins and original source references require v2. A
  live-take notes part names the external plugin identity and requires v3.
- `notes.mid`: format 1, 960 PPQ, 4/4, one constant-tempo conductor.
  Only instrument parts produce MIDI. Live-instrument note parts stay out on
  purpose: an SMF track cannot carry the plugin identity, so writing one would
  claim a sound the bundle cannot promise.
  Channels are zero-based 0–15, including
  channel 9 without automatic drum remapping. Notes retain exact velocities.
  Nanosecond onsets and absolute ends round independently to nearest ticks,
  ties later. Onset/end error is at most half a tick, duration error at most
  one tick. A gate rounding to zero gets one tick. Tempo rounds to integer
  microseconds per quarter; tick conversion uses that encoded tempo so drift
  does not accumulate against audio. Unrepresentable tempos, track counts,
  or SMF delta times fail rather than wrap.
- `media/<sha256>.wav`: exact original source/retained copies and generated
  float32 stereo hit clips, wet/dry stems, and a reference mix.

Each hit audio entry is an independent clip on its own track. Do not import
overlapping hits destructively onto one pad track. `event.source` joins the
original sound, while `event.asset` joins the audible baked hit. Audio entries
identify wet/dry roles and join event/part/asset IDs. Every fallback starts at
frame zero of `TimeOrigin` and has the same length, including the longest tail.
Hit placement rounds to the nearest output frame, ties later, with at most a
half-frame onset error.

FIFO-compatible same-pitch MIDI overlaps remain together (D14). Non-FIFO
overlaps after editing or across input sources use additional tracks with
unchanged channels and gates. Text markers carry `bn:<take>:<sequence>`,
and the manifest maps every note track to its part and event IDs. This does
not promise that a DAW will retain Brownnote IDs on re-export.

Real part IDs hash the target's kind and stable identity, not its position
among audible parts. Muting/deleting another part does not renumber them.
Any truncated-hash collision fails duplicate-part validation, never merges.

## Live takes

A project with a live CLAP instrument exports every retained take twice: one
`sample_clips` part whose single event is the byte-for-byte retained WAV, and —
when the take has notes — one `live_instrument_notes` part per take:

```json
{
  "id": {"take": "12", "sequence": 0},
  "part": 1234,
  "start_nanos": 100000000,
  "duration_nanos": 50000000,
  "pitch": 60,
  "channel": 0,
  "velocity": 100
}
```

with the part kinds

```json
{"id": 99, "kind": {"type": "sample_clips", "source": "<sha256>"}}
{"id": 1234, "kind": {"type": "live_instrument_notes", "plugin": {
  "backend": "clap", "id": "org.example.synth",
  "artifact": "<sha256>", "abi": "clap-1.2", "version": "0.4.0"}}}
```

The part kind names the external plugin identity, never the sampled-instrument
ID, because only a host that can load that plugin can sound those notes.
Positions are the original sample-timed onsets converted from the take's own
recording tempo, with the effective durations, controls and edits; event IDs
are the real take event IDs, so notes and audio join by take identity. Identity
is name, artifact hash, ABI and version only: the plugin's machine path is
never exported, and no plugin state is claimed to be portable.

## Sound ownership and bounds

Pad hits follow native one-shot playback: linear interpolation, pitch/rate,
linear velocity and gain, constant-power pan, and source-identity 5 ms choke
fades across pads sharing a sound. Captured pad key releases do not cut tails.
Each hit remains separate even when simultaneous. Settings and baked processing
are loss records, not claims of editable plugin state.

For instruments or already-baked per-event variants, the owning feature
supplies `ProjectFrozenHit` with retained project asset IDs. These renders must
correspond to the effective performance and already include hit-local pitch,
gain, velocity, pan, choke, processing, and tails. Export only resamples them.
Optional dry renders are also retained media. Required media never regenerates:
missing or corrupt bytes fail export even if source audio exists.

Without a frozen instrument render, MIDI still exports, but an explicit loss
states that the instrument is absent from audio fallbacks. Live-take notes have
no per-hit render by design: the take's retained audio clip already carries that
performance, so they add no fallback hit and no loss. Unrecoverable dry
audio is also reported, not replaced with wet audio. The reference sums wet
audio, applies the project's static master gain, and clamps to [-1,1], without
normalization or tanh. Stems remain pre-master and unclamped. This offline
handoff preserves all hits; it does not simulate finite live voice-pool
stealing, master automation, or unavailable shared nonlinear master effects.
It is not a listening or SoundCraft-import claim (D19).

Rendering happens off the audio callback and holds decoded/rendered files in
memory. RIFF sizes, output-frame allocations, invalid settings, and non-finite
audio are checked. Large bundles still need enough process memory; streaming
is not implemented. The bundle transaction stages and syncs originals, all generated
WAVs, MIDI, and the manifest before its one publication rename.

## Automated verification

`cargo test --manifest-path native/Cargo.toml --test export_bundle` is the
rerunnable writer contract. It decodes SMF with a separate chunk/VLQ/FIFO reader
and WAVs with hound, tests real store/takes/edits, and removes the original
cache before export and the project before reading a relocated bundle.
Fault tests fail each staged write, including generated WAV, SMF and manifest
writes. No hardware, plugin, or new dependency is required.

Maintainer follow-up: mount an off-thread export action in the UI and supply
effective per-event frozen renders when an instrument/plugin owner exists.
Listening checks and the pinned SoundCraft adapter remain separate gates.
