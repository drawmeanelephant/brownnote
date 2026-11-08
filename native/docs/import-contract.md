# Sample import and runtime registry

Refs #99. First service/registry slice only, under D3 and D7. #99 remains open.
There is no import UI, picker, or application save/reopen UI here. Adoption
uses the existing project store, never a second document writer.

## Import boundary

`ImportService::new(Arc<dyn MediaPublisher>)` creates one worker with one waiting
request. `with_reader(publisher, ReadFile)` injects the file reader.
`import_sample(Some(path))` returns an `ImportJob`, or `StartError::Busy` without
changing an existing job. `None` is cancellation and does not queue I/O.
`ImportJob::take()` never blocks. A successful result contains the real project
`Asset` (including `AssetId` and byte length), decoded engine `Sample`, normalized
waveform, and display label.

Read, decode, hash, waveform generation, copy and sync all run on the worker,
not the UI or audio callback. Hash/decode/copy use the same owned encoded
snapshot. The shared kit decoder retains PCM16/float32, mono/stereo, 8–192 kHz,
and 1 frame through 30 seconds. There is no browser-style 10 MiB limit.
The default reader accepts regular files and checks cancellation between
64 KiB reads. A read already in progress, decoding, hashing, or a publisher
call cannot be interrupted. Cancellation is checked between stages and again
at completion. Callers must cancel the registry replacement ticket too.

The storage owner can implement:

```rust
trait MediaPublisher: Send + Sync {
    fn publish(&self, reference: &AssetRef, bytes: &[u8]) -> std::io::Result<()>;
}
```

The publisher must validate the reference against these bytes, publish them as
project-owned `media/<id>.wav`, verify any existing object, and never replace
an existing object. It must sync the file and containing directories before
returning success. It does not update a document. `ProjectMedia` supplies this
immutable-file boundary using a synced temporary file and a create-new hard
link. Concurrent identical publications are safe. Corrupt existing objects and
symlink objects fail without overwriting them.

The project root is private to the storage owner. External mutation of its
directory hierarchy during publication is not supported; #100 must coordinate
that boundary with its transactions. Cancellation or a later assignment/save
failure can leave unreferenced immutable media. Do not delete it from an import
callback, since another pad or transaction may reference it. Store-owned
collection must account for saved versions and recovery. Media is not cache.

### Unsaved application ownership

The real app starts with an explicit `Arc<SessionMedia>` owner. This
`MediaPublisher` keeps exact encoded bytes in memory until the unsaved session
ends. It is not durable, never participates in pedal-cache eviction, and makes
no saved-project claim. Kit sources are captured into this owner at startup;
imports and required retained renders publish into it on their workers.

Before a future save can succeed, the storage worker calls
`SessionMedia::adopt(&dyn MediaPublisher)` with the real store owner's
coordinated immutable publisher. Adoption snapshots owned byte references and
publishes them without holding the session lock during file I/O. It is safe to
retry after partial publication. With `ProjectMedia` publishing into the same
private destination, `Store::save(document, &[])` then verifies the published
media. A different destination can use the existing `MediaSource` copy
contract. Adoption writes no document and is not a successful save.

This first slice retains all published session objects until session end,
including cancelled/unreferenced results. Store-owned collection, history,
recovery and save coordination remain later application work.

## Runtime boundary

`AssetRegistry` is the sole allocator of its `Slots`. It maps stable `AssetId`
values to `RuntimeAsset` (slot, sample, waveform, label), and `PadId` values to
the real `SampleAssignment`, an explicit `ChokeIdentity`, and an optional error.
An engine group is allocated independently from any reusable slot.

1. Call `begin(pad)` before starting import/render work. A newer ticket for that
   pad makes older completions stale; another pad's tickets are independent.
2. Pass the completion to `replace_source(ticket, &imported, choke, send)`.
   `send` must use the real output's boolean admission result. The registry
   checks the ticket, selects a truly free slot, and admits `Load` before
   changing the pad. Full slots/queues leave the pad and every other pad intact.
   A failed admission can retry the same ticket; a successful one consumes it.
