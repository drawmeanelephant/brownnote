#!/usr/bin/env python3
"""Packaging boundary tests; actual executable behavior belongs to artifact smoke."""
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import wave

sys.dont_write_bytecode = True
import package_macos as package


class PackageTests(unittest.TestCase):
    def test_shell_entry_points_explain_their_arguments(self):
        for name in ["package-macos.sh", "smoke-package.sh"]:
            result = subprocess.run([str(Path(__file__).with_name(name)), "--help"],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("usage:", result.stdout)

    def test_generated_kit_contains_five_decodable_non_silent_tones(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "tones"
            package.tones(root)
            self.assertEqual(sorted(p.name for p in root.iterdir()), [
                "fart_chain.wav", "fart_chorus.wav", "fart_delay.wav",
                "fart_dry.wav", "fart_fuzz.wav"])
            for file in root.iterdir():
                with wave.open(str(file)) as wav:
                    self.assertEqual((wav.getnchannels(), wav.getsampwidth(),
                                      wav.getframerate(), wav.getnframes()), (1, 2, 44100, 44100))
                    self.assertNotEqual(wav.readframes(44100), bytes(88200))

    def test_extract_rejects_escape_and_symlink_before_writing(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, kind in [
                ("../escape", tarfile.REGTYPE),
                ("/absolute", tarfile.REGTYPE),
                (package.LAYOUT + "/link", tarfile.SYMTYPE),
                ("wrong-root/file", tarfile.REGTYPE),
            ]:
                archive = root / "unsafe.tar.gz"
                with tarfile.open(archive, "w:gz") as tar:
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    member.linkname = "/tmp"
                    tar.addfile(member)
                with self.assertRaisesRegex(RuntimeError, "Unsafe"):
                    package.extract(archive, root / "extract")
                self.assertFalse((root / "extract").exists())

    def test_extract_preserves_executable_and_space_containing_layout(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "safe.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo(package.LAYOUT + "/brownnote-native")
                member.mode = 0o755
                member.size = 4
                tar.addfile(member, io.BytesIO(b"test"))
            result = package.extract(archive, root / "space containing path")
            self.assertEqual((result / "brownnote-native").read_bytes(), b"test")
            self.assertTrue((result / "brownnote-native").stat().st_mode & 0o111)

    @unittest.skipUnless(package.platform.system() == "Darwin"
                         and package.platform.machine() == "arm64", "macOS arm64 boundary")
    def test_bad_checksum_fails_cli_and_records_failure_without_executing(self):
        with tempfile.TemporaryDirectory() as temp:
            artifact = Path(temp) / "damaged.tar.gz"
            artifact.write_bytes(b"not an archive")
            artifact.with_name(artifact.name + ".sha256").write_text(
                f'{"0" * 64}  {artifact.name}\n')
            result = subprocess.run([str(Path(__file__).with_name("smoke-package.sh")), str(artifact)],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            report = json.loads(artifact.with_name(artifact.name + ".smoke.json").read_text())
            self.assertEqual(report["status"], "failed")
            self.assertEqual(report["checks"], [])
            self.assertIn("checksum mismatch", report["error"])


if __name__ == "__main__":
    unittest.main()
