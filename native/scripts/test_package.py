#!/usr/bin/env python3
"""Packaging boundary tests; actual executable behavior belongs to artifact smoke."""
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import wave
from unittest import mock

sys.dont_write_bytecode = True
import package_macos as package


class PackageTests(unittest.TestCase):
    def test_shell_entry_points_explain_their_arguments(self):
        for name in ["package-macos.sh", "smoke-package.sh"]:
            result = subprocess.run([str(Path(__file__).with_name(name)), "--help"],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("usage:", result.stdout)

    def test_crate_version_names_the_artifact_beside_the_commit(self):
        version = package.crate_version()
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")
        head = "0123456789abcdef" + "0" * 24
        self.assertEqual(
            package.artifact_name(version, head),
            f"brownnote-native-macos-arm64-{version}-0123456789ab-unsigned-test.tar.gz")

    def test_dirty_working_tree_is_refused(self):
        package.require_clean("")
        with self.assertRaisesRegex(RuntimeError, r"Refusing to package a dirty working tree"):
            package.require_clean(" M native/src/main.rs\n?? scratch.txt\n")

    def test_package_refuses_a_dirty_tree_before_writing_an_artifact(self):
        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp) / "packages"
            responses = iter(["a" * 40, "?? scratch.txt\n"])
            with mock.patch.object(package, "mac_arm"), \
                    mock.patch.object(package, "output", side_effect=lambda args: next(responses)):
                with self.assertRaisesRegex(RuntimeError, "dirty working tree"):
                    package.package(destination)
            self.assertFalse(destination.exists())

    def test_commit_other_than_the_tagged_commit_is_refused(self):
        package.require_commit("a" * 40, None)
        package.require_commit("a" * 40, "a" * 40)
        with self.assertRaisesRegex(RuntimeError, "Refusing to package commit") as raised:
            package.require_commit("a" * 40, "b" * 40)
        self.assertIn("a" * 40, str(raised.exception))
        self.assertIn("b" * 40, str(raised.exception))

    def test_package_refuses_a_tag_commit_that_is_not_head(self):
        with tempfile.TemporaryDirectory() as temp:
            destination = Path(temp) / "packages"
            responses = iter(["a" * 40, ""])
            with mock.patch.object(package, "mac_arm"), \
                    mock.patch.object(package, "output", side_effect=lambda args: next(responses)):
                with self.assertRaisesRegex(RuntimeError, "the tagged commit is"):
                    package.package(destination, expected_commit="b" * 40)
            self.assertFalse(destination.exists())

    def test_release_tag_must_be_the_crate_version(self):
        package.require_tag(None, "0.1.0")
        package.require_tag("v0.1.0", "0.1.0")
        for tag in ["0.1.0", "v0.2.0", "v0.1"]:
            with self.assertRaisesRegex(RuntimeError, "Refusing tag"):
                package.require_tag(tag, "0.1.0")

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

    def test_generated_tones_pass_the_recording_boundary(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "kit"
            package.tone_kit(root)  # Raises if a generated tone matches a recording.
            recorded = package.recorded_hashes()
            self.assertTrue(recorded)
            for file in root.iterdir():
                self.assertNotIn(package.digest(file), recorded)

    def test_declaration_names_every_repository_recording(self):
        self.assertEqual(sorted(package.declared_recordings()),
                         sorted(path.name for path in package.audio_files(package.RECORDINGS)))

    def test_kit_with_a_real_recording_replaced_by_copy_is_refused(self):
        real = package.RECORDINGS / "fart_dry.wav"
        self.assertTrue(real.is_file())
        generated_tones = package.tones

        def real_recording_instead_of_tones(directory):
            generated_tones(directory)
            shutil.copy2(real, directory / "fart_dry.wav")

        with tempfile.TemporaryDirectory() as temp:
            with mock.patch.object(package, "tones", real_recording_instead_of_tones):
                with self.assertRaisesRegex(
                        package.UndeclaredRecording,
                        r"fart_dry\.wav.*recordings/fart_dry\.wav"):
                    package.tone_kit(Path(temp) / "resources/recordings")

    def test_copying_recordings_without_a_naming_declaration_is_refused(self):
        real = package.RECORDINGS / "fart_dry.wav"
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            source = temp / "recordings"
            source.mkdir()
            shutil.copy2(real, source / "fart_dry.wav")
            with self.assertRaisesRegex(package.UndeclaredRecording, "PROVENANCE.md is missing"):
                package.bundle_recordings([source / "fart_dry.wav"], temp / "unpacked", source)
            self.assertFalse((temp / "unpacked").exists())
            (source / "PROVENANCE.md").write_text("Nothing is declared here.\n")
            with self.assertRaisesRegex(package.UndeclaredRecording, "does not name it"):
                package.bundle_recordings([source / "fart_dry.wav"], temp / "unpacked", source)
            self.assertFalse((temp / "unpacked").exists())
            (source / "PROVENANCE.md").write_text("fart_dry.wav\n")
            package.bundle_recordings([source / "fart_dry.wav"], temp / "unpacked", source)
            self.assertEqual((temp / "unpacked" / "fart_dry.wav").read_bytes(),
                             real.read_bytes())

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
