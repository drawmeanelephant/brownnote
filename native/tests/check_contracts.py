#!/usr/bin/env python3
"""Run contract fixtures against the actual exported native library."""
import os
from pathlib import Path
import subprocess
import sys

native = Path(__file__).resolve().parents[1]
suite = sys.argv[1] if len(sys.argv) > 1 else "project"
if suite not in ("project", "performance"):
    raise SystemExit("usage: check_contracts.py [project|performance]")

env = dict(os.environ, CARGO_BUILD_JOBS="1")
env["PATH"] = "/opt/homebrew/opt/rustup/bin:" + env["PATH"]
raise SystemExit(subprocess.call(
    ["cargo", "test", "--manifest-path", str(native / "Cargo.toml"), "--test", suite],
    env=env,
))
