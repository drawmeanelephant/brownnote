#!/usr/bin/env python3
"""Exercise the docs builder's toolchain gate without fetching or building Boris."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


def shipped_feature_bullets(markdown):
    """Return the bullets under the roadmap's "What exists today" heading."""
    heading = re.search(r"^## What exists today[ \t]*$", markdown, re.MULTILINE)
    if heading is None:
        raise AssertionError("roadmap has no '## What exists today' section")
    rest = markdown[heading.end():]
    following = re.search(r"^## ", rest, re.MULTILINE)
    section = rest[: following.start()] if following else rest
    bullets = []
    current = None
    for line in section.splitlines():
        if line.startswith("- "):
            if current is not None:
                bullets.append(current)
            current = line
        elif current is not None and line.startswith("  ") and line.strip():
            current = current + " " + line.strip()
        elif current is not None:
            bullets.append(current)
            current = None
    if current is not None:
        bullets.append(current)
    return bullets


def uncited_bullets(bullets):
    """Return bullets that do not name a #<number> issue or PR."""
    return [bullet for bullet in bullets if not re.search(r"#\d+", bullet)]


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

    def test_shipped_features_cite_a_tracking_reference(self):
        page = (ROOT / "docs/site/content/roadmap.md").read_text()
        bullets = shipped_feature_bullets(page)
        self.assertTrue(bullets, "What exists today must list shipped features")
        self.assertEqual(
            [],
            uncited_bullets(bullets),
            "every What exists today bullet must cite the issue or PR that delivered it",
        )

    def test_uncited_shipped_feature_fails(self):
        markdown = (
            "## What exists today\n\n"
            "- Pads play samples.\n"
            "- Takes record and overdub (#106).\n"
            "\n## Still open\n\n"
            "- Something else (#101).\n"
        )
        self.assertEqual(
            ["- Pads play samples."],
            uncited_bullets(shipped_feature_bullets(markdown)),
        )


if __name__ == "__main__":
    unittest.main()
