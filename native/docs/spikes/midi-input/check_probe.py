#!/usr/bin/env python3
"""Check saved evidence, or rerun the probe using only its own virtual ports."""
import argparse
import json
import os
from pathlib import Path
import subprocess

here = Path(__file__).resolve().parent
native = here.parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--run", action="store_true")
parser.add_argument("--evidence", type=Path, default=here / "probe-evidence.json")
args = parser.parse_args()
if args.run:
    env = dict(os.environ, CARGO_BUILD_JOBS="1")
    env["PATH"] = "/opt/homebrew/opt/rustup/bin:" + env["PATH"]
    env.setdefault("CARGO_TARGET_DIR", str(native / "target/midi-probe"))
    result = subprocess.run(
        ["cargo", "run", "--locked", "--manifest-path", str(here / "Cargo.toml")],
        env=env, capture_output=True, text=True,
    )
    print(result.stderr, end="")
    if result.returncode:
        raise SystemExit(result.returncode)
    evidence = json.loads(result.stdout)
else:
    evidence = json.loads(args.evidence.read_text())

assert evidence["platform"] == "aarch64-apple-darwin"
assert evidence["backend_candidate"] == "midir =0.11.1 (CoreMIDI, default features)"
assert evidence["existing_ports_opened"] == 0
for field in ("explicit_close_stopped_callbacks", "source_removal_enumerated",
              "stale_port_name_error", "same_name_recreation_changed_port_id",
              "owned_endpoints_removed"):
    assert evidence[field], field
assert evidence["existing_input_count"] == evidence["final_input_count"]
samples = evidence["samples"]
assert [sample["bytes"] for sample in samples] == [
    [145, 40, 1], [145, 40, 127], [145, 40, 0], [145, 40, 64],
]
assert samples[0]["backend_timestamp_us"] > samples[0]["host_elapsed_ns"] // 1000
for before, after in zip(samples, samples[1:]):
    backend_delta_ns = (
        after["backend_timestamp_us"] - before["backend_timestamp_us"]
    ) * 1000
    host_delta_ns = after["host_elapsed_ns"] - before["host_elapsed_ns"]
    assert backend_delta_ns > 0 and host_delta_ns > 0
    # A scale check, not a latency or timestamp-accuracy guarantee. Host receive
    # time includes callback dispatch. Reject millisecond/nanosecond confusion.
    assert abs(backend_delta_ns - host_delta_ns) < 20_000_000
if args.run:
    output = native / "target/midi-probe/latest-evidence.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"New evidence: {output}")
print("PASS: owned-port enumeration/connect/timestamp/close/removal/recreation evidence")
