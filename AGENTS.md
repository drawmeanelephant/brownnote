# AGENTS.md

Instructions for autonomous agents working in this repository. Read this
before changing anything; `README.md` is the user-facing introduction and
`CONTRIBUTING.md` is the human review bar.

## What this repository is

brownnote is three cooperating things in one repository:

| Path | What it is | Language | Entry point |
|---|---|---|---|
| `.` | `brownnote` CLI: pipes audio through the GuitarPedal bench | C11 + POSIX | `src/brownnote.c` |
| `native/` | `brownnote-native`: standalone pad instrument | Rust 2024 | `native/src/main.rs` |
| `scripts/` | local browser studio and preset renderer | Python 3 | `scripts/studio.py` |

`docs/site/` is the published field manual (Boris + Zig), not an application.
`docs/studio-architecture.md` traces one studio request from gate to disk.

## First-time setup

The C CLI itself has no third-party dependencies. The real audio path needs
the upstream GuitarPedal bench:

```sh
git clone --depth 1 https://github.com/torvalds/GuitarPedal vendor/GuitarPedal
make -C vendor/GuitarPedal/Validation bench
make
```

Without the bench the suite still passes: real-bench cases print `SKIP`.
Never turn a skip into a pass by deleting the case.

Python 3 is the only requirement for the studio and the check scripts. The
native app additionally needs Rust 1.99 (edition 2024); on this Mac:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
```

All required environment variables are listed in `.env.example`. Copy it to
`.env` for local work; `.env` is gitignored and must never be committed.

## Commands

| Task | Command |
|---|---|
| Fresh clone to a built tree | `make setup` |
| Build the CLI | `make` |
| C unit tests (WAV core) | `make unit` |
| Full C suite | `make test` |
| WAV fuzz corpus under ASan/UBSan | `make fuzz` |
| Everything sanitized | `make sanitize` |
| Build the native app | `make native` |
| Native fmt + clippy + tests | `make native-test` |
| Native plugin host workspace | `make native-plugin-host` |
| Studio on http://127.0.0.1:8765/ | `make studio` |
| Studio HTTP, security and DAST | `make studio-check` |
| DAST against a running studio | `make dast STUDIO_URL=127.0.0.1:8765` |
| Render presets | `make presets` |
| Docs site | `bash docs/build.sh` |
| Format / verify formatting | `make fmt`, `make fmt-check` |
| C line-coverage gate | `make coverage` |
| Native line-coverage gate | `make native-coverage` |
| Complexity ratchet | `make complexity` |
| Duplicate-code scan | `make dup` |
| Unused-crate scan | `make unused-deps` |
| Binary size budget | `make sizes` |
| Repo hygiene checks | `make hygiene` |
| Install the Git hooks | `make hooks` |

`make test` is the required gate. Do not report success from a build alone.

## Conventions

### C

- Tabs for indentation, K&R braces, 80 columns. `.clang-format` is the
  authority: `make fmt` rewrites, `make fmt-check` verifies.
- Functions are `snake_case`. Public helpers in `src/wav.c` carry a `wav_`
  prefix so fuzz harnesses cannot collide; statics in `src/brownnote.c` stay
  unprefixed.
- Macros and compile-time constants are `UPPER_SNAKE_CASE`
  (`EXIT_USAGE`, `MAX_ARGS`).
- Structs are lowercase tags (`struct wav_info`) used without typedefs.
- Error messages start with `brownnote: ` and go to stderr through `die()`
  or the logger. Exit codes are documented in the README table; do not
  invent new ones.
- No DSP in this repository. The bench is the signal path.

### Rust

- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` are the gates;
  `unwrap`/`expect` are denied outside tests, and `unsafe` is denied.
- `clippy::cognitive_complexity` is capped at 40 in `native/clippy.toml`
  (`make complexity-rust`). Split the function; do not raise the cap.
