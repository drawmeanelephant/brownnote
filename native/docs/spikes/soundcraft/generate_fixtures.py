#!/usr/bin/env python3
"""Generate synthetic, CC0 spike inputs without using the validation readers."""
import argparse
import hashlib
import json
import struct
from pathlib import Path


def wav(path, rate, samples):
    data = struct.pack(f"<{len(samples)}h", *samples)
    fmt = struct.pack("<HHIIHH", 1, 1, rate, rate * 2, 2, 16)
    path.write_bytes(b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt "
                     + struct.pack("<I", 16) + fmt + b"data"
                     + struct.pack("<I", len(data)) + data)


def vlq(n):
    out = [n & 127]
    while n >> 7:
        n >>= 7
        out.insert(0, (n & 127) | 128)
    return bytes(out)


def track(events):
    data, last = bytearray(), 0
    for tick, message in sorted(events, key=lambda e: e[0]):
        data.extend(vlq(tick - last) + message)
        last = tick
    data.extend(b"\0\xff\x2f\0")
    return b"MTrk" + struct.pack(">I", len(data)) + data


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    impulse = [0] * 48000
    for frame, value in [(0, 20000), (12000, -16000), (36000, 12000)]:
        impulse[frame] = value
    wav(args.output / "impulse-48k.wav", 48000, impulse)
    # Integer triangle, 441 Hz, one second. No platform-dependent sin()/round().
    tone = [((i % 100) if i % 100 < 50 else 100 - i % 100) * 400 - 10000
            for i in range(44100)]
    wav(args.output / "tone-44k1.wav", 44100, tone)
    wav(args.output / "baked-octave.wav", 44100, tone[::2])
    choked = impulse[:2400]
    wav(args.output / "baked-choke.wav", 48000, choked)
    conductor = track([(0, b"\xff\x51\x03\x07\xa1\x20"),
                       (0, b"\xff\x58\x04\x04\x02\x18\x08")])
    parts = [
        ("Distinct", [(0, 60, 0, 960, 31), (0, 64, 480, 240, 79),
                      (1, 67, 720, 1440, 113)]),
        ("SamePitch", [(0, 60, 0, 960, 45), (0, 60, 240, 1200, 99)]),
    ]
    tracks = [conductor]
    for name, notes in parts:
        events = [(0, b"\xff\x03" + bytes([len(name)]) + name.encode())]
        for channel, pitch, start, length, velocity in notes:
            events.extend([(start, bytes([0x90 | channel, pitch, velocity])),
                           (start + length, bytes([0x80 | channel, pitch, 64]))])
        tracks.append(track(events))
    (args.output / "performance.mid").write_bytes(
        b"MThd" + struct.pack(">IHHH", 6, 1, len(tracks), 960) + b"".join(tracks))
    hashes = {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sorted(args.output.iterdir()) if p.suffix in (".wav", ".mid")}
    (args.output / "sha256.json").write_text(json.dumps(hashes, indent=2) + "\n")


if __name__ == "__main__":
    main()
