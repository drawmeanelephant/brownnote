#!/usr/bin/env python3
"""Bounded source-only check pending the integrator's `pub mod takes;` export.

This is not exported-library, application, scheduling, or audio evidence.
Run inside the swarm's whole-check lock, with its shared CARGO_TARGET_DIR.
Once exported, cargo test --lib takes::tests checks the real library instead.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

native = Path(__file__).resolve().parents[1]
target = Path(os.environ["CARGO_TARGET_DIR"]).resolve()
print("SOURCE-ONLY takes check; NOT exported-library/application coverage", flush=True)
build = subprocess.run(
    ["cargo", "build", "--manifest-path", str(native / "Cargo.toml"),
     "--lib", "--message-format=json"],
    capture_output=True, text=True,
)
print(build.stderr, end="", flush=True)
if build.returncode:
    print(build.stdout, end="")
    raise SystemExit(build.returncode)
artifacts = [
    Path(filename)
    for line in build.stdout.splitlines()
    for message in [json.loads(line)]
    if message.get("reason") == "compiler-artifact"
    and message["target"]["name"] == "brownnote_native"
    for filename in message["filenames"]
    if filename.endswith(".rlib")
]
if len(artifacts) != 1:
    raise SystemExit(f"expected one native library artifact, got {artifacts}")

with tempfile.TemporaryDirectory(prefix="brownnote-takes-source-") as temp:
    root = Path(temp) / "check.rs"
    root.write_text(
        "pub use brownnote_native::{performance, project};\n"
        f"#[path = {json.dumps(str(native / 'src/takes/mod.rs'))}]\n"
        "pub mod takes;\n",
        encoding="utf-8",
    )
    binary = target / "debug" / "takes-source-tests"
    # Check the owned module's lints as well, despite its missing root export.
    subprocess.run(
        ["clippy-driver", "--test", "--edition=2024", "--crate-name=takes_source",
         str(root), "--extern", f"brownnote_native={artifacts[0]}",
         "-L", f"dependency={target / 'debug/deps'}",
         "-D", "warnings", "-D", "unsafe_code",
         "-D", "clippy::unwrap_used", "-D", "clippy::expect_used",
         "-o", str(binary)],
        check=True,
    )
    raise SystemExit(subprocess.call([str(binary), "--nocapture"]))
