# Live CLAP instrument (#112)

One live CLAP instrument belongs to a project. It is recalled by identity and
opaque state, plays into recorded takes, and keeps those takes playable after
the plugin is gone. This is the app-facing recall contract on top of the live
host in `native/src/plugins/live.rs` (slice A).

## Identity

The project stores the tuple the helper reports when it loads the plugin:

- backend (`clap`),
- plugin ID,
- SHA-256 of the loaded artifact,
- ABI and version strings.

The bundle path is stored separately as `location_hint`. It is a machine hint
for reconnecting the bundle on this machine (D8), never identity, and never
required to open the project. `plugin::Plugin` is shared with offline plugin
renders.

`LiveInstrumentDocument::identity_hash()` hashes the exact identity tuple for a
compact record and diagnostics. Changing the hint never changes identity.

## Project document

`project.json` v1 accepts an optional `live_instrument` entry with its own
`version: 1` (`project::live::LiveInstrumentDocument`):

- `plugin`: the identity tuple above.
- `name`: the display name the helper reported.
- `params`: `{ id, value }` pairs, strictly ascending by ID, exact numbers.
- `state`: the plugin's opaque saved state, base64 through `StateBytes`, bounded
  by `MAX_STATE_BYTES`.
- `location_hint`: optional machine path.
- `enabled`: whether the project asks this instrument to run.
- `takes`: zero or more retained take clips.

The field is optional because the project version stays 1; a document without
it has no live instrument, and older v1 fixtures load unchanged. Unknown fields
are rejected like every other project entry.

Each retained take is
`{ take, asset, rate, frames }`:

- `take` is the canonical decimal `TakeId`;
- `asset` names a `live_take_render` project asset: a stereo 32-bit float WAV;
- `rate` and `frames` describe the retained window.

`Document::validate` rejects unsupported versions, a non-CLAP backend, an empty
identity field, an oversized or non-canonical state, unordered or duplicate
parameters, a non-canonical or duplicate take ID, out-of-range rate/frames, and
an asset that is missing or not `live_take_render`. Validation errors carry IDs,
not machine paths.

## Session lifecycle

- Opening a project starts the host from the saved identity, hints, parameters
  and state. The output-open path mounts it at the opened stream's real sample
  rate and buffer and hands the returned `LiveVoice` to the audio callback
  (`Output::open_selected_with_live`). No session runs on the audio thread.
- A save asks the running host for its current opaque state and parameters. If
  the host cannot answer, the save is refused rather than writing stale state.
- Closing the project, clearing the instrument, or changing the output device
  or rate stops the helper. A device or rate change deliberately rebuilds the
  host and voice at the new stream's settings (D16); the state saved before the
  stop is what the rebuilt host recalls.
- One instrument per project. Choosing a plugin by absolute path and ID runs
  identity lookup off the UI thread; the panel shows the result, notices and
  errors.

## Missing or faulty plugin

A missing bundle, a changed artifact, or a helper fault never fails the project
open. The instrument is shown disabled with the reason: its full identity
(backend, ID, version, short artifact hash) plus the failure. Retained take
audio is still loaded and plays. If the project is cleared, the identity record
and `enabled: false` remain so the project still says which plugin made the
takes.

A retained take pins identity: while a project has retained takes, choosing a
different plugin identity is refused. The audio must not silently become
"played by" a different plugin.

## Retained take audio (D4)

While a pass records, the app captures every stereo block the worker rendered.
On commit it trims the capture to the pass window, encodes it as a stereo float
WAV, publishes it as project media, and records a take entry. Nothing is
re-rendered offline and no render recipe is invented for a live instrument: the
captured audio is the authoritative take, not a cache.

On reopen the take WAV is decoded into two unassigned mixer slots and replayed
as one stereo clip (`ScheduledAction::Clip`). It is project-owned media, so it
plays with no plugin, no helper and no pad assignment, and it is never mixed
into a pad meter. A save copies the bytes into `media/<sha256>.wav` like every
other asset.

## Export

A neutral bundle cannot host a live plugin. For each retained take the adapter
writes one `sample_clips` part with one event whose asset is the retained WAV.
The WAV is copied byte-for-byte.

