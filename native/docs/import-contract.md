# Sample import and runtime registry

Refs #99. First service/registry slice only, under D3 and D7. #99 remains open.
There is no import UI, picker, project document writer, or save/reopen path here.

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

## Integrator request and evidence boundary

- Add `pub mod import; pub mod assets;` to `native/src/lib.rs`. No dependency
  changes are needed. The path-bootstrap test must then be replaced by imports
  from `brownnote_native::{assets, import}` and rerun against the real manifest.
  Until then, exported-artifact verification is blocked.
- Add an `AssetRegistry` to shared `State`, initialize it from authoritative kit
  media, and make it the one source of slot ownership. Do not run the old pedal
  allocator alongside it. Route triggers through `playback(PadId)` and preview/
  label/error reads through `source`, `audible`, and `pad`.
- Wire poll retirement, pending unload retries, and output rebuild notifications
  as above. Replace the shared `FIRST_RENDER_SLOT` re-export/recovery range.
  The lane-owned pedal allocator now scans actual slot occupancy, not a fixed
  boundary; legacy scene/test names remain until shared wiring lands.
- Import UI, State import-job polling, keyboard/path/drag-and-drop interaction,
  pedal completion invalidation, and the real #100 save/reopen/relocation path
  remain integration gates. Do not infer them from service tests.

`native/tests/import_assets.rs` generates WAV fixtures and drives the real
decoder, project types, bounded queues, and mixer. It is a bootstrap test, not
proof of library exports, native interaction, hardware output, or listening.
