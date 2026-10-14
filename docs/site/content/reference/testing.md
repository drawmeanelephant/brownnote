---
title: Testing
parent: reference/index
status: published
summary: The unit tests, the shell suite, and what skips cleanly.
---

# Prove the plumbing

```bash
make test
# or, explicitly:
BENCH=vendor/GuitarPedal/Validation/bench/bench tests/run_tests.sh
```

`make test` builds and runs the unit tests for the WAV parser, the resampler,
and the sample conversions, then the suite below. `make unit` runs just the
unit tests; `tests/run_tests.sh` alone is the suite, without them.

## What the suite covers

- build
- raw-mode plumbing identity against `/bin/cat` (byte-identical)
- fuzz through the real bench (output changes, sane RMS)
- unknown-effect error propagation (the bench's candidate list, and its
  exit code)
- chaining
- SIGPIPE against a closed downstream reader
- WAV passthrough (header and level preserved)
- WAV fuzz
- `--list`
- streaming WAV headers, 30 MB bounded-memory streaming, and anti-aliasing decimation
- README alias-table drift against `src/brownnote.c` and the bench's `--list`
- `scripts/presets.sh` single-shot FX and multi-track song rendering
- when the Zig tools happen to be around, the full
  kujamba → brownnote → zcn round trip

Real-bench and round-trip tests skip cleanly — and the suite still passes —
when their binaries are absent, which is what CI relies on.
