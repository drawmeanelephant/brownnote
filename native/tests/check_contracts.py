#!/usr/bin/env python3
"""Compile exact contract sources before integrator-owned dependency/export wiring.

This is a bootstrap, not evidence that the shipped library exports the modules.
The integrator must also run the real manifest's tests after adding wiring.
"""
import os
from pathlib import Path
import subprocess
import sys

native = Path(__file__).resolve().parents[1]
bootstrap = native / "target" / "contract-bootstrap"
bootstrap.mkdir(parents=True, exist_ok=True)
suite = sys.argv[1] if len(sys.argv) > 1 else "project"
if suite not in ("project", "performance"):
    raise SystemExit("usage: check_contracts.py [project|performance]")

# Include existing production modules, not stand-in definitions of pedal values.
(bootstrap / "lib.rs").write_text("\n".join(
    f'#[path = "{native / "src" / (name + ".rs" if name == "kit" else name + "/mod.rs")}"]\n'
    f"pub mod {name};" for name in ("engine", "kit", "pedals")
) + "\n")
(bootstrap / "Cargo.toml").write_text(f"""
[package]
name = "brownnote-contract-bootstrap"
version = "0.0.0"
edition = "2024"
[lib]
name = "brownnote_native"
path = "lib.rs"
[dependencies]
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
sha2 = "=0.10.9"
hound = "=3.5.1"
rtrb = "=0.4.1"
[[test]]
name = "{suite}"
path = "{native / "tests" / (suite + ".rs")}"
""")
env = dict(os.environ, CARGO_BUILD_JOBS="1")
env["CARGO_TARGET_DIR"] = str(native / "target" / "lane-a")
env["PATH"] = "/opt/homebrew/opt/rustup/bin:" + env["PATH"]
raise SystemExit(subprocess.call(
    ["cargo", "test", "--manifest-path", str(bootstrap / "Cargo.toml"), "--test", suite],
    env=env,
))
