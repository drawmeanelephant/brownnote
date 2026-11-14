# SoundCraft adapter

This adapter consumes the current #109 export `manifest.json` (v4, with
v1/v2/v3 read support) and content-hash WAV media.
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
must bake choke/envelope/rate into per-hit media. A zero-duration one-shot is
legal: its full retained asset is the imported sound. MIDI uses public
clip/note commands at 960 PPQ, not a new SMF codec. It follows the writer's
contract exactly: absolute onset and end round independently to the encoded
tempo, and every gate gets at least one tick, so zero-length and sub-tick notes
stay editable instead of blocking the handoff. The bundle origin is not added
again to already-relative event positions.

## Track and clip identity

A content hash is not an arrangement. During assembly, beside the static master
volume, every imported audio track is renamed with the target's public
`track.rename` and every imported clip with `clip.rename`. The name is
`<pad or part> · Take <k> · Hit <n>`, for example `Pad 03 · Take 1 · Hit 24`.
Pad parts use `Pad 01`..`Pad NN`; instrument-render, retained-take and live-note
parts use `Part <id> note`, `Part <id> take audio` and `Part <id> live note`.
MIDI instrument tracks are named `Part <id> · MIDI · Editable, muted`.

Names are deterministic for a bundle. Takes are numbered by first appearance in
the manifest and hits by manifest order, so the suffix stays unique across
overlapping hits on one pad, separate parts that reuse a pad, and 128-bit take
IDs. The target trims and truncates names to 64 Unicode scalar values and
rejects a duplicate track name, so the whole name is bounded before it is sent:
the variable pad/part prefix is truncated first and the ` · Take <k> · Hit <n>`
suffix is always kept. `adapter-report.json` records that same join on
`audio[]` as `event`, `track`, `clip` and `name` (and `midi[]` as `part`,
`track`, `clip`, `name`), so an editor can map a timeline track back to the
manifest without guessing from hashes.

The renaming never touches media. `media/<sha256>.wav` in the bundle and
`Audio Files/<sha256>.wav` in the session stay content-addressed, clips keep
their full baked length, and overlapping hits stay on separate tracks. On
reopen, `verify()` fails the conversion if any saved track name or clip name
differs from the name it sent. The strict fake target in
`native/tests/fixtures/soundcraft_adapter/fake.py` enforces the same 64-scalar
truncation, empty-name rejection and duplicate-track-name error as the released
app, so the default suite proves the naming contract without the GUI.

Only media the target actually imports must be in its proven PCM16/float32
mono/stereo 44.1/48 kHz slice. Retained source-only provenance may be any
native-supported 8-192 kHz WAV; its bytes are still hash/size-verified and
copied into `media/`. The static `master_gain` is applied once to each imported
pre-master clip track through the target's track volume, leaving per-hit gain,
velocity, pan and the editable clips untouched. At zero it maps to the
target's -144 dB fader floor and the report states that this is effectively
silent rather than mathematically zero.

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
- The adapter on a manifest v3 bundle from the live-take export path: one held
  note played through a live CLAP instrument and a pad hit from the same pass,
  beside the take's retained audio. The pinned app records one unsupported
  mapping naming the plugin (backend, ID, version, short artifact) and the note
  count. The note is not written as MIDI, the retained take and the pad hit
  import as two separate audio clips on their own tracks, and no machine path
  appears in the report. This is the #171 destination decision, checked against
  the actual app instead of the fake target.
  Evidence: [evidence/live-take.json](evidence/live-take.json).
- Track and clip names (#233): the real #109 writer bundle converts with
  readable `Pad 03 · Take 1 · Hit 1`-style names, a fresh app reopens the saved
  session, and `session.inspect` reports the identical track and clip names.
  The whole output directory is then copied, the original output and the
  bundle are removed, and the reopened session still reports the same names
  with loaded relative `Audio Files/`. Two app launches exit cleanly and no
  `SoundCraft` process is left behind.
  Evidence: [evidence/track-names.json](evidence/track-names.json).

Observations: SoundCraft keeps the deleted clip's WAV in `Audio Files/`. When
that unused file is removed, `session.open` lists it as missing, but
`session.audio_health` stays healthy. Health reports only media that a clip
uses.

**Not covered:** edits were made through the public control protocol, not the
mouse or keyboard. No audio device was opened, and nobody has listened to the
result. The maintainer listening steps below are an optional quality follow-up.

To repeat the check, prepare the app as described above, then also set
`BROWNNOTE_SOUNDCRAFT_EDIT_EVIDENCE=/absolute/scratch/adapter-edit.json`.
The adapter edit check runs from the same opt-in test. The live-take case is a
second opt-in test; set `BROWNNOTE_SOUNDCRAFT_LIVE_EVIDENCE` to its evidence
path. The track/clip naming case is a third opt-in test; set
`BROWNNOTE_SOUNDCRAFT_NAMES_EVIDENCE` to its evidence path.
`make native-soundcraft-test` runs all five.

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
export BROWNNOTE_SOUNDCRAFT_LIVE_EVIDENCE=/absolute/scratch/live-take.json
export BROWNNOTE_SOUNDCRAFT_NAMES_EVIDENCE=/absolute/scratch/track-names.json
cargo test --manifest-path native/Cargo.toml --test soundcraft_adapter \
  pinned_app_recipe_and_adapter -- --ignored --exact
cargo test --manifest-path native/Cargo.toml --test soundcraft_adapter \
  pinned_app_reports_v3_live_take_notes_and_imports_the_retained_audio \
  -- --ignored --exact
cargo test --manifest-path native/Cargo.toml --test soundcraft_adapter \
  pinned_app_track_and_clip_names_survive_save_quit_reopen -- --ignored --exact
cargo test --manifest-path native/Cargo.toml --test soundcraft_adapter \
  pinned_app_master_gain_bounce_matches_the_reference -- --ignored --exact
cargo test --manifest-path native/Cargo.toml --test soundcraft_adapter \
  pinned_app_accepts_writer_minimum_gates_and_source_only_rates \
  -- --ignored --exact
```

The wrapper imports the unchanged `Probe`, `scenario` and independent fixture
checker, but supplies a separate scratch directory instead of spike `.work`.
It verifies binary hashes before launch. The recipe checks overlapping clips,
MIDI notes/channels/velocities/durations, 120 BPM, 960 ticks = 24,000 samples,
44.1 kHz conversion, sampler variants and choke rejection, move/delete/note
edits, save/quit/reopen, source-free relocation, and missing-media negative
control. It then runs the actual adapter on a minimal bundle; the adapter
reopens and inspects counts, clip positions, notes, tempo and media availability.
The live-take test builds its bundle through the live-take export path and
checks the same pinned app against a `live_instrument_notes` part: one
unsupported record naming the plugin and its note count, the retained take and
the pad hit as two separate audio clips, no MIDI part, and no machine path in
the report. The master-gain test converts at master 0, 0.25, 0.8 and 1.0, then
bounces each saved session offline through the pinned GUI and compares its peak
with the bundle's reference. The minimum-gate test converts a writer bundle
with a zero-length one-shot and zero/sub-tick instrument notes, then converts
8, 96 and 192 kHz source-only bundles whose imported hits are 48 kHz.
The track-name test converts the writer bundle, asserts every assigned name is
bounded and unique, reopens the saved session in a fresh app and compares every
`session.inspect` track name and clip name with the adapter report, then
repeats after copying the output and deleting the original output and bundle.
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
