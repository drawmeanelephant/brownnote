#!/usr/bin/env python3
"""Check deterministic generation and reject independently corrupted semantics."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import mido

from check_fixtures import check


ROOT = Path(__file__).resolve().parent


class Fixtures(unittest.TestCase):
    def setUp(self):
        (ROOT / ".work").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=ROOT / ".work")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.generate()

    def generate(self):
        subprocess.run([sys.executable, str(ROOT / "generate_fixtures.py"), str(self.path)],
                       check=True)

    def rehash(self, name):
        path = self.path / "sha256.json"
        hashes = json.loads(path.read_text())
        hashes[name] = hashlib.sha256((self.path / name).read_bytes()).hexdigest()
        path.write_text(json.dumps(hashes))

    def test_deterministic_bytes_and_independent_readers(self):
        before = {p.name: p.read_bytes() for p in self.path.iterdir()}
        self.generate()
        self.assertEqual(before, {p.name: p.read_bytes() for p in self.path.iterdir()})
        self.assertEqual(check(self.path)["performance.mid"]["notes"], 5)

    def test_moved_transient_is_rejected_even_with_updated_hash(self):
        path = self.path / "impulse-48k.wav"
        data = bytearray(path.read_bytes())
        data[44:48] = data[46:48] + data[44:46]
        path.write_bytes(data)
        self.rehash(path.name)
        with self.assertRaises(AssertionError):
            check(self.path)

    def test_wrong_velocity_is_rejected_even_with_updated_hash(self):
        path = self.path / "performance.mid"
        midi = mido.MidiFile(path)
        event = next(e for e in midi.tracks[1] if e.type == "note_on")
        event.velocity = 32
        midi.save(path)
        self.rehash(path.name)
        with self.assertRaises(AssertionError):
            check(self.path)

    def test_wrong_duration_is_rejected_even_with_updated_hash(self):
        path = self.path / "performance.mid"
        midi = mido.MidiFile(path)
        event = next(e for e in midi.tracks[1] if e.type == "note_off")
        event.time += 1
        midi.save(path)
        self.rehash(path.name)
        with self.assertRaises(AssertionError):
            check(self.path)


if __name__ == "__main__":
    unittest.main()
