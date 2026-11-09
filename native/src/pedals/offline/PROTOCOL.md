# Offline plugin helper protocol, version 1

A plugin helper is an executable that hosts **one plugin instance** for one
chain stage of an offline render. The renderer
([`runner.rs`](runner.rs)) starts one helper per stage, talks to it over
stdin and stdout, and kills it when the stage ends, fails, times out or is
cancelled. Helpers for every backend (CLAP, LADSPA) speak this protocol, so
block scheduling, latency compensation, tail measurement, finite-output
checks, deadlines and cancellation are implemented once, in the renderer.

The helper never decides how long to render. It processes the blocks it is
sent.

## Framing

Every message is one frame:

| Bytes | Content |
| --- | --- |
| 1 | tag (ASCII) |
| 4 | payload length, unsigned little-endian, at most 32 MiB |
| n | payload |

Audio is interleaved IEEE-754 `f32`, little-endian. JSON is UTF-8 and
rejects unknown fields. A helper writes nothing else to stdout; diagnostics
go to stderr (the renderer keeps the first 16 KiB for error reports).

## Renderer to helper

| Tag | Payload |
| --- | --- |
| `O` open | `u32` JSON length, the JSON below, then the raw state bytes if `state` is true |
| `P` process | `u32` frame count, then `frames * channels` samples |
| `E` end | empty |

```json
{"protocol": 1, "backend": "clap", "plugin_id": "com.example.verb",
 "location": "/path/to/plugin", "rate": 48000, "channels": 2,
 "max_block_frames": 512, "params": [{"id": 3, "value": 0.25}], "state": false}
```

On `O` the helper loads the plugin at `location`, creates `plugin_id`,
applies the state blob (if any), then each parameter (state first, so
parameters win), configures `channels` in and out at `rate` with blocks of
at most `max_block_frames`, selects **offline** processing, and activates.

## Helper to renderer

| Tag | When | Payload |
| --- | --- | --- |
| `R` ready | once, after a successful open | JSON below |
| `B` block | once per `P`, in order | exactly `frames * channels` samples |
| `D` done | once, after `E` | `0` for no state, or `1` followed by the state blob |
| `X` error | instead of any reply | `{"code": "...", "message": "..."}` |

```json
{"protocol": 1,
 "plugin": {"backend": "clap", "id": "com.example.verb",
            "artifact": "<sha256 of the loaded binary>",
            "abi": "clap 1.2.2", "version": "1.0.3"},
 "name": "Example Verb", "latency_frames": 0, "tail_frames": 96000,
 "params": [{"id": 3, "name": "Mix", "min": 0.0, "max": 1.0,
             "default": 0.5, "stepped": false}]}
```

- `artifact` hashes the file the helper actually loaded (for a CLAP bundle,
  its executable). The renderer compares the whole `plugin` object with the
  request and fails with `ArtifactChanged` on any difference.
- `tail_frames` is `null` when the plugin declares nothing and
  `4294967295` for infinite. It is reported, never trusted.
- After `D` the helper deactivates, destroys the plugin and exits 0.
- After `X` the helper exits non-zero.

### Error codes during open

The renderer maps `unsupported` to `Unsupported` (for example a plugin that
requires hard realtime processing, or cannot do the channel layout). Every
other code is `Unavailable` and names why the plugin cannot be used. Use
these where they apply: `bundle-missing`, `dlopen-failed`, `no-entry`,
`unknown-plugin-id`, `init-failed`, `activate-failed`, `state-rejected`,
`param-rejected`.

Errors after open map to `Failed`.

## What the renderer enforces

- Deadlines for open, each block, close, and the whole render. A late helper
  is killed (`SIGKILL`) and the render fails with `Timeout`.
- A crash or early exit is reported with its exit status and stderr.
- Every returned block must have the requested length and only finite
  samples; otherwise the render fails. Failed renders return no audio:
  callers keep the sound they already had.
- Cancellation kills the helper within one 10 ms poll interval.

## Test fixture

[`native/tests/fixtures/offline_render/fake_helper.py`](../../../tests/fixtures/offline_render/fake_helper.py)
implements this protocol with scripted fake plugins (gain, echo, latency,
endless drone, crash, hang, NaN, protocol violations). New helpers can copy
its structure for their own fault fixtures.
