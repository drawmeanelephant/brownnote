---
title: Effect aliases
parent: reference/index
status: published
summary: Short names for the pedal's own effect names.
---

# Effect aliases

`--effect` accepts these aliases. Exact bench names always win over aliases,
and `--list` shows the bench's truth: every routable effect and every pot
with its range.

| Alias | Effect |
|---|---|
| `fuzz`, `distortion` | Rat Sketch |
| `overdrive` | Klonlike |
| `boost` | Boost |
| `wah` | Auto-Wah |
| `compressor` | Compressor |
| `delay`, `echo` | Tape Echo |
| `reverb` | Reverb |
| `chorus`, `flanger` | Flanger |
| `phaser` | Phaser |
| `tremolo` | Tremolo |
| `vibrato` | Vibrato |
| `pitch` | Pitch |
| `eq` | Parametric EQ |
| `cabinet` | Cabinet |
| `amp` | Frenchie Amp |
| `preamp` | Preamp |
| `tone` | Tone 1 |

Anything that matches neither an alias nor an exact name goes to the bench's
own matcher: case-insensitive, friendly to unambiguous substrings, and it
prints the candidate list when it cannot resolve a name.
