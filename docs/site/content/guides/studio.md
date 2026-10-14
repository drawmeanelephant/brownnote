---
title: Presets and Studio Sequencer
parent: guides/index
status: published
summary: Render signature pedal chains, multi-track genre songs, and jam in the browser 16-step sequencer and stem mixer.
---

# Presets, multi-track songs, and the 16-step Studio

Beyond single-file piping, brownnote ships with a CLI preset and song
generator (`scripts/presets.sh`) and a local browser 16-step sequencer and
6-channel stem mixer (`scripts/studio.py`).

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

## Browser 16-step sequencer & stem mixer (`make studio`)

```bash
make studio
# or directly:
python3 scripts/studio.py
```

`make studio` ensures the dry and pedaled stem WAVs are built in
`build/tune/`, then serves the interactive studio interface on
`http://127.0.0.1:8765/`:

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

For the underlying CLI flags, see [[reference/cli|the command reference]] and
[[reference/effects|effect aliases]].
