#!/usr/bin/env python3
"""Exercise the docs builder's toolchain gate without fetching or building Boris."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class DocsToolchainTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="brownnote-docs-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for relative in (
            ".zigversion",
            "docs/build.sh",
            "docs/boris-pin.txt",
            "docs/site/boris.json",
        ):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, destination)
        self.expected = (self.root / ".zigversion").read_text().strip()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        zig = self.bin / "zig"
        zig.write_text(
            '#!/usr/bin/env bash\n'
            '[[ "$#" == 1 && "$1" == version ]] || exit 99\n'
            'printf "%s\\n" "$TEST_ZIG_VERSION"\n'
        )
        zig.chmod(0o755)

    def build(self, version):
        environment = {
            **os.environ,
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
            "TEST_ZIG_VERSION": version,
            "BORIS_CHECKOUT": str(self.root / "missing-boris"),
        }
        result = subprocess.run(
            ["bash", str(self.root / "docs/build.sh")],
            cwd=self.root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertFalse((self.root / "missing-boris").exists())
        self.assertFalse((self.root / "zig-cache").exists())
        return result.stderr

    def test_pinned_compiler_passes_the_version_gate(self):
        self.assertIn("external Boris checkout is missing", self.build(self.expected))

    def test_unsupported_compilers_stop_before_checkout(self):
        for version in ("0.16.0", "0.18.0-dev.1"):
            with self.subTest(version=version):
                message = self.build(version)
                self.assertIn(f"Zig {self.expected} is required", message)
                self.assertNotIn("external Boris checkout is missing", message)

    def test_version_file_controls_the_required_compiler(self):
        (self.root / ".zigversion").write_text("0.17.2\n")
        self.assertIn("external Boris checkout is missing", self.build("0.17.2"))
        self.assertIn("Zig 0.17.2 is required", self.build(self.expected))

    def test_malformed_pin_is_rejected(self):
        (self.root / ".zigversion").write_text("0.17.1; echo unexpected\n")
        self.assertIn("invalid Zig version pin", self.build(self.expected))


if __name__ == "__main__":
    unittest.main()
