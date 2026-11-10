#!/usr/bin/env python3
"""Cyclomatic-complexity ratchet for the C sources.

Default budget: CCN <= 18, NLOC <= 700. Functions above that on the day the
gate was introduced are listed in tools/complexity-baseline.txt with their
current numbers; they may shrink but not grow, and the entry must be deleted
once the function fits the default budget. New code gets no exemption.

Usage: scripts/check_complexity.py [--update]
"""

from __future__ import annotations

import csv
import io
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "tools" / "complexity-baseline.txt"
TARGETS = ["src", "tests/unit_wav.c", "tests/unit_log.c", "tests/fuzz_wav.c"]
MAX_CCN = 18
MAX_NLOC = 700


def lizard_rows() -> list[tuple[str, str, int, int]]:
    cmd = ["lizard", "--csv", "-l", "c", *TARGETS]
    try:
        out = subprocess.run(
            cmd, cwd=ROOT, check=True, capture_output=True, text=True
        ).stdout
    except FileNotFoundError:
        raise SystemExit(
            "check_complexity: lizard is not installed "
            "(pipx install lizard, or make complexity-c in CI)"
        )
    rows = []
    # lizard --csv emits its banner line before the header row.
    lines = [ln for ln in out.splitlines() if ln.count(",") >= 8]
    # columns: NLOC,CCN,token,PARAM,length,"name@start-end@file",file,name,...
    for row in csv.reader(io.StringIO("\n".join(lines))):
        if len(row) < 8 or row[0].strip() == "NLOC":
            continue
        location, name = row[5], row[7]
        parts = location.split("@")
        if len(parts) != 3:
            continue
        path = parts[2].strip()
        line = parts[1].split("-")[0]
        if not path.endswith(".c"):
            continue
        rows.append((f"{path}:{name}", f"{path}:{line}", int(row[0]), int(row[1])))
    return rows


def load_baseline() -> dict[str, tuple[int, int]]:
    if not BASELINE.exists():
        return {}
    entries: dict[str, tuple[int, int]] = {}
    for line in BASELINE.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, ccn, nloc = line.rsplit(None, 2)
        entries[key] = (int(ccn), int(nloc))
    return entries


def dump_baseline(entries: dict[str, tuple[int, int]]) -> None:
    BASELINE.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "# Functions that exceeded the CCN<=18 / NLOC<=700 budget when this",
        "# ratchet was added. They may shrink, never grow; delete the entry",
        "# once the function fits the default budget. Regenerate with",
        "# `scripts/check_complexity.py --update`.",
        "",
    ]
    for key in sorted(entries):
        ccn, nloc = entries[key]
        lines.append(f"{key} {ccn} {nloc}")
    BASELINE.write_text("\n".join(lines) + "\n")


def main(argv: list[str]) -> int:
    rows = lizard_rows()
    if not rows:
        print("check_complexity: lizard reported no functions", file=sys.stderr)
        return 1
    baseline = load_baseline()
    update = "--update" in argv
    violations: list[str] = []
    over = {key: (ccn, nloc) for key, _loc, nloc, ccn in rows
            if ccn > MAX_CCN or nloc > MAX_NLOC}

    for key, (ccn, nloc) in over.items():
        if key in baseline:
            bccn, bnloc = baseline[key]
            if ccn > bccn or nloc > bnloc:
                violations.append(
                    f"{key}: {ccn} CCN / {nloc} NLOC exceeds baseline "
                    f"{bccn} / {bnloc}"
                )
        else:
            violations.append(
                f"{key}: {ccn} CCN / {nloc} NLOC exceeds budget "
                f"{MAX_CCN} / {MAX_NLOC}"
            )
    for key in baseline:
        if key not in over:
            violations.append(
                f"{key}: now within budget — delete its baseline entry"
            )

    if update:
        dump_baseline(over)
        print(f"check_complexity: baseline updated ({len(over)} entries)")
        return 0

    for line in violations:
        print(f"check_complexity: {line}", file=sys.stderr)
    if violations:
        return 1
    worst = max(rows, key=lambda r: r[3])
    print(
        f"check_complexity: ok ({len(rows)} functions, "
        f"worst {worst[0]} at {worst[3]} CCN, "
        f"{len(baseline)} baselined)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
