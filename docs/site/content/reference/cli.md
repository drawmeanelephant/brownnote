---
title: Commands and flags
parent: reference/index
status: published
summary: Every flag, bench discovery order, and the exit-code contract.
---

# Command reference

```
brownnote [--effect NAME]... [--pot EFFECT:POT=VALUE]...
          [--mix EFFECT=VAL]... [--bench PATH] [--list]
```

Input arrives on stdin and the processed result leaves on stdout, always in
the same format it arrived. See [[reference/formats|input modes]] for what
brownnote does to each.

## Flags

| Flag | Meaning |
|---|---|
| `--effect NAME` | route an effect, in the order given (repeatable; chains) |
| `--pot E:POT=V` | set one pot, passed to the bench verbatim (repeatable) |
| `--mix E=V` | set an effect's mix 0..120 (repeatable) |
| `--bench PATH` | bench binary to run (see discovery below) |
| `--list` | list the effects and pots, straight from the bench |

`--effect` accepts [[reference/effects|aliases]] and exact GuitarPedal effect
names; anything else goes to the bench's own matcher, which is
case-insensitive, unambiguous-substring friendly, and prints the candidate
list when it cannot resolve a name.

## Bench discovery

The bench is located by, in order:

1. the `--bench` flag,
2. `BROWNNOTE_BENCH` in the environment,
3. `vendor/GuitarPedal/Validation/bench/bench` relative to the working
   directory or to the brownnote executable.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | brownnote usage or format error (bad flag, unsupported WAV) |
| other | the bench's exit status, propagated (unknown effect = 1) |
| 141 | downstream closed the pipe (128 + SIGPIPE) |
