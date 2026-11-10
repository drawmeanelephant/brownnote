# Privacy and data handling

brownnote has no accounts, no telemetry endpoint and no server-side storage.
This page says exactly what is written to disk on the machine running it, so a
maintainer can answer "what do you collect?" with specifics.

## What is never collected

- No analytics, crash reports or usage pings leave the machine. There is no
  analytics key, DSN or endpoint in the tree; `scripts/check_secrets.py`
  rejects credential-shaped configuration.
- The studio binds to loopback by default and refuses a non-loopback
  `STUDIO_HOST` unless `STUDIO_ALLOW_REMOTE=1` is set deliberately.
- Uploaded audio, patch names and recordings are never transmitted anywhere by
  the application itself.

## What is written locally

| Data | Location | Why | Removal |
|---|---|---|---|
| Uploaded samples | `build/user/` | the studio plays them | delete the file, or POST `{name, delete:true}` |
| Library intake | `build/library/` | catalogue entries | delete the directory |
| Patches | `build/patches/` | saved instrument state | delete the file, or the studio's delete action |
| Renders | `build/render/`, `build/tune/` | audition and export | `make clean` |
| Usage events (opt-in) | `build/analytics/events.jsonl` | local feature-usage counts | delete the file or `make clean` |
| Request logs | stderr | one JSON line per request | not persisted unless redirected |

## Usage events

`STUDIO_ANALYTICS=1` turns on the only usage instrumentation. Each line is
`{"ts", "event", ...fields}` where `event` is a fixed name (`upload_sample`,
`patch_save`, `render_sequence`, `jam_join`, ...). Fields are identifiers and
counts; any key that looks sensitive (`token`, `secret`, `key`, `cookie`,
`auth`, `session`, `dsn`, `pass`) is replaced with `<redacted>`. File
contents, audio, absolute paths and query strings are not recorded.

The file is append-only for the life of the local checkout, never rotated and
never uploaded. Retention is therefore whatever the operator keeps; the
default is to delete it with `make clean`.

## Logs

`BROWNNOTE_LOG` and `STUDIO_LOG_LEVEL` emit structured lines with a trace id.
Values whose key names a credential are redacted at the logging call site
(`src/log.c`, `_sensitive()` in `scripts/studio.py`), so a token pasted into a
patch name or a request id does not land in a log line.

## Third parties

- GitHub hosts the repository, issues and Actions. CI logs are public for this
  repository; do not put personal data in a failing command.
- Dependabot reads only the dependency manifests.
- The NINJAM jam feature connects to a server the user names, when the user
  joins a session. That connection carries the audio the user chooses to
  stream and the nickname they type; it is an explicit action, off by default.

## Reporting

For a privacy or data-handling problem, follow `SECURITY.md`. Do not open a
public issue with personal data in it.
