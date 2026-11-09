# Live instrument helper protocol

The wire format between the native app's live worker
(`native/src/plugins/live.rs`, app side in `live_protocol.rs`) and the live
CLAP instrument helper (`native/plugin-host/host/src/clap_live.rs`, binary
`brownnote-clap-live`). Version 1.

This is a separate protocol from the offline render protocol
(`native/src/pedals/offline/PROTOCOL.md`). The offline one streams audio in
both directions for bounded batch renders; this one streams **events in,
audio out**, one fixed-size block per exchange, forever, for a live
instrument. Both share the open/ready handshake shape and the error frame.

The helper process is never the audio callback. Only the app's live worker
thread speaks this protocol, and every wait it makes has a deadline; the
audio callback only reads blocks the worker has already received.

## Framing

Every message is one byte tag, a little-endian u32 payload length (at most
32 MiB), then the payload. App to helper on the helper's stdin, helper to
app on its stdout. The helper redirects its own fd 1 to stderr and keeps a
private copy of the real stdout, so plugin prints cannot corrupt a frame.
If stdin closes early the helper exits 0 without a reply.

## Session

1. App sends `O` (open). Payload: u32 JSON header length, then the JSON
   header (the same shape as the offline protocol's `open`: `protocol`
   (must be 1 here), `backend` (`"clap"`), `plugin_id`, `location`, `rate`,
   `channels`, `max_block_frames`, `params`, `state` flag), then the opaque
   state bytes if the flag is set. State is applied first, then parameters.
2. Helper replies `R` (ready) with a JSON `PluginInfo` (identity, name,
   latency, declared tail, parameter descriptors), or `X` (error) with a
   JSON `{code, message}` and exits 3. Load failures use the same codes as
   the offline helper (`bundle-missing`, `dlopen-failed`, `no-clap-entry`,
   `unknown-plugin-id`, `init-failed`, `activate-failed`, `param-rejected`,
   `state-rejected`, `unsupported`). A plugin without a note input port
   that accepts the CLAP note dialect is `unsupported`, as is one whose
   main output is neither the requested channel count nor mono.
3. App sends any number of `P` (process) messages. Payload: u32 frame
   count (1..=`max_block_frames`), then input events, each exactly
   24 bytes:

   | Offset | Type | Meaning |
   |---|---|---|
   | 0 | u32 | `time`: frame offset within the block, nondecreasing |
   | 4 | u8 | kind: 0 note-on, 1 note-off, 2 parameter |
   | 5 | u8 | reserved (0) |
   | 6 | i16 | channel (note events) |
   | 8 | i16 | key (note events) |
   | 10 | u16 | reserved (0) |
   | 12 | u32 | note ID (notes) or parameter ID (params) |
   | 16 | f64 | velocity 0..=1 (note-on) or parameter value |

   The helper replies to each `P` with `L` (block): u32 frames, u32
   note-end count, then `frames * channels` little-endian f32 samples
   (interleaved; a mono instrument feeds every channel), then that many
   note ends of 12 bytes each (u32 time, i32 note ID, i16 channel,
   i16 key): the `CLAP_EVENT_NOTE_END`s the plugin emitted during the
   block. A typed failure is `X` instead; after sending `X` the helper
   exits 3.

4. App may send `S` (save state) between blocks; the helper replies `T`
   (state): one byte flag, then the state bytes if the flag is 1.
5. App sends `E` (end) to finish. The helper stops processing, saves state,
   deactivates, replies `D` (done, same payload shape as `T`) and exits 0.
