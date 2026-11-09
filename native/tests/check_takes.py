#!/usr/bin/env python3
"""Check the exported takes API under the swarm's shared validation lock."""
import os
from pathlib import Path
import re
import subprocess


root = Path(__file__).resolve().parents[2]
source = root / "native/src/takes/tests.rs"
expected = set(re.findall(r"(?m)^#\[test\]\nfn (\w+)\(", source.read_text()))
if not expected:
    raise SystemExit("No checkout-local takes fixtures")


def run(*command):
    print("$ " + " ".join(command), flush=True)
    result = subprocess.run(command, cwd=root, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        raise SystemExit(result.returncode)
    return result.stdout


def require_tests(output):
    for name in expected:
        if "test takes::tests::" + name + " ... ok" not in output:
            raise SystemExit("Stale/missing exported takes fixture: " + name)
    print(f"Verified {len(expected)} checkout-local exported takes fixtures", flush=True)


for relative in ["native/src/lib.rs", "native/src/main.rs"]:
    os.utime(root / relative, None)
run("git", "rev-parse", "HEAD")
require_tests(run("cargo", "test", "--manifest-path", "native/Cargo.toml",
                  "--lib", "takes::tests", "--", "--nocapture"))
run("cargo", "test", "--manifest-path", "native/Cargo.toml", "--test", "takes")
require_tests(run("make", "native-test"))
