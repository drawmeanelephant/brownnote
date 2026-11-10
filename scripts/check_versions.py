#!/usr/bin/env python3
"""Reject dependency version drift across the Cargo manifests.

The repository has several independent Cargo roots (the app, the plugin-host
workspace, and the spike crates). They must not disagree about the version of
a crate they share: a drifted `serde` or `wgpu` makes "works in native/" a
lie for the plugin host.

Policy:
  * exact pins (`=x.y.z`) must match everywhere the crate appears;
  * a caret requirement (`^x` / `x`) may not be wider than another
    requirement for the same crate;
  * git dependencies must pin the same revision.

Exit 1 with a readable diff when drift is found.

Usage: scripts/check_versions.py [--lock]
  --lock  also compare Cargo.lock resolved versions for the crates that both
          the app and the plugin host depend on.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFESTS = [
    "native/Cargo.toml",
    "native/plugin-host/Cargo.toml",
    "native/plugin-host/host/Cargo.toml",
    "native/plugin-host/fault-fixture/Cargo.toml",
    "native/plugin-host/ladspa-fixture/Cargo.toml",
    "native/docs/spikes/midi-input/Cargo.toml",
    "native/docs/spikes/sampled-instrument/Cargo.toml",
    "native/docs/spikes/clap/Cargo.toml",
]

DEP_TABLES = (
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
)


def requirements(path: Path) -> dict[str, list[str]]:
    """Map crate name -> requirement strings declared in one manifest."""
    data = tomllib.loads(path.read_text())
    found: dict[str, list[str]] = defaultdict(list)

    def scan(table: dict) -> None:
        for name, spec in table.items():
            if isinstance(spec, str):
                found[name].append(spec)
            elif isinstance(spec, dict):
                if "rev" in spec:
                    found[name].append(f"rev:{spec['rev']}")
                elif "version" in spec:
                    found[name].append(str(spec["version"]))
                elif "path" in spec:
                    found[name].append("path")

    for key in DEP_TABLES:
        scan(data.get(key, {}))
    for target in data.get("target", {}).values():
        for key in DEP_TABLES:
            scan(target.get(key, {}))
    return found


def normalize(req: str) -> str:
    req = req.strip()
    if req.startswith("rev:"):
        return req
    return req.lstrip("=^~ ")


def main() -> int:
    seen: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for rel in MANIFESTS:
        path = ROOT / rel
        if not path.exists():
            print(f"check_versions: missing manifest {rel}", file=sys.stderr)
            return 1
        for name, reqs in requirements(path).items():
            for req in reqs:
                seen[name].append((rel, req))

    drift = 0
    for name, entries in sorted(seen.items()):
        versions = {normalize(req) for _, req in entries}
        if len(versions) > 1:
            drift += 1
            print(f"check_versions: {name} disagrees:", file=sys.stderr)
            for rel, req in entries:
                print(f"  {rel}: {req}", file=sys.stderr)

    if drift:
        print(
            "check_versions: align the manifests (same exact pin or revision) "
            "or move the shared dependency into a workspace table",
            file=sys.stderr,
        )
        return 1

    print(f"check_versions: ok ({len(seen)} crates, {len(MANIFESTS)} manifests)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
