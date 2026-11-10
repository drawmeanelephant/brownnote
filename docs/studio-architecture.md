# How a request flows through the studio

`scripts/studio.py` is one process: a `ThreadingHTTPServer` that serves four
in-memory pages, a whitelist of audio files, and a small JSON API. There is no
database and no background queue; everything below happens in the request that
asked for it.

```mermaid
flowchart LR
  page[browser page] -->|fetch + X-Studio-Client| gate{origin / host / CSRF gate}
  gate -->|reject| err[403 or 400 JSON]
  gate -->|allow| route[path router]
  route -->|/api/state| stems[ensure_stems]
  route -->|/api/render-*| render[renderer]
  route -->|/api/patch| patches[JSON files]
  route -->|/api/upload-sample| user[build/user]
  route -->|/api/library/*| library[build/library]
  route -->|/api/jam| jam[kujamba child]
  stems --> build[(build/tune)]
  render --> ffmpeg[ffmpeg]
  ffmpeg --> build
  route --> metrics[(counters)]
  metrics --> prom[/metrics]
```

## Request path

1. `StudioHandler._start_request()` validates `X-Request-ID` (or makes one) and
   `_blocked()` rejects a non-loopback `Host`, a foreign `Origin`/`Referer`, and
   any `/api/*` call without the client header. Rejections are JSON with the
   trace id in the response header and the log line.
2. `do_GET`/`do_POST` dispatch on the path. Unknown paths answer 404 JSON; the
   static whitelist means `/scripts/`, `/tests/`, `.git` and directory listings
   are never served.
3. A handler either reads a body through `_read_body(cap)` (one place that
   enforces the size cap and rejects chunked/ambiguous lengths), calls a
   builder/renderer, or reads fixture state under `build/`.
4. `send_response`/`end_headers` stamp `X-Request-ID`, `Nosniff` and the CSP.
   `finish()` writes one `request_done` JSON line with status and duration and
   bumps the counters behind `/metrics`.

## State on disk

| Path | Written by | Read by |
|---|---|---|
| `build/tune/*.wav` | `ensure_stems` (first `/api/state`) | every page |
| `build/user/*.wav` | `/api/upload-sample` | `/api/state` |
| `build/library/` | `/api/library/intake` | `/api/library` |
| `build/patches/*.json` | `/api/patch` | `/api/patch`, `/api/patches` |
| `build/render/` | `/api/render-*` | the page that asked for the render |
| `build/analytics/events.jsonl` | `analytics_event` when opt-in | humans |

`make clean` removes `build/`; every path above is recreated on demand, so a
clean checkout and a cleaned tree behave the same.

## Where to look when something breaks

- `/healthz` — process is up, and which feature flags are on.
- `/metrics` — request counts by route/status and uptime.
- the request log line with the same `trace` as the failing response header.
- `make studio-check` — boots the server and replays the HTTP, security and
  dynamic attack suites against it.

## CLI and native

The C CLI (`src/brownnote.c`) is a pipe: parse arguments, resolve the alias,
spawn the GuitarPedal bench, stream WAV bytes through it, write the result. Its
`BROWNNOTE_LOG`/`BROWNNOTE_PROFILE` output follows the same trace id that is
passed to the bench child. The Rust instrument (`native/src/`) owns its own
audio callback and plugin hosts and is documented in `native/README.md`.
