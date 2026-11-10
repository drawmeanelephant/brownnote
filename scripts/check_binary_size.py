#!/usr/bin/env python3
"""Report the native binary size and fail when it exceeds its budget.

The native app links rune, wgpu, cpal and rustysynth.  A budget turns "the
binary got huge" into a reviewable CI failure instead of a surprise on the
release page.

Usage:
  scripts/check_binary_size.py --binary native/target/release/brownnote-native \
      --budget-mb 160
"""

from __future__ import annotations

import argparse
import os
import sys


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--budget-mb", type=float, required=True)
    args = ap.parse_args()

    try:
        size = os.path.getsize(args.binary)
    except OSError as exc:
        print(f"check_binary_size: {exc}", file=sys.stderr)
        return 1

    mb = size / (1024 * 1024)
    print(f"check_binary_size: {args.binary}: {mb:.1f} MiB "
          f"(budget {args.budget_mb:.0f} MiB)")
    if mb > args.budget_mb:
        print(
            "check_binary_size: over budget — check for a new heavy crate "
            "(run `cargo tree --duplicates`), or raise the budget with a "
            "measured reason in the PR",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