3. Already registered assets share a slot. Both source and retained references
   keep that slot live. Replacement retires only unreferenced old source/render
   assets. Already-playing voices continue reading their old slots.
4. Poll `flush_unloads(send)` and route each `Output::take_retired()` item to
   `returned(item)`. A full queue keeps an unload pending; an admitted unload
   is not sent twice. A slot is reusable only after its matching retirement.
5. `cancel(ticket)` prevents assignment of a late completion.
   `report_error(ticket, message)` keeps the current sound usable.

`replace_retained` requires project-owned `MediaKind::RetainedRender` media;
it preserves the source and choke identity. `reset_to_source` invalidates
pending work and releases only unreferenced retained media. Pedal cache-only
results must be published before calling this API (D4).

`new(existing_slots)` reserves all occupied slots. Bootstrap them with
`register_existing(slot, imported)` and `bind_existing(pad, assignment, choke)`.
Metadata must be computed from authoritative encoded media, not decoded floats.
Unknown occupied slots remain reserved, never silently overwritten.

For output recovery, create the fresh Output from `registry.slots().clone()`,
discard the old Output and its return queue, then call `output_rebuilt()` and
`flush_unloads` on the new output. Fresh streams stop voices (D16); previously
admitted unloads need admission to the new queue.

## Mounted application

- `brownnote_native::{assets, import}` are exported. `import_assets` imports the
  real library, with no path bootstrap.
- `Kit::load` reads each file once. `Kit::from_encoded` hashes, decodes and
  previews each owned snapshot. `State::new` and `with_cache` automatically
  register this authoritative metadata, deduplicating identical encoded media.
  The legacy decoded kit view is not an alternative source of asset identity.
  Kit choke groups preserve the existing per-kit-file behavior, even if two
  filenames contain identical encoded bytes and share a slot. Imports use
  explicit content-asset choke identity, independently of slot reuse.
- `State` owns one registry. Triggers use its playback/choke mapping; duration,
  audible/source waveform, filename, error and assignment reads use its getters.
  A failed replacement reports an error without disabling a playable old pad.
- `State::import_sample(pad_index, Some(path))` queues service work and a registry
  ticket. `None` or `cancel_import` cancels it. Queue/slot refusal retains the
  prepared result for retry without changing the current assignment. Busy
  service admission leaves previous work alone. Polling drives completion;
  setup's waker starts polling when a request is queued.
- Pedal edits and imports invalidate each other's tickets. Pedal workers
  encode/hash/publish required float32 WAV media before admission, even on a
  cache hit. No render-only allocator, slot boundary or parallel waveform
  ownership remains. An edit keeps the previously audible sound until its new
  render is admitted; removing the last stage returns to the assigned source.
- Poll routes retirement through `returned`, retries pending unloads and keeps
  waking while retirement is outstanding. Every fresh output, including device
  UI stop/open, retry, silent recovery and scenes, uses the registry snapshot
  and calls `output_rebuilt` after discarding the previous output.

`native/tests/import_assets.rs` generates WAV fixtures and drives the real
decoder, project types, bounded queues, and mixer. Mounted tests in
`ui_import_tests.rs`, `ui_pedals_tests.rs` and `ui/devices/tests.rs` drive the
actual State/Runtime, output queues, pointer/keyboard callers and worker
completions. Encoded fixtures use valid WAV bytes, not fabricated asset IDs.

## Remaining issue gates

The second #99 slice supplies the keyboard-accessible typed-path/drag-and-drop
UI. The actual application save/reopen/relocation flow, history/recovery and
store-owner coordination still belong to #100 and later integration. Tests
of session adoption through the real store do not claim that save UI exists.
Listening and physical-device/controller evidence are not supplied by these
headless/mixer tests. #99 remains open.
