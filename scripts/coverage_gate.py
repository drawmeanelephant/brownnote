#!/usr/bin/env python3
"""Enforce a line-coverage floor for the C units.

Runs gcov over the .gcda files produced by `make coverage` and fails when any
named source falls below the minimum.  Coverage of the audio path is the
point: a change that adds branches without tests should not land.

Usage:
  scripts/coverage_gate.py --dir build/coverage --minimum 70 \
      --source src/wav.c --source src/log.c
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

# `-` marks a line with no executable code (comments, declarations); it is
# neither hit nor missed and must not count toward the ratio.
LINE = re.compile(r"^\s*(#####|\d+):\s*\d+:")


def parse_gcov(path: Path) -> tuple[int, int]:
    total = hit = 0
    for line in path.read_text(errors="replace").splitlines():
        m = LINE.match(line)
        if not m:
            continue
        total += 1
        if m.group(1) != "#####":
            hit += 1
    return hit, total


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--minimum", type=float, default=70.0)
    ap.add_argument("--source", action="append", default=[])
    args = ap.parse_args()

    work = Path(args.dir).resolve()
    root = Path(__file__).resolve().parent.parent
    gcda = sorted(work.glob("*.gcda"))
    if not gcda:
        print(f"coverage_gate: no .gcda files in {work}", file=sys.stderr)
        return 1

    # gcov resolves the recorded source paths (`src/wav.c`) relative to the
    # working directory, so run it from the repository root, not the build dir.
    subprocess.run(["gcov", "-o", str(work)] + [str(p) for p in gcda],
                   cwd=root, check=True, capture_output=True, text=True)

    # gcov drops `<source>.gcov` in its working directory; move them next to
    # the object files so the CI artifact path is predictable.
    for produced in root.glob("*.gcov"):
        produced.replace(work / produced.name)

    failed = False
    for src in args.source:
        report = work / (Path(src).name + ".gcov")
        if not report.exists():
            print(f"coverage_gate: no gcov report for {src}",
                  file=sys.stderr)
            failed = True
            continue
        hit, total = parse_gcov(report)
        if total == 0:
            print(f"coverage_gate: {src} has no instrumented lines",
                  file=sys.stderr)
            failed = True
            continue
        pct = 100.0 * hit / total if total else 0.0
        print(f"coverage_gate: {src}: {pct:.1f}% ({hit}/{total} lines)")
        if pct < args.minimum:
            print(f"coverage_gate: {src} is below {args.minimum:.0f}%",
                  file=sys.stderr)
            failed = True

    if failed:
        print("coverage_gate: FAILED", file=sys.stderr)
        return 1
    print(f"coverage_gate: ok (minimum {args.minimum:.0f}%)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
