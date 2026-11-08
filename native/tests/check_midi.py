#!/usr/bin/env python3
"""Run MIDI fixtures; reject bootstrap-only evidence for an exported gate."""
import argparse
import os
from pathlib import Path
import subprocess

native = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--bootstrap", action="store_true",
                    help="allow path inclusion, not exported-artifact verification")
args = parser.parse_args()
if not args.bootstrap:
    if "pub mod midi;" not in (native / "src/lib.rs").read_text():
        raise SystemExit("BLOCKED: integrator must export `pub mod midi;` in native/src/lib.rs")
    if '#[path = "../src/midi/mod.rs"]' in (native / "tests/midi.rs").read_text():
        raise SystemExit("BLOCKED: replace test path bootstrap with `use brownnote_native::midi;`")
env = dict(os.environ, CARGO_BUILD_JOBS="1")
env["PATH"] = "/opt/homebrew/opt/rustup/bin:" + env["PATH"]
env.setdefault("CARGO_TARGET_DIR", str(native / "target"))
raise SystemExit(subprocess.call(
    ["cargo", "test", "--locked", "--manifest-path", str(native / "Cargo.toml"),
     "--test", "midi"], env=env,
))
