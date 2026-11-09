#!/usr/bin/env python3
"""Stress real render deadline/signal tests in fresh processes.

Run under the swarm's whole-check lock when sharing a Cargo target.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile


root = Path(__file__).resolve().parents[2]
cases = [
    "render_timeout_keeps_its_deadline_and_leaves_no_partial_files",
    "a_ready_brownnote_receives_sigterm_through_the_render_wait_path",
    "a_brownnote_that_ignores_sigterm_is_still_killed_within_the_grace",
]
print("Tested head: " + subprocess.check_output(
    ["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(), flush=True)
os.utime(root / "native/src/lib.rs", None)
built = subprocess.run(
    ["cargo", "test", "--manifest-path", str(root / "native/Cargo.toml"),
     "--lib", "--no-run", "--message-format=json"],
    cwd=root, capture_output=True, text=True,
)
if built.returncode:
    print(built.stdout + built.stderr, flush=True)
    raise SystemExit(built.returncode)
executables = [
    message["executable"]
    for line in built.stdout.splitlines()
    for message in [json.loads(line)]
    if message.get("reason") == "compiler-artifact"
    and message["target"]["name"] == "brownnote_native"
    and message.get("executable")
]
if len(executables) != 1:
    raise SystemExit("Expected one checkout-local library test executable")

passed = 0
with tempfile.TemporaryDirectory(prefix="brownnote-timeout-stress-") as temp:
    helper = Path(temp) / "dirname"
    helper.write_text('#!/bin/sh\n/bin/sleep 0.25\nexec /usr/bin/dirname "$@"\n')
    helper.chmod(0o755)
    for delayed in [False, True]:
        env = dict(os.environ)
        if delayed:
            env["PATH"] = temp + os.pathsep + env["PATH"]
        for _ in range(3):
            for case in cases:
                result = subprocess.run(
                    [executables[0], "pedals::render::tests::" + case,
                     "--exact", "--nocapture"],
                    cwd=root, env=env, capture_output=True, text=True, timeout=15,
                )
                if result.returncode or "1 passed; 0 failed" not in result.stdout:
                    print(result.stdout + result.stderr, flush=True)
                    raise SystemExit(
                        "Render timeout fixture failed; delayed startup=" + str(delayed))
                passed += 1
print(f"PASS: {passed} fresh-process deadline, ready-handler and SIGKILL cases; "
      "200 ms, 1200 ms and 5 s bounds unchanged", flush=True)
