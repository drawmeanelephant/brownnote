#!/usr/bin/env python3
"""Validate this checkout, rejecting stale tests from a shared Cargo target.

Run under the swarm's whole-check validation lock/target wrapper.
"""
import os
from pathlib import Path
import re
import subprocess
import time


root = Path(__file__).resolve().parents[2]
source = root / "native/tests/project_store.rs"
expected = set(re.findall(r"(?m)^#\[test\]\nfn (\w+)\(", source.read_text()))
if not expected:
    raise SystemExit("No project store fixtures found in this checkout")


def run(*command):
    print("$ " + " ".join(command), flush=True)
    result = subprocess.run(command, cwd=root, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        raise SystemExit(result.returncode)
    return result.stdout


def require_current_tests(output):
    passed = set(re.findall(r"(?m)^test (\w+) \.\.\. ok$", output))
    missing = expected - passed
    if missing:
        raise SystemExit("Stale/incomplete project store artifact, missing: "
                         + ", ".join(sorted(missing)))
    print(f"Verified all {len(expected)} checkout-local project store fixtures",
          flush=True)


run("git", "rev-parse", "HEAD")
# Shared workspace package filenames can outlive a different checkout's
# fingerprints. Invalidate only this lane's inputs, without deleting artifacts
# or recompiling dependencies under a separate target.
for relative in [
    "native/src/project/mod.rs",
    "native/src/project/store.rs",
    "native/src/project/recovery.rs",
    "native/tests/project_store.rs",
]:
    os.utime(root / relative, None)
run("cargo", "rustc", "--manifest-path", "native/Cargo.toml", "--lib", "--",
    f"-Cmetadata=brownnote_recovery_validation_{time.time_ns()}")
require_current_tests(run("cargo", "test", "--manifest-path", "native/Cargo.toml",
                          "--test", "project_store", "--", "--nocapture"))
run("cargo", "test", "--manifest-path", "native/Cargo.toml", "--test", "project")
require_current_tests(run("make", "native-test"))
