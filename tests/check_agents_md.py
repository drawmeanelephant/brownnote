#!/usr/bin/env python3
"""Validate that AGENTS.md still describes this repository.

Checks, so agent instructions cannot rot silently:
  * AGENTS.md exists, is non-trivial, and has the required sections;
  * every relative Markdown link resolves;
  * every `make <target>` it mentions exists in the Makefile;
  * every backticked path it names exists on disk.

CI runs this on every PR.  A broken command in AGENTS.md is a broken agent.

Usage: tests/check_agents_md.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
AGENTS = ROOT / "AGENTS.md"
SECTIONS = ["First-time setup", "Commands", "Conventions", "Definition of done"]

LINK = re.compile(r"\[[^\]]+\]\(([^)]+)\)")
MAKE = re.compile(r"\bmake ([A-Za-z0-9_.-]+)")
TICK = re.compile(r"`([^`\n]+)`")
FENCED = re.compile(r"```.*?```", re.S)

SKIP_PREFIXES = (
    "build/", "vendor/", "native/target", "zig-cache", "core.",
    "BROWNNOTE_", "STUDIO_", ".env", "0x", "TODO(", "http",
)
SKIP_CONTAINS = ("<", ">", "...", "$", "|", "=", "{", "}")
FILE_SUFFIXES = {
    "md", "py", "c", "h", "rs", "toml", "yml", "yaml", "json", "sh",
    "cfg", "lock", "txt", "wav", "mp3", "gz", "env", "ini",
}


def make_targets() -> set[str]:
    text = (ROOT / "Makefile").read_text()
    targets: set[str] = set()
    for line in text.splitlines():
        m = re.match(r"^([A-Za-z0-9_.-]+):", line)
        if m:
            targets.add(m.group(1))
    return targets


def main() -> int:
    if not AGENTS.exists():
        print("check_agents_md: AGENTS.md is missing", file=sys.stderr)
        return 1
    text = AGENTS.read_text()
    errors: list[str] = []

    if len(text) < 100:
        errors.append("AGENTS.md is empty or trivial")
    for section in SECTIONS:
        if section.lower() not in text.lower():
            errors.append(f"missing section: {section}")

    for target in sorted(set(MAKE.findall(text))):
        if target.startswith("-"):
            continue
        if target not in make_targets():
            errors.append(f"`make {target}` is not a Makefile target")

    for link in LINK.findall(text):
        if link.startswith(("http://", "https://", "#", "mailto:")):
            continue
        if not (ROOT / link.split("#")[0]).exists():
            errors.append(f"link target does not exist: {link}")

    inline = FENCED.sub("", text)
    for token in TICK.findall(inline):
        token = token.strip()
        if " " in token or token.startswith("-"):
            continue
        if token.startswith("/"):
            # HTTP route, not a repository path.
            continue
        if "/" not in token and Path(token).suffix.lstrip(".") not in FILE_SUFFIXES:
            # prose symbol (e.g. StudioHandler._blocked) — not a path
            continue
        if token.startswith(SKIP_PREFIXES):
            continue
        if any(part in token for part in SKIP_CONTAINS):
            continue
        candidate = ROOT / token.rstrip("/")
        if not candidate.exists():
            errors.append(f"referenced path does not exist: {token}")

    for err in errors:
        print(f"check_agents_md: {err}", file=sys.stderr)
    if errors:
        print("check_agents_md: FAILED", file=sys.stderr)
        return 1
    print("check_agents_md: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
