# SoundCraft adapter

This adapter consumes the current #109 export `manifest.json` (v3, with v1/v2
read support) and content-hash WAV media.
It owns a fresh GUI process through explicit `--app` and `--cli` paths. It never
connects to a personal session, starts audio, uses CLI `run`, or writes private
SoundCraft session JSON.

```sh
cargo run --manifest-path native/Cargo.toml --bin brownnote-soundcraft -- \
  --bundle /absolute/export-bundle \
  --app /absolute/SoundCraft.app/Contents/MacOS/SoundCraft \
  --cli /absolute/soundcraft-cli --output /absolute/new-session-directory
```

Move the **entire** output directory. It contains `session.scraft`, the target's
relative `Audio Files/`, the untouched source manifest, `media/` and SMF, a pin, and
`adapter-report.json`. Existing output or staging directories are refused.
Failures kill the owned process and remove only this invocation's staging.

Every event asset imports to a separate new audio track, even for the same pad.
No destructive same-track packing is used. Full assets retain tails. A duration
that differs from the full asset is reported, not silently trimmed; upstream
must bake choke/envelope/rate into per-hit media. MIDI uses public clip/note
commands at 960 PPQ, not a new SMF codec. Onsets/durations round to the nearest
sample/tick with explicit conversion records. The bundle origin is not added
again to already-relative event positions.

#109 writer bundles set each event's asset to its `HitWet` render, so pad hits
arrive with gain, velocity, pan, pitch and choke already baked. Frozen
instrument renders arrive the same way. If a `HitWet` entry disagrees with its
event's asset, or its onset differs by more than one sample, the bundle is
refused. Dry hits, stems and the reference mix stay in the copied `media/`
for comparison and are not imported as tracks.

## Mapping boundary

The manifest does not describe sampler roots, key ranges, play modes, plugin
state or effects. A `live_instrument_notes` part names the plugin, never its
state. None of this is **inferred** from event renders. MIDI tracks start
muted, so an unrelated default synth or approximate sampler cannot double
retained audio. Notes without retained audio carry an explicit unsupported
record. Original bundle loss records remain unchanged in the report.

`live_instrument_notes` parts name an external plugin identity. This target
cannot load a plugin from a bundle or carry its identity into a track, so each
such part is recorded as an explicit unsupported mapping naming the plugin
(backend, ID, version, short artifact hash) and its note count. The take's
retained audio clip is the fallback, the notes are never written as MIDI, and
no machine path is reported.

Optionally pass `--mappings /absolute/mappings.json`, an adapter-local JSON array:

```json
[{
  "part": 2,
  "asset": "<64 lowercase hex characters for an asset in the bundle>",
  "root_note": 60,
  "key_low": 60,
  "key_high": 72,
  "key_track": true,
  "one_shot": true,
  "choke_group": null
}]
```

Only one sample per instrument part is supported. A choke group or invalid key
range is recorded as unsupported and not sent to the target. No multisample,
plugin-state, gain/velocity/pan-law, envelope, interpolation or gate-retrigger
equivalence is claimed. A successful approximate map remains muted.

## D19 re-pin, 9 November 2026

