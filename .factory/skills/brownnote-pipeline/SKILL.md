---
name: brownnote-pipeline
description: Build, test, and extend the brownnote C CLI that pipes audio through the GuitarPedal bench. Use when changing src/, tests/unit_wav.c, tests/run_tests.sh, or the Makefile.
---

# brownnote C pipeline

Use this skill for work on the C CLI at the repository root.

## Setup that actually matters

The CLI compiles with nothing but a C compiler. The audio path needs the
upstream bench, which is not vendored:

```sh
git clone --depth 1 https://github.com/torvalds/GuitarPedal vendor/GuitarPedal
make -C vendor/GuitarPedal/Validation bench
```

Without it, the suite prints `SKIP` for the real-bench cases and still
passes. Never delete a case to get a green run.

## Verify the change

```sh
make unit          # WAV parser/resampler unit tests
make test          # unit tests + end-to-end suite
make sanitize      # ASan/UBSan rebuild + suite + fuzz corpus
make fmt-check     # clang-format check
```

`make test` is the gate. A build alone proves nothing.

## Rules of the signal path

- brownnote does no DSP. It spawns the bench, pumps samples, and writes the
  result. Do not add filters or gains here.
- Raw mode is float32 little-endian stereo at 48 kHz and must stay
  byte-identical through `/bin/cat` (case (b) in `tests/run_tests.sh`).
- WAV mode converts to the bench format and mirrors the input's rate and
  channel count on the way out. The sample conversions are asymmetric on
  purpose (`v/32768` in, `lroundf(clamp(v)*32767)` out).
- Exit codes are contractual: 0 success, 2 usage/format, 141 closed pipe,
  anything else is the bench's status. Keep the README table in sync.

## Adding behavior

1. Put parsing/validation in `src/wav.c` with a `wav_` prefix; unit-test it
   in `tests/unit_wav.c`.
2. Keep pump and process plumbing in `src/brownnote.c`. Every write must stay
   bounded and every wait must go through the poll loop; a naive
   write-then-read loop deadlocks on the bench's block buffer.
3. Add a case to `tests/run_tests.sh` that fails before the change. Use its
   own `mktemp -d` directory and clean up through the trap.
4. If a new diagnostic goes to stderr, route it through `bn_log` so
   `BROWNNOTE_LOG` and redaction apply, and keep the `brownnote: ` prefix for
   fatal errors.

## Traps found the hard way

- The bench writes stdout through a block buffer; both pipes can fill at
  once. Case (f) guards SIGPIPE, case (k) guards argv overflow.
- `BARGV_MAX` must cover `--effect` + `--pot` + `--mix` at their independent
  caps, not one list's cap.
- WAV output to a pipe cannot know its size: the header keeps
  `0xFFFFFFFF` placeholders, and a regular file is backfilled at the end.
- Do not use `timeout(1)` in tests; macOS runners do not have it.
