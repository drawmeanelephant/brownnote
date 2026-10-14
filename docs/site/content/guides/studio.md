---
title: Presets and Studio Sequencer
parent: guides/index
status: published
summary: Render signature pedal chains, multi-track genre songs, and jam in the four-page browser studio (sequencer, desk, pads, crates).
---

# Presets, multi-track songs, and the 16-step Studio

Beyond single-file piping, brownnote ships with a CLI preset and song
generator (`scripts/presets.sh`) and a local browser studio
(`scripts/studio.py`) with four pages over the same stems.

## Preset pedalboards (`scripts/presets.sh fx`)

`scripts/presets.sh` synthesizes Swahili phrases with `kujamba` (falling back
to a deterministic Python formant/flutter synth when `kujamba` is not
installed), routes them through the GuitarPedal bench via `brownnote`, and
writes stereo WAV or 320 kbps MP3 (when `ffmpeg` is available):

```bash
scripts/presets.sh list
scripts/presets.sh fx cathedral --phrase "harufu mbaya angani" --play
scripts/presets.sh fx cyber-laser --seed 303 --out laser.mp3
```

| Preset | Signal chain |
|---|---|
| `doom` | `Pitch (Octave=20)` → `Rat Sketch (Distortion=115, Filter=30)` → `Cabinet (Drive=90)` |
| `cathedral` | `Auto-Wah` → `Phaser` → `Tape Echo` → `Reverb (Room=115, mix=95)` |
| `machinegun` | `Compressor` → `Klonlike` → `Frenchie Amp` → `Tremolo (Rate=95, Depth=110)` |
| `swamp` | `Vibrato` → `Flanger` → `Rat Sketch (Mode=2)` |
| `klon-crunch` | `Boost` → `Klonlike` → `Frenchie Amp` → `Cabinet` → `Reverb` |
| `cyber-laser` | `Auto-Wah (Resonance=115)` → `Pitch (+1 oct)` → `Flanger` → `Tape Echo` |
| `dry` | untreated synthesis, straight from `kujamba` |

## Multi-track genre songs (`scripts/presets.sh song`)

`scripts/presets.sh song` renders five distinct stems (sub-bass, splat snare,
wah/riff, fuzz lead, and sub-bomb), sequences them across bars with stereo
panning and pitch transposition, and masters the mix to WAV or 320 kbps MP3:

```bash
scripts/presets.sh song funk --bpm 116 --bars 8 --play
scripts/presets.sh song metal --bpm 168 --bars 6 --out metal.mp3
make presets   # renders all 6 FX presets + all 4 genre songs into build/presets/
```

| Genre | Default tempo | Arrangement |
|---|---|---|
| `funk` | 116 BPM | minor-pentatonic sub-bass groove, splat backbeat, stereo Auto-Wah call-and-response, and Rat Sketch + Tape Echo solo |
| `metal` | 168 BPM | double-kick 16th-note blast, double-tracked Rat Sketch + Frenchie Amp + Cabinet palm-mute chugs, and high-register shred lead |
| `techno` | 132 BPM | four-on-the-floor sub-kick, offbeat crisp hi-hat farts, and 16th-note Auto-Wah + Flanger 303 acid sequence |
| `doom` | 66 BPM | tectonic sub-bass power chords, stereo-widened sludge riffs, slow drums, and cathedral reverb |

## Browser studio (`make studio`)

```bash
make studio
# or directly:
python3 scripts/studio.py
```

`make studio` ensures the dry and pedaled stem WAVs are built in
`build/tune/`, then serves the interactive studio on
`http://127.0.0.1:8765/` — four pages over the same six stems, linked by
the **SEQ+MIX · DESK · PADS · CRATES** nav bar at the top of each page.

### `/` — SEQ+MIX (16-step sequencer & stem mixer)

- **6-channel stem mixer**: Sub-Bass/Kick, Splat Snare, Crispy Hi-Hat,
  Auto-Wah Funk, Rat Fuzz Lead, and Brown-Note Bomb — each with volume,
  stereo pan, semitone pitch shift, **DRY** A/B toggle, mute, and solo.
- **16-step grid**: click any cell to cycle Normal (`●`) → Accent (`★`) → Off;
  `Shift+Click` transposes individual steps (`+3`, `+5`, `+7`, `+10`, `+12`,
  `-5`, `-12` semitones). Includes tempo, swing, pattern presets, and a live
  WebAudio oscilloscope.
- **Server-side MP3 bounce & jukebox**: click **BARF SEQ TO MP3** to bounce
  the active sequence and mixer state to a 320 kbps stereo MP3, or trigger new
  custom Swahili phrase and genre renders straight from the browser.

### `/mixer` — DESK (console strips)

A vertical console-desk skin over the same stems: per-channel strips with
gain (0–140%), pan (±100), pitch (−12…+18 st), mute/solo/dry, a VU meter,
**TAP** (one-shot `fireSource`) and **LOOP** (retriggers the trimmed stem),
plus a master fader, oscilloscope, and panic button.

### `/pads` — MPC (finger drummer)

A 4×4 pad grid mapped to keys `1-4` / `Q W E R` / `A S D F` / `Z X C V`.
Click height on a pad sets velocity, and a hit on a stem chokes that
stem's tail, so flams and stutters stay tight.

Below the grid sits the **Loop Station**, an event looper: arm **● REC**
and play the pads — hits are quantized to 16ths on a 1, 2, or 4-bar grid
and replayed by an `audioCtx.currentTime` lookahead scheduler. Each armed
pass becomes a take layer (OVERDUB on stacks them, off replaces them);
layers can be muted, cleared, or undone one at a time. BPM (default 116,
or `seq.bpm` from the shared `brownnote.studio.v1` state), a bars select,
and a click/metronome toggle live in the same panel, plus a ring view of
the loop. **⬇ MP3** bounces the loop through `/api/render-sequence` using
the extended `16 × bars` step grid — a lossy summary (one velocity/pitch
per stem per 16th, 3 velocity levels).

### `/library` — CRATES (rendered library)

Every MP3 and WAV in `build/presets/` and `recordings/` as a card with a
drawn waveform, a playhead, and kind filters (songs, FX presets, custom
renders, recordings).

For the underlying CLI flags, see [[reference/cli|the command reference]] and
[[reference/effects|effect aliases]].
