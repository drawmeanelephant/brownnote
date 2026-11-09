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
The WAV is copied byte-for-byte. Notes from that take are not exported as MIDI
because the retained audio is the take.

The bundle records a `baked_processing` loss per take naming the honest plugin
identity and stating that the retained take was played live and is not editable
plugin state another host can reconstruct. The manifest format does not change.

## First-scope limits

- One pinned bundle chosen by absolute path plus plugin ID. Installed-plugin
  discovery, scanning, and a plugin browser are out of scope.
- Parameters are recalled, not exposed as panel controls. The panel shows the
  instrument, status/reason, path and ID, choose and clear.
- The machine hint is not rewritten when the user relocates the bundle; type
  the new absolute path and choose the same identity.
- No offline render, freeze, or note-level export for live takes; only the
  captured stereo take audio is retained.
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
`native/src/project/live.rs` unit tests cover the document, validation, legacy
documents and identity-vs-hint rules. The callback allocation test
(`native/tests/rt_alloc.rs`) is unchanged because this slice adds no callback
work.
