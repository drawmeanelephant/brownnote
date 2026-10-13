---
title: Input modes and formats
parent: reference/index
status: published
summary: Raw float32 versus WAV, and what brownnote does to your samples.
---

# What happens to your samples

brownnote peeks at stdin and picks a mode. The output mode always mirrors
the input mode.

## Raw

Float32 little-endian stereo frames at 48 kHz — the bench's own wire format,
in one end and out the other. This is the native mode of the pedal's audio
core.

The bench reads and writes native `float`, so the little-endian layout holds
on little-endian hosts only; on a big-endian host brownnote refuses raw mode
(exit 2) rather than emit a stream no little-endian consumer can read. WAV
mode converts explicitly on both sides of the bench and works on either byte
order.

## WAV

A RIFF/WAVE file: PCM s16le, mono or stereo, any rate from 8 to 192 kHz.
brownnote converts to float32 stereo 48 kHz for the bench, runs the effects,
and writes a WAV back with the *input's* rate and channel count.

- **Resampling** is Catmull-Rom, low-pass filtered on every rate drop — the
  way in for rates above 48 kHz, the way back for rates below it — so
  out-of-band energy is strongly attenuated rather than folded back.
  Transport plumbing, not an effect.
- **Mono output** takes the left channel of the processed stereo signal.
- **Sample conversion** is `v/32768` in and
  `lroundf(clamp(v,-1,1)*32767)` out — asymmetric on purpose, the common
  convention, bounded by one LSB either way.
- **Size limit:** results larger than the WAV format's 4 GiB limit fail with
  exit 2 rather than wrapping the header's 32-bit size fields.
