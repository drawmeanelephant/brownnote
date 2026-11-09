# SoundCraft adapter, fake-executable slice

This adapter consumes only #109's `manifest.json` v1 and content-hash WAV media.
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
relative `Audio Files/`, the untouched source manifest and `media/`, a pin, and
`adapter-report.json`. Existing output or staging directories are refused.
Failures kill the owned process and remove only this invocation's staging.

Every event asset imports to a separate new audio track, even for the same pad.
No destructive same-track packing is used. Full assets retain tails. A duration
that differs from the full asset is reported, not silently trimmed; upstream
must bake choke/envelope/rate into per-hit media. MIDI uses public clip/note
commands at 960 PPQ, not a new SMF codec. Onsets/durations round to the nearest
sample/tick with explicit conversion records. The bundle origin is not added
again to already-relative event positions.

## Mapping boundary

Manifest v1 does not describe sampler roots, key ranges, play modes, plugins or
effects. These are **not inferred** from event renders. MIDI tracks start muted,
so an unrelated default synth or approximate sampler cannot double retained
audio. Notes without retained audio carry an explicit unsupported record.
Original bundle loss records remain unchanged in the report.

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

This is a **source-reviewed candidate**, not a real-app-validated release.
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

## Maintainer real-app gate, not run in this lane

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
control. It then runs the actual adapter on a minimal v1 bundle; the adapter
reopens and inspects counts, clip positions, notes, tempo and media availability.
Neither a skipped test nor fake-process results satisfy this gate.

Finally use a positive synthetic adapter output (the spike's final relocated
session intentionally lacks media). Open it with normal audio, move a hit,
delete another, edit a note, save, quit, reopen, and move the whole directory
after removing original sources. Inspect again and listen to retained audio,
tails, choke and overlap. Unmute editable sampler tracks only deliberately.
Record maintainer listening separately. #110 and #111 stay open until their
actual-app and listening gates pass.
