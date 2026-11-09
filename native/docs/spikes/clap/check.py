"""Run the whole #96 CLAP probe check under the shared lock and Cargo target.

Usage (from the repository root):
  python3 native/docs/spikes/clap/check.py \
    --lock-file /abs/.brownnote-swarm-validation.lock \
    --target-dir /abs/shared-target [--save-evidence]
"""

import argparse
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
FIX = ROOT / "fixtures"
SYNTH = ROOT / ".work/plugins/poly_mod_synth.clap"
EFFECT = ROOT / ".work/plugins/Airwindows Consolidated.clap"
SYNTH_ID = "com.moist-plugins-gmbh.poly-mod-synth"
EFFECT_ID = "com.airwindows.consolidated"

# (subdir, name, bundle, id, fixture, extra args)
RENDERS = [
    ("synth", "life-256", SYNTH, SYNTH_ID, "synth-lifecycle.tsv", []),
    ("synth", "life-37", SYNTH, SYNTH_ID, "synth-lifecycle.tsv", ["--block", "37"]),
    ("synth", "life-128", SYNTH, SYNTH_ID, "synth-lifecycle.tsv", ["--block", "128"]),
    ("synth", "life-live-128", SYNTH, SYNTH_ID, "synth-lifecycle.tsv",
     ["--block", "128", "--mode", "realtime", "--live"]),
    ("synth", "wildcard-off", SYNTH, SYNTH_ID, "synth-lifecycle-wildcard-off.tsv", []),
    ("synth", "without-2", SYNTH, SYNTH_ID, "synth-lifecycle-without-2.tsv", []),
    ("synth", "velocity", SYNTH, SYNTH_ID, "synth-velocity.tsv", []),
    ("synth", "release-long", SYNTH, SYNTH_ID, "synth-release-long.tsv", []),
    ("synth", "same-key-during-release", SYNTH, SYNTH_ID, "synth-same-key-during-release.tsv", []),
    ("synth", "zero-attack", SYNTH, SYNTH_ID, "synth-zero-attack.tsv", []),
    ("effect", "burst-512", EFFECT, EFFECT_ID, "effect-burst.tsv", []),
    ("effect", "burst-rt-512", EFFECT, EFFECT_ID, "effect-burst.tsv", ["--mode", "realtime"]),
    ("effect", "burst-256", EFFECT, EFFECT_ID, "effect-burst.tsv", ["--block", "256"]),
    ("effect", "burst-live-256", EFFECT, EFFECT_ID, "effect-burst.tsv",
     ["--block", "256", "--mode", "realtime", "--live"]),
    ("effect", "bypass", EFFECT, EFFECT_ID, "effect-bypass.tsv", []),
    ("effect", "tail-30s", EFFECT, EFFECT_ID, "effect-tail-30s.tsv", []),
]
# Renders repeated in a second fresh process and compared byte for byte.
REPEAT = ["life-256", "burst-512", "tail-30s"]
# Compact evidence kept in git; every other WAV is recorded by hash only.
KEEP_WAVS = ["synth/life-256.wav", "synth/wildcard-off.wav", "synth/same-key-during-release.wav",
             "synth-state/state-reference.wav", "synth-state/state-mutated.wav",
             "effect/burst-512.wav"]


def command(args, **kw):
    print("+ " + shlex.join(str(a) for a in args), flush=True)
    return subprocess.run(args, check=True, **kw)


