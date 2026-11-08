# SoundCraft spike fixtures

These deterministic, synthetic files contain no user or third-party recordings.
They are dedicated to the public domain under CC0-1.0.
`sha256.json` records their bytes. The generator and independent readers are in
[`native/docs/spikes/soundcraft`](../../../docs/spikes/soundcraft/).

| File | Content |
|---|---|
| `impulse-48k.wav` | Mono PCM16, 48 kHz, 48,000 frames. Nonzero samples at frames 0, 12,000, and 36,000 have values 20,000, -16,000, and 12,000. |
| `tone-44k1.wav` | Mono PCM16, 44.1 kHz, 44,100 frames. Integer triangle tone, 100-frame period, 441 Hz. |
| `baked-octave.wav` | Every second tone sample, still at 44.1 kHz. Duration 0.5 seconds, frequency 882 Hz. A resampling/import fixture, not a Brownnote renderer. |
| `baked-choke.wav` | The first 2,400 impulse frames at 48 kHz. Duration 50 ms. It has only the initial transient. This does not model a general choke fade. |
| `performance.mid` | SMF format 1, 960 PPQ, three tracks: conductor, `Distinct`, and `SamePitch`. Tempo 500,000 microseconds per quarter, 4/4. |

Note tuples are `(zero-based channel, pitch, start tick, duration ticks, velocity)`.
The conductor track has no notes.

- `Distinct`: `(0, 60, 0, 960, 31)`, `(0, 64, 480, 240, 79)`,
  `(1, 67, 720, 1440, 113)`.
- `SamePitch`: `(0, 60, 0, 960, 45)`, `(0, 60, 240, 1200, 99)`.

There are five note-on and five note-off events, with release velocity 64.
The same-pitch notes end FIFO, consistent with D14. At 120 BPM and 48 kHz,
one tick is 25 samples and one quarter note is 24,000 samples.
The file does not encode Brownnote event IDs or original take times.