Latest release: [v0.19.0](https://github.com/storytold/soundcraft/releases/tag/v0.19.0),
published `2026-11-09T11:54:20Z`. Its lightweight tag resolves to
`04878f8fda885c95f8be5893bbfe4ea4e062a814`. [pin.json](pin.json) records release
API digests, checked downloaded bytes, checksum-file digest and extracted macOS
universal executable hashes. The DMG was mounted read-only, then detached.
Neither executable was run. The detached signing signature was not verified.
Runtime version checks are compatibility gates, not binary authentication;
the opt-in real-app recipe additionally checks both executable hashes.

The source review alone was not app evidence; see the real-app check below.
#95's v0.18.0 app evidence is not relabeled as v0.19.0 evidence.

Compared with #95's full commit, upstream reports a diverged history.
Its comparison includes file-export/naming changes, serial mixer rendering,
updater changes, UI/shortcuts and dependency changes. MIDI command/import and
sampler sources are not listed as changed. The CLI/control documents and
`cmd/{file,midi,sampler,track,mix,event}.rs`, `io/midi.rs`, sampler parameter
definitions and save/media ownership were reviewed at the **new full commit**.
Audio import still defaults to a new track; sampler still has no choke group;
save owns relative `Audio Files/`; MIDI note commands still use 960 PPQ.
Source review is not proof that the new binary behaves identically.

Source links use this prefix:
`https://github.com/storytold/soundcraft/blob/04878f8fda885c95f8be5893bbfe4ea4e062a814/`.
Relevant files are `docs/cli.md`, `docs/control-protocol.md`,
`crates/engine/src/cmd/{file,midi,sampler,track,mix,event}.rs`,
`crates/engine/src/io.rs`, `crates/engine/src/io/midi.rs`, and
`crates/dsp/src/plugins/sampler.rs`.

## Real-app check, 9 November 2026

Latest release was still v0.19.0 on 9 November 2026, so the pin above stands.
The DMG and CLI zip matched `SHA256SUMS.txt`. The DMG was mounted read-only and
the app was copied into a scratch directory. Both executables matched
[pin.json](pin.json) before launch. Gatekeeper accepted the app as a notarized
Developer ID build. No quarantine or security setting was bypassed. The host was
macOS 27.3 on Apple silicon with a desktop session.

The opt-in test below passed against the actual v0.19.0 GUI, started with
`--no-audio --control 0` and an isolated profile:

- #95's unchanged fixture tests (4 passed), the independent fixture checker and
  the complete probe. This covers same-track overlap loss, same-pitch MIDI
  import/export, 960 PPQ, 44.1 kHz conversion, sampler gate/one-shot/octave/
  outside-map bounces, choke rejection, move/delete/note edits, quit/reopen,
  source-free relocation and the missing-media negative control.
  Evidence: [evidence/recipe-app.json](evidence/recipe-app.json).
- The adapter on a minimal bundle with an explicit sampler mapping.
- The adapter on a bundle written by the real #109 project, takes and edit
  path. The bundle has two overlapping hits on one pad (one nudged, a third
  deleted) and three overlapping notes, two with frozen renders. Every hit and
  frozen render becomes its own track at the writer's `HitWet` frame. In the
  GUI, one hit is moved, another deleted and one note edited. The session is
  saved, then reopened after quitting. The whole output directory is copied
  and the original output and bundle are removed. The copy reopens with
  identical tracks, notes, tempo and loaded relative `Audio Files/`. Removing
  the WAV used by the moved clip is then reported as missing and unhealthy.
  Evidence: [evidence/adapter-edit.json](evidence/adapter-edit.json).

Observations: SoundCraft keeps the deleted clip's WAV in `Audio Files/`. When
that unused file is removed, `session.open` lists it as missing, but
`session.audio_health` stays healthy. Health reports only media that a clip
uses.

**Not covered:** edits were made through the public control protocol, not the
mouse or keyboard. No audio device was opened, and nobody has listened to the
result. The maintainer listening steps below are an optional quality follow-up.

To repeat the check, prepare the app as described above, then also set
`BROWNNOTE_SOUNDCRAFT_EDIT_EVIDENCE=/absolute/scratch/adapter-edit.json`.
The adapter edit check runs from the same opt-in test.
## Maintainer real-app gate

Use a desktop session and checksum-verified v0.19.0 app/CLI. Read #95's
`REPRODUCE.md`; reuse its scripts **without editing the spike directory**.
Its `prepare.py` is read-only evidence of the download/verification recipe:
it intentionally refuses v0.19.0 because its own pin is v0.18.0. Download the
new artifacts into a separate scratch directory and verify this adapter's pin.
Do not bypass quarantine, launch denials or other OS security controls.

Create a separate Python venv with the spike's frozen
`evidence/python-requirements.txt`. Run the spike's `test_fixtures.py` and
`check_fixtures.py` using that Python. Then:

```sh
export BROWNNOTE_SOUNDCRAFT_RUN_PINNED=1
export BROWNNOTE_SOUNDCRAFT_APP=/absolute/SoundCraft.app/Contents/MacOS/SoundCraft
export BROWNNOTE_SOUNDCRAFT_CLI=/absolute/soundcraft-cli
export BROWNNOTE_SOUNDCRAFT_PYTHON=/absolute/venv/bin/python
export BROWNNOTE_SOUNDCRAFT_EVIDENCE=/absolute/scratch/app.json
cargo test --manifest-path native/Cargo.toml --test soundcraft_adapter \
  pinned_app_recipe_and_adapter -- --ignored --exact
```

The wrapper imports the unchanged `Probe`, `scenario` and independent fixture
checker, but supplies a separate scratch directory instead of spike `.work`.
It verifies binary hashes before launch. The recipe checks overlapping clips,
MIDI notes/channels/velocities/durations, 120 BPM, 960 ticks = 24,000 samples,
44.1 kHz conversion, sampler variants and choke rejection, move/delete/note
edits, save/quit/reopen, source-free relocation, and missing-media negative
control. It then runs the actual adapter on a minimal bundle; the adapter
reopens and inspects counts, clip positions, notes, tempo and media availability.
Neither a skipped test nor fake-process results satisfy this gate.

Finally use a positive synthetic adapter output (the spike's final relocated
session intentionally lacks media). Open it with normal audio, move a hit,
delete another, edit a note, save, quit, reopen, and move the whole directory
after removing original sources. Inspect again and listen to retained audio,
tails, choke and overlap. Unmute editable sampler tracks only deliberately.
Record maintainer listening separately. The actual-app edit/save/reopen gate
passed on the committed evidence and #110 closed on it; the listening pass is
an optional quality follow-up, not a close gate. #111 stays open until its
real-project convergence gate passes.
