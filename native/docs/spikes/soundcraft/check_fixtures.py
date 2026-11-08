#!/usr/bin/env python3
"""Independent readers: stdlib wave and mido, not the generator's codec."""
import argparse
from collections import defaultdict, deque
import hashlib
import json
from pathlib import Path
import struct
import wave

import mido


def check(root):
    for name, digest in json.loads((root / "sha256.json").read_text()).items():
        assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digest, name
    facts = {}
    for name, rate, frames in [("impulse-48k.wav", 48000, 48000),
                               ("tone-44k1.wav", 44100, 44100),
                               ("baked-octave.wav", 44100, 22050),
                               ("baked-choke.wav", 48000, 2400)]:
        with wave.open(str(root / name), "rb") as f:
            assert (f.getnchannels(), f.getsampwidth(), f.getframerate(), f.getnframes()) == (
                1, 2, rate, frames)
            values = struct.unpack(f"<{frames}h", f.readframes(frames))
        if name == "impulse-48k.wav":
            assert [(i, v) for i, v in enumerate(values) if v] == [
                (0, 20000), (12000, -16000), (36000, 12000)]
        if name == "tone-44k1.wav":
            assert sum(a < 0 <= b for a, b in zip(values, values[1:])) == 441
        if name == "baked-octave.wav":
            assert sum(a < 0 <= b for a, b in zip(values, values[1:])) == 441
        if name == "baked-choke.wav":
            assert [(i, v) for i, v in enumerate(values) if v] == [(0, 20000)]
        facts[name] = {"rate": rate, "frames": frames}
    midi = mido.MidiFile(root / "performance.mid")
    assert (midi.type, midi.ticks_per_beat, len(midi.tracks)) == (1, 960, 3)
    assert [m.tempo for m in midi.tracks[0] if m.type == "set_tempo"] == [500000]
    expected = [
        [(0, 60, 0, 960, 31), (0, 64, 480, 240, 79), (1, 67, 720, 1440, 113)],
        [(0, 60, 0, 960, 45), (0, 60, 240, 1200, 99)],
    ]
    for part, wanted in zip(midi.tracks[1:], expected):
        tick, actual, pending = 0, [], defaultdict(deque)
        for event in part:
            tick += event.time
            if event.type == "note_on" and event.velocity:
                pending[event.channel, event.note].append((tick, event.velocity))
            elif event.type in ("note_on", "note_off"):
                start, velocity = pending[event.channel, event.note].popleft()
                actual.append((event.channel, event.note, start, tick - start, velocity))
        assert not any(pending.values())
        assert sorted(actual) == sorted(wanted), (actual, wanted)
    facts["performance.mid"] = {"format": 1, "ppq": 960, "notes": 5, "tempo_us": 500000}
    return facts


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("fixtures", type=Path)
    print(json.dumps(check(parser.parse_args().fixtures), indent=2))