A take's notes export too, as one more part per take: manifest v3
`live_instrument_notes`, carrying the plugin identity (backend, ID, artifact
SHA-256, ABI, version) instead of the sampled-instrument ID. Every note keeps
its take-local position: the original sample-timed onset converted from the
take's own recording tempo, with the effective duration, channel, pitch and
velocity the edit view reports. The events keep their real
`<take>:<sequence>` IDs, so the notes and the retained audio of one take join
by take identity without renaming anything. A take with no notes adds no such
part. Held notes carry the project's single instrument target, so every
instrument-target note of a live take is attributed to the live plugin here;
the take's retained audio remains the record of what actually sounded.

The bundle records a `baked_processing` loss per take naming the honest plugin
identity, stating that the retained take was played live and is not editable
plugin state another host can reconstruct, and that the take's notes live in
their own part.

A destination that cannot load the plugin reports that mapping explicitly
instead of dropping the notes: the SoundCraft adapter records an `unsupported`
mapping naming the plugin identity and the note count, and keeps the retained
take audio as the audible fallback. The notes never become sampled-instrument
notes, and they are not written into `notes.mid`: an SMF track cannot carry a
plugin identity, so a plain MIDI track would claim a sound the bundle cannot
promise. Identity is name-and-hash only — the machine path is never exported.

## First-scope limits

- One pinned bundle chosen by absolute path plus plugin ID. Installed-plugin
  discovery, scanning, and a plugin browser are out of scope.
- Parameters are recalled, not exposed as panel controls. The panel shows the
  instrument, status/reason, path and ID, choose and clear.
- The machine hint is not rewritten when the user relocates the bundle; type
  the new absolute path and choose the same identity.
- No offline render or freeze: only the captured stereo take audio and that
  take's editable notes are retained, and neither can reconstruct the plugin's
  sound without the plugin.
- The live host's own limits (one helper process, bounded queues, block
  deadlines) apply unchanged.

## Evidence

`native/src/ui/live/tests.rs` drives the real app and the real live fixture:

- save and reopen in a new `State` restores the opaque state, starts the host
  and renders identical samples for the same note;
- a recorded pass keeps the exact played stereo audio; with the bundle deleted
  the project opens, the instrument is disabled with identity in the reason,
  and the retained take plays from its own slots without a pad meter;
- the panel exposes the instrument, status reason, path/ID fields, choose and
  clear, and clearing keeps the identity and the retained takes.

`native/tests/export_bundle.rs::live_instrument_take_exports_its_recorded_audio_and_honest_identity`
checks the exported part, byte-for-byte audio, and the honest loss record.
`native/tests/export_bundle.rs::live_take_notes_export_with_the_plugin_identity_and_retained_audio`
checks the note count, positions, durations, controls and recording tempo
against a take fixture recorded at 60 BPM while the project runs at 120 BPM,
the plugin identity through a manifest round trip, the unchanged retained audio
and the absence of any machine path.
`native/tests/soundcraft_adapter.rs::unavailable_live_plugin_is_reported_and_the_retained_take_still_converts`
converts such a bundle through the strict fake target: the plugin is reported
as an unsupported mapping by identity and the retained take is imported as an
audio clip.
`native/src/project/live.rs` unit tests cover the document, validation, legacy
documents and identity-vs-hint rules. The callback allocation test
(`native/tests/rt_alloc.rs`) is unchanged because this slice adds no callback
work.

## Tests

`make native-clap-live-test` is the required lane and runs
`native/tests/clap_live.rs` and `native/src/ui/live/tests.rs` with
`BROWNNOTE_REQUIRE_CLAP=1`, so a missing helper or fixture is a failure rather
than a skip.

Real output hardware is not something a CI runner guarantees, so the one test
that opens the default output device,
`cpal_device_callback_timing_with_the_live_instrument`, is excluded by name
from both `make native-clap-live-test` and `make native-test`. It is not
counted as passed there; it does not run. Under `BROWNNOTE_REQUIRE_CLAP=1`, an
unset `BROWNNOTE_LIVE_CPAL` is a panic naming the opt-in target, so a lane that
forgets the exclusion fails loudly instead of reporting a silent pass.

Run it where audio exists:

```sh
make native-clap-live-cpal-test
```

It sets `BROWNNOTE_LIVE_CPAL=1` and runs exactly that test against the
composed callback on the default device. The device-free coverage that must
stay in every lane is
`engine_callback_with_the_live_instrument_allocates_and_frees_nothing` (zero
allocations and frees on the composed callback) and `native/tests/output_timing.rs`
(callback frames, CPAL instants and restart identity).

