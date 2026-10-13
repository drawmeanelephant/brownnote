#!/usr/bin/env python3
"""Regenerate the fuzz seed corpus in tests/corpus/.

Each file targets a different parser or resampler path; all are tiny.
Run from the repo root:  python3 tests/gen_corpus.py
"""

import math
import os
import struct

OUT = os.path.join(os.path.dirname(__file__), "corpus")


def chunk(cid, body):
    out = cid + struct.pack("<I", len(body)) + body
    if len(body) & 1:
        out += b"\0"
    return out


def fmt(format_=1, channels=1, rate=44100, bits=16):
    return chunk(b"fmt ", struct.pack(
        "<HHIIHH", format_, channels, rate, rate * channels * 2,
        channels * 2, bits))


def wav(*chunks_):
    body = b"WAVE" + b"".join(chunks_)
    return b"RIFF" + struct.pack("<I", len(body)) + body


def tone(frames, rate, freq=440.0):
    return b"".join(
        struct.pack("<h", int(9000 * math.sin(2 * math.pi * freq * i / rate)))
        for i in range(frames))


def stereo_tone(frames, rate):
    return b"".join(
        struct.pack("<hh", int(9000 * math.sin(2 * math.pi * 440 * i / rate)),
                    int(5000 * math.sin(2 * math.pi * 330 * i / rate)))
        for i in range(frames))


def write(name, data):
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(data)
    print(f"corpus/{name}: {len(data)} bytes")


os.makedirs(OUT, exist_ok=True)

# a well-formed file that reaches every stage: parse, convert,
# decimate (192 kHz), upsample, passthrough
write("valid-mono.wav", wav(fmt(1, 1, 44100),
                          chunk(b"data", tone(400, 44100))))
write("valid-stereo-192k.wav",
      wav(fmt(1, 2, 192000), chunk(b"data", stereo_tone(384, 192000))))

# structure edge cases
write("empty.bin", b"")
write("truncated-header.bin", b"RIFF\x0f\x00")
write("riff-only.bin", b"RIFF" + struct.pack("<I", 4) + b"WAVE")
write("fmt-only.wav", wav(fmt(1, 1, 44100)))

# data chunk declaring 0xFFFFFFFF (streaming writers' convention)
write("data-huge-decl.wav",
      b"RIFF" + struct.pack("<I", 0xFFFFFFFF) + b"WAVE" +
      fmt(1, 1, 44100) + b"data" + struct.pack("<I", 0xFFFFFFFF) +
      tone(64, 44100))

# an odd-sized LIST before data (pad byte must not become a header)
write("odd-list.wav", wav(fmt(1, 1, 44100), chunk(b"LIST", b"INFOj"),
                          chunk(b"data", tone(32, 44100))))

# rejected format fields
write("float-fmt.wav", wav(fmt(3, 1, 44100), chunk(b"data", tone(32, 44100))))
write("bits-24.wav", wav(fmt(1, 1, 44100, 24), chunk(b"data", b"\0" * 96)))
write("ch-0.wav", wav(fmt(1, 0, 44100), chunk(b"data", tone(32, 44100))))
write("ch-3.wav", wav(fmt(1, 3, 44100), chunk(b"data", b"\0" * 192)))
write("rate-low.wav", wav(fmt(1, 1, 7999), chunk(b"data", tone(32, 7999))))
write("rate-high.wav", wav(fmt(1, 1, 192001), chunk(b"data", tone(32, 44100))))

# 3 bytes of mono audio: not frame-aligned
write("unaligned-data.wav", wav(fmt(1, 1, 44100), chunk(b"data", b"\x01\x02\x03")))

# data before fmt (batch accepts, streaming rejects — both must not crash)
write("data-before-fmt.wav", wav(chunk(b"data", tone(32, 44100)), fmt(1, 1, 44100)))

# two data chunks (batch last-wins, streaming first-wins)
write("two-data.wav", wav(fmt(1, 1, 44100), chunk(b"data", tone(16, 44100)),
                          chunk(b"data", tone(32, 44100))))

# a fmt chunk with extra payload, then data
write("fmt-extra.wav", b"RIFF" + struct.pack("<I", 0) + b"WAVE" +
      b"fmt " + struct.pack("<I", 18) +
      struct.pack("<HHIIHH", 1, 1, 48000, 192000, 4, 16) + b"\x07\x08" +
      chunk(b"data", tone(32, 48000)))

# an extreme declared chunk size that runs past EOF mid-walk
write("huge-list-decl.wav", wav(fmt(1, 1, 44100), b"LIST" +
                              struct.pack("<I", 0xFFFFFFFE) + b"tiny",
                              chunk(b"data", tone(16, 44100))))