- `cargo nextest run --profile ci` is how CI runs the tests: per-test timings
  land in `native/target/nextest/ci/junit.xml`, and the `ci` profile retries a
  failure twice so flaky tests are labelled instead of hidden.
- Modules are `snake_case` files under `native/src/`; tests live either in
  `#[cfg(test)]` modules or in `native/tests/<area>.rs`.
- The audio callback must not allocate, lock, log, or clone `Arc`.

### Python

- Standard library only, 4-space indent, no formatter dependency.
- Every HTTP route must keep the loopback/origin/CSRF gate in
  `StudioHandler._blocked` intact.

### Test files and names

- C unit tests: `tests/unit_<module>.c`; shell cases: `tests/run_tests.sh`,
  labelled `(a)`, `(b)`, ...
- Rust integration tests: `native/tests/<area>.rs`; Python checkers:
  `tests/check_<thing>.py` or `native/tests/check_<thing>.py`.
- Test names state behavior: `subject_condition_expectation`
  (for example `parse_rejects_unaligned_data`).
- Shell cases must be isolated: create your own `mktemp -d`, never write into
  another case's directory.

## Git hooks

`make hooks` points `core.hooksPath` at `.githooks/`. The pre-commit hook
runs formatting, ShellCheck, TODO-format and large-file checks on staged
files. CI runs the same checks plus everything in `make hygiene`, so a
local hook failure is a CI failure.

## Dependencies

- Pin exact versions and commit the lockfile. Rust dependencies use `=x.y.z`
  where the crate allows it, or a `rev` pin for git dependencies.
- Do not merge a dependency bump until the release is at least **7 days**
  old. Renovate/Dependabot PRs are held to that window; note the wait in the
  PR body.
- `scripts/check_versions.py` rejects version drift across the Cargo
  manifests; `make unused-deps` (cargo-machete) rejects unused crates.
- Adding a C dependency means adding it to the build and documenting it in
  the README; there are none today and that is intentional.

## Observability

- `BROWNNOTE_LOG=error|warn|info|debug` enables structured `key=value`
  logging on stderr. Log lines automatically redact values whose key looks
  sensitive (`pass`, `token`, `secret`, `key`, `cookie`, `authorization`).
- `BROWNNOTE_TRACE_ID=<id>` (or a generated id) is attached to every log
  line and passed to the bench child so one run can be followed end to end.
- `BROWNNOTE_PROFILE=1` prints a JSON phase-timing summary at exit.
- The studio logs one JSON line per request to stderr, stamps every response
  with `X-Request-ID` (echoed from the client when it is well-formed), reports
  liveness and enabled flags at `/healthz`, and exposes Prometheus counters at
  `/metrics`. Set `STUDIO_LOG_LEVEL=debug` for access lines.
- `STUDIO_ANALYTICS=1` appends local usage events to
  `build/analytics/events.jsonl` (upload, render, patch, jam). It is off by
  default, never leaves the machine, and carries no file contents or paths.
- `BROWNNOTE_FLAGS=name=on,name2=off` and `STUDIO_FEATURES=...` gate optional
  behavior. Unknown flags are rejected so typos are loud.
- Deployment: see `docs/deployment.md` for where to check the published site
  and the last deploy run.

## Security and secrets

- No credentials belong in the tree. Use environment variables locally and
  GitHub Actions secrets in CI; `.env.example` documents the names only.
- Data handling, the opt-in local usage events and log redaction are documented
  in `docs/privacy.md`. Nothing is sent off the machine.
- Run `make check-secrets` before adding config that could carry a token.
- Report vulnerabilities per `SECURITY.md`, never in a public issue.

## Definition of done

1. `make unit` and `make test` pass (bench skips are acceptable).
2. For `native/` changes: `make native-test` passes, or the exact skipped
   hardware test is named in the PR body.
3. New behavior has a test that fails without the change.
4. `make hygiene` passes.
5. Docs affected by the change are updated in the same commit.