def probe(binary, *args):
    command([binary, *args], cwd=FIX)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock-file", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--save-evidence", action="store_true")
    args = parser.parse_args()
    if not args.lock_file.is_absolute() or not args.target_dir.is_absolute():
        parser.error("lock-file and target-dir must be absolute")
    with args.lock_file.open("a") as lock:
        print(f"Waiting for whole-check lock {args.lock_file}", flush=True)
        fcntl.flock(lock, fcntl.LOCK_EX)
        os.environ.update(CARGO_TARGET_DIR=str(args.target_dir), CARGO_BUILD_JOBS="2")
        print("Whole-check lock acquired", flush=True)
        prep = command([sys.executable, ROOT / "prepare.py"], capture_output=True, text=True)
        print(prep.stdout, end="")
        hashes = json.loads(prep.stdout.strip().splitlines()[-1])
        manifest = ROOT / "Cargo.toml"
        command(["cargo", "fmt", "--manifest-path", manifest, "--check"])
        command(["cargo", "clippy", "--locked", "--manifest-path", manifest, "--all-targets",
                 "--", "-D", "warnings"])
        command(["cargo", "build", "--locked", "--release", "--manifest-path", manifest])
        binary = args.target_dir / "release/brownnote-clap-probe"

        (ROOT / "out").mkdir(exist_ok=True)
        out = Path(tempfile.mkdtemp(prefix="run-", dir=ROOT / "out"))
        print(f"Run directory {out}", flush=True)
        for bundle, name in ((SYNTH, "synth"), (EFFECT, "effect")):
            with (out / f"describe-{name}.json").open("w") as f:
                command([binary, "describe", bundle], stdout=f)
        for sub, name, bundle, pid, fixture, extra in RENDERS:
            probe(binary, "render", bundle, pid, fixture, out / sub, name, *extra)
        probe(binary, "state", SYNTH, SYNTH_ID, "synth-lifecycle.tsv", out / "synth-state", "Gain=0.5")
        probe(binary, "state", EFFECT, EFFECT_ID, "effect-burst.tsv", out / "effect-state", "Bigness=0.25")
        # Restore into fresh processes: no state survives in memory between these.
        for sub, bundle, pid, fixture in (("synth-state", SYNTH, SYNTH_ID, "synth-lifecycle.tsv"),
                                          ("effect-state", EFFECT, EFFECT_ID, "effect-burst.tsv")):
            for blob, name in (("s0", "fresh-s0"), ("s1", "fresh-s1"), ("s0", "fresh-s0-again")):
                probe(binary, "render", bundle, pid, fixture, out / sub, name,
                      "--load-state", out / sub / f"state-{blob}.bin")
        repeat = out / "repeat"
        for sub, name, bundle, pid, fixture, extra in RENDERS:
            if name in REPEAT:
                probe(binary, "render", bundle, pid, fixture, repeat / sub, name, *extra)

        command([sys.executable, ROOT / "verify.py", out])
        report = json.loads((out / "measurements.json").read_text())
        report["host"] = f"{platform.machine()}-{platform.system()} {platform.mac_ver()[0]}"
        report["rustc"] = command(["rustc", "--version"], capture_output=True,
                                  text=True).stdout.strip()
        report["plugin_executables_sha256"] = hashes
        # Paced-thread lateness depends on machine load; record it alongside.
        report["loadavg_1_5_15_at_end"] = [round(x, 2) for x in os.getloadavg()]
        report["wav_sha256"] = {
            str(p.relative_to(out)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(out.rglob("*.wav")) if "repeat" not in p.parts
        }
        (out / "measurements.json").write_text(json.dumps(report, indent=2) + "\n")
        if args.save_evidence:
            save_evidence(out)
        print(f"PASS: CLAP probe evidence at {out}")


def save_evidence(out):
    ev = ROOT / "evidence"
    for old in ev.rglob("*"):
        if old.is_file():
            old.unlink()
    ev.mkdir(exist_ok=True)
    for path in sorted(out.rglob("*")):
        rel = path.relative_to(out)
        if path.is_dir() or rel.parts[0] == "repeat":
            continue
        if path.suffix in (".json", ".tsv", ".bin"):
            dst = ev / rel
        elif str(rel) in KEEP_WAVS:
            dst = ev / (str(rel) + ".gz")
        else:
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        data = path.read_bytes()
        dst.write_bytes(gzip.compress(data, mtime=0) if dst.suffix == ".gz" else data)


if __name__ == "__main__":
    main()
