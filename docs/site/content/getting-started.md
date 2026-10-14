---
title: Start here
parent: index
status: published
summary: Build the bench, build brownnote, and fuzz your first fart.
---

# Make your first processed noise

brownnote is plumbing: the GuitarPedal test bench does the actual sound.
Getting to a fuzzed fart takes two clones and two makes.

## 1. Get the sources

```bash
git clone https://github.com/drawmeanelephant/brownnote.git
cd brownnote
git clone https://github.com/torvalds/GuitarPedal.git vendor/GuitarPedal
```

You need a C compiler, GNU make, and Git on macOS or Linux. No other
dependencies, no package manager.

## 2. Build the bench, then brownnote

```bash
make -C vendor/GuitarPedal/Validation bench   # builds vendor/.../bench/bench
make                                          # builds build/brownnote
```

The bench is the pedal's audio core compiled as a host program, used
unmodified. `make -C vendor/GuitarPedal/Validation check-effects` proves the
bench itself (and takes a few minutes).

## 3. Run something through it

```bash
build/brownnote --list
build/brownnote --effect fuzz < in.wav > out.wav
```

`--list` prints every routable effect and every pot with its range, straight
from the bench. `--effect` accepts [[reference/effects|aliases]] like `fuzz`,
`delay`, and `chorus`, plus exact GuitarPedal effect names; anything else
goes to the bench's own case-insensitive, unambiguous-substring matcher,
which prints the candidate list when it cannot resolve a name.

## 4. Choose your level of commitment

| You want | Next step |
|---|---|
| A fuzzed fart file | [[guides/pipeline|The pipeline]] |
| Preset pedalboards, genre songs, and the 16-step Studio UI | [[guides/studio|Presets and Studio Sequencer]] |
| Every flag and exit code | [[reference/cli|Command reference]] |
| What happens to your samples | [[reference/formats|Input modes and formats]] |

<Aside kind="tip">

brownnote finds the bench by `--bench PATH`, then `BROWNNOTE_BENCH` in the
environment, then `vendor/GuitarPedal/Validation/bench/bench` relative to the
working directory or to the brownnote executable.

</Aside>
