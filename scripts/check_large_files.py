#!/usr/bin/env python3
"""Fail when a tracked file exceeds the size budget.

Audio evidence is allowed to be large, but only inside recordings/ and only
up to the recordings budget; everything else is held to a much smaller cap so
generated bundles and crash dumps do not land in the history.

Usage:
  scripts/check_large_files.py            # scan tracked files
  scripts/check_large_files.py --staged   # scan staged files (pre-commit)
"""

from __future__ import annotations

import subprocess
import sys

DEFAULT_LIMIT = 10 * 1024 * 1024
RECORDINGS_LIMIT = 25 * 1024 * 1024


def tracked_files(staged: bool) -> list[str]:
    cmd = ["git", "diff", "--cached", "--name-only", "--diff-filter=ACMR"]
    if not staged:
        cmd = ["git", "ls-files"]
    out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
    return [line for line in out.splitlines() if line]


def main() -> int:
    staged = "--staged" in sys.argv[1:]
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    bad: list[str] = []
    for rel in tracked_files(staged):
        path = f"{root}/{rel}"
        try:
            size = __import__("os").path.getsize(path)
        except OSError:
            continue
        limit = RECORDINGS_LIMIT if rel.startswith("recordings/") else DEFAULT_LIMIT
        if size > limit:
            bad.append(f"{rel}: {size} bytes > {limit}")
    for line in bad:
        print(f"check_large_files: {line}", file=sys.stderr)
    if bad:
        print(
            "check_large_files: move evidence under recordings/ or add a "
            "documented exception in scripts/check_large_files.py",
            file=sys.stderr,
        )
        return 1
    print(f"check_large_files: ok ({len(tracked_files(staged))} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
