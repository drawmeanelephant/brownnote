#!/usr/bin/env python3
"""Validate the shipped application's project controls, rejecting stale tests.

Run inside the swarm's whole-check lock with its shared CARGO_TARGET_DIR.
"""
import os
from pathlib import Path
import re
import subprocess
import time


root = Path(__file__).resolve().parents[2]


def run(*args):
    print("$ " + " ".join(args), flush=True)
    result = subprocess.run(args, cwd=root, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        raise SystemExit(result.returncode)
    return result.stdout


def require_tests(output):
    names = re.findall(r"(?m)^fn (project_controls_\w+)\(", (
        root / "native/src/ui/projects/tests.rs").read_text())
    for name in names:
        if not re.search(r"(?m)^test [^\n]*::" + name + r" \.\.\. ok$", output):
            raise SystemExit("Stale/missing application project fixture: " + name)
    if not names:
        raise SystemExit("No application project fixtures")
    print(f"Verified {len(names)} checkout-local project control fixtures", flush=True)


for relative in ["native/src/lib.rs", "native/src/main.rs",
                 "native/src/project/session.rs", "native/src/kit.rs"]:
    os.utime(root / relative, None)
run("git", "rev-parse", "HEAD")
run("cargo", "rustc", "--manifest-path", "native/Cargo.toml", "--lib", "--",
    f"-Cmetadata=brownnote_project_session_{time.time_ns()}")
require_tests(run("cargo", "test", "--manifest-path", "native/Cargo.toml",
                  "--bin", "brownnote-native", "ui::projects::tests", "--",
                  "--nocapture"))
require_tests(run("make", "native-test"))
