---
title: The recorded evidence
parent: guides/index
status: published
summary: The dry fart, and the same fart through fuzz, delay, chorus, and the full chain.
---

# Proof, pressed to WAV

The repository's `recordings/` directory holds the evidence: the dry fart,
and the same fart through fuzz (Rat Sketch), delay (Tape Echo), chorus
(Flanger), and the three-effect chain, plus a demonstration track with all
five in sequence.

| File | Signal chain |
|---|---|
| `fart_dry.wav` | untreated, straight from fart-app |
| `fart_fuzz.wav` | Rat Sketch |
| `fart_delay.wav` | Tape Echo |
| `fart_chorus.wav` | Flanger |
| `fart_chain.wav` | fuzz → delay → chorus |
| `demo_brownnote.wav` / `.m4a` | all of the above, in sequence |

`demo_brownnote` — a fart goes in; a distorted, delayed, chorused fart comes
out. These were rendered before the resampler's anti-aliasing filter, so
re-rendering them today would produce slightly different output.

To make your own, see [[guides/pipeline|the pipeline]].
