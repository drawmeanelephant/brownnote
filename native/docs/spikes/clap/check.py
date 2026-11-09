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
import shutil
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


FAULT = ROOT / ".work/plugins/brownnote-fault-fixture.clap"
FAULT_INFO = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>brownnote-fault-fixture</string>
<key>CFBundleIdentifier</key><string>org.brownnote.fault-fixture</string>
<key>CFBundlePackageType</key><string>BNDL</string>
</dict></plist>
"""


def bundle_fault_fixture(target_dir):
    macos = FAULT / "Contents/MacOS"
    macos.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(target_dir / "release/libbrownnote_clap_fault_fixture.dylib",
                    macos / "brownnote-fault-fixture")
    (FAULT / "Contents/Info.plist").write_text(FAULT_INFO)


def run_failures(binary, out):
    """Each case runs as its own process; exit codes are part of the evidence."""
    fails = out / "failures"
    fails.mkdir()
    garbage = fails / "garbage.clap"
    garbage.write_bytes(bytes(range(256)) * 64)
    state = fails / "keep-state.bin"
    state.write_bytes(b"project-owned plugin state that must survive a failed load")
    before = hashlib.sha256(state.read_bytes()).hexdigest()
    # A loadable Mach-O dylib that exports no clap_entry.
    not_a_clap = fails / "not-a-clap.dylib"
    subprocess.run(["cc", "-dynamiclib", "-x", "c", "-", "-o", not_a_clap],
                   input=b"int brownnote_not_clap = 1;\n", check=True)
    cases = {
        "missing-bundle": (ROOT / ".work/plugins/absent.clap", "org.brownnote.fault.passthrough"),
        "garbage-binary": (garbage, "org.brownnote.fault.passthrough"),
        "not-a-clap": (not_a_clap, "org.brownnote.fault.passthrough"),
        "unknown-id": (FAULT, "org.brownnote.fault.absent"),
        "init-fails": (FAULT, "org.brownnote.fault.init-fails"),
        "activate-fails": (FAULT, "org.brownnote.fault.activate-fails"),
    }
    for name, (bundle, pid) in cases.items():
        args = [binary, "render", bundle, pid, "fault-sine.tsv", fails / f"load-{name}-out", "x",
                "--load-state", state]
        print("+ " + shlex.join(str(a) for a in args), flush=True)
        r = subprocess.run(args, cwd=FIX, capture_output=True, text=True)
        err = json.loads(r.stdout.strip().splitlines()[-1]) if r.stdout.strip() else {}
        # Committed evidence must not carry this machine's checkout path.
        detail = err.get("detail")
        if detail:
            detail = detail.replace(str(fails), "<run>/failures").replace(str(ROOT), "<spike>")
        (fails / f"load-{name}.json").write_text(json.dumps({
            "exit_code": r.returncode, "error": err.get("error"), "detail": detail,
            "output_created": (fails / f"load-{name}-out").exists(),
            "state_unchanged": hashlib.sha256(state.read_bytes()).hexdigest() == before,
        }) + "\n")

    exits = {}
    for fault in ("passthrough", "crash", "hang", "nonfinite"):
        args = [binary, "inproc", FAULT, f"org.brownnote.fault.{fault}", "fault-sine.tsv", fails,
                f"inproc-{fault}", "--timeout-ms", "200"]
        print("+ " + shlex.join(str(a) for a in args), flush=True)
        exits[fault] = subprocess.run(args, cwd=FIX, timeout=60).returncode
    (fails / "inproc-exit-codes.json").write_text(json.dumps(exits) + "\n")

    for fault in ("passthrough", "crash", "hang", "nonfinite", "init-fails"):
        probe(binary, "isolate", FAULT, f"org.brownnote.fault.{fault}", "fault-sine.tsv", fails,
              f"helper-{fault}", "--timeout-ms", "200", "--max-restarts", "2")
    for block in ("64", "256"):
        probe(binary, "render", FAULT, "org.brownnote.fault.passthrough", "fault-sine-long.tsv",
              fails, f"inproc-passthrough-{block}", "--block", block)
        probe(binary, "isolate", FAULT, "org.brownnote.fault.passthrough", "fault-sine-long.tsv",
              fails, f"helper-passthrough-{block}", "--block", block)
    probe(binary, "isolate", EFFECT, EFFECT_ID, "effect-burst.tsv", fails, "helper-effect-256",
          "--block", "256", "--timeout-ms", "1000")


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
        # cargo fmt cannot find targets of a virtual manifest from outside it.
        command(["cargo", "fmt", "--all", "--check"], cwd=ROOT)
        command(["cargo", "clippy", "--locked", "--manifest-path", manifest, "--all-targets",
                 "--", "-D", "warnings"])
        command(["cargo", "build", "--locked", "--release", "--manifest-path", manifest])
        binary = args.target_dir / "release/brownnote-clap-probe"
        bundle_fault_fixture(args.target_dir)

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
        run_failures(binary, out)
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
