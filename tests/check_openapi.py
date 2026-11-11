#!/usr/bin/env python3
"""Verify docs/openapi.yaml still describes the studio's routes.

The schema is the contract agents code against. If someone adds a route to
scripts/studio.py without describing it, this fails.

Usage: tests/check_openapi.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STUDIO = ROOT / "scripts" / "studio.py"
SPEC = ROOT / "docs" / "openapi.yaml"

ROUTE = re.compile(r'parsed\.path == "(/api/[A-Za-z0-9_/-]+)"')
STARTS = re.compile(r'parsed\.path\.startswith\("(/api/[A-Za-z0-9_/-]+)"\)')
EXTRA = re.compile(r'parsed\.path == "(/healthz|/metrics)"')


def main() -> int:
    source = STUDIO.read_text()
    routes = set(ROUTE.findall(source)) | set(EXTRA.findall(source))
    # dynamic prefixes (e.g. /api/library/) are covered by their concrete
    # endpoints; drop the bare prefix.
    routes = {r.rstrip("/") for r in routes if r.rstrip("/")}
    if not routes:
        print("check_openapi: no routes found in studio.py", file=sys.stderr)
        return 1

    spec = SPEC.read_text()
    if "openapi:" not in spec or "paths:" not in spec:
        print("check_openapi: spec is not an OpenAPI document", file=sys.stderr)
        return 1
    described = set(re.findall(r"^  (/[A-Za-z0-9_/{}-]+):$", spec, re.M))

    missing = sorted(routes - described)
    stale = sorted(p for p in described if p not in routes and "{" not in p)
    for route in missing:
        print(f"check_openapi: route not documented: {route}", file=sys.stderr)
    for route in stale:
        print(f"check_openapi: documented route not served: {route}", file=sys.stderr)
    if missing or stale:
        print("check_openapi: FAILED", file=sys.stderr)
        return 1
    print(f"check_openapi: ok ({len(described)} paths)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
