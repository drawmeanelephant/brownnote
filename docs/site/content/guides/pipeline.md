---
title: The pipeline
parent: guides/index
status: published
summary: fart-app renders the phrase, brownnote runs the pedal, zcn checks the result.
---

# fart → pedal → proof

fart-app renders WAV files and zcn consumes them, so with those two binaries
built ([fart-app](https://github.com/drawmeanelephant/fart-app),
[zcn](https://github.com/drawmeanelephant/zcn)):

```bash
kujamba render --phrase "kujamba karibu" --out fart.wav
build/brownnote --effect fuzz < fart.wav > fuzzed.wav
zcn check-wav fuzzed.wav --min-rms 0.01
```

A fart goes in; a fuzzed fart comes out; zcn checks the result.

## Chain effects in order

`--effect` is repeatable and routes in the order given:

```bash
build/brownnote --effect fuzz --effect delay --effect chorus \
  < fart.wav > chain.wav
```

Set pots and per-effect mix by name; `--list` prints the bench's own
inventory of effects, pots, and ranges:

```bash
build/brownnote --list
build/brownnote --effect fuzz --pot 'Rat Sketch:Distortion=60' < in.wav > out.wav
build/brownnote --effect fuzz --mix 'Rat Sketch=100' < in.wav > out.wav
```

## Live rooms

For a live room, `kujamba` and `zcn` both speak NINJAM to a server, and
brownnote's raw float32 mode is the pipe between a synth that speaks it and
a client that feeds it:

```
kujamba | brownnote --effect fuzz | zcn
```

Raw mode is the bench's own wire format — float32 stereo frames at 48 kHz,
in one end and out the other — so there is no header to parse and nothing
to convert. See [[reference/formats|input modes]] for the endianness caveat.

## When things go wrong

- An unresolvable effect name fails with the bench's own candidate list and
  its exit code, not a brownnote invention.
- A closed downstream reader surfaces as exit 141 (128 + SIGPIPE).
- Usage and format errors are exit 2. The full table is in
  [[reference/cli|the command reference]].
