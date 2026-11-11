#!/usr/bin/env python3
"""Desktop pacing of the looks: fresh native windows of the perf build, each tracing its frames.

  python3 scripts/showcase_native.py [--runs 3] [--only TEXT]

Each run opens a real window (Retina scale, the display's own refresh, display-link pacing) on a
scene's scripted hits, traces 300 frames with Rune's --native --trace, and reads per frame the CPU
time Rune records and the present interval in display refreshes. A frame presented two or more
refreshes after the one before is a missed refresh: the window animates every frame in these
scenes, so a gap means it was late. Misses are split at 1.2 s after the first frame: on this
Mac a window that animates from launch is paced at 60 Hz for about its first second, whatever
it draws. This is the app's own submit and present timing, not photons; the window server's
composite and the panel come after it.
"""
import argparse
import json
import statistics
import subprocess
import sys
from pathlib import Path

NATIVE = Path(__file__).resolve().parent.parent
BIN = NATIVE / "target" / "perf" / "brownnote-showcase"
TMP = NATIVE / "target" / "showcase" / "measure" / "native"


def configs():
    out = []
    for fx in ["off", "light", "full"]:
        out.append((f"bunker/{fx}/storm", ["--look", "bunker", "--fx", fx, "--scene", "storm"]))
    for look in ["space", "afterhours"]:
        out.append((f"{look}/full/storm", ["--look", look, "--scene", "storm"]))
    for look in ["bunker", "space", "afterhours"]:
        out.append((f"{look}/full/performance", ["--look", look, "--scene", "performance"]))
    out.append(("bunker/full/storm/reduced", ["--look", "bunker", "--reduced-motion", "--scene", "storm"]))
    return out


def one(name, args, k):
    TMP.mkdir(parents=True, exist_ok=True)
    trace = TMP / f"{name.replace('/', '-')}-{k}.jsonl"
    try:
        done = subprocess.run([str(BIN), *args, "--native", "--trace", str(trace), "--frames", "300", "--no-audio"],
                              cwd=NATIVE, capture_output=True, text=True, timeout=120)
    except subprocess.TimeoutExpired:
        # The display link stops while the display sleeps or the screen is locked.
        sys.exit(f"{name}: no 300 frames in 120 s; is the display awake and unlocked?")
    if done.returncode != 0:
        sys.exit(f"failed {name}: {done.stderr}")
    with trace.open() as f:
        _header, *frames = [json.loads(line) for line in f]
    # Drop the launch frame: it carries window and pipeline setup.
    frames = frames[1:]
    tick = statistics.median(f["present_dt_ms"] for f in frames if f.get("present_dt_ms"))
    t0 = frames[0]["t"]
    shown = [f for f in frames if f.get("present_dt_ms")]
    missed = [f for f in shown if round(f["present_dt_ms"] / tick) >= 2]
    early = sum(f["t"] - t0 < 1200 for f in missed)
    cpu = sorted(f["quality"]["cpu_ms"] for f in frames if "quality" in f)
    return {"hz": 1000 / tick, "frames": len(frames), "missed_launch": early, "missed": len(missed) - early,
            "presented_launch": sum(f["t"] - t0 < 1200 for f in shown),
            "presented": sum(f["t"] - t0 >= 1200 for f in shown), "cpu50": cpu[len(cpu) // 2],
            "cpu95": cpu[int(0.95 * (len(cpu) - 1))], "cpumax": cpu[-1]}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--runs", type=int, default=3)
    p.add_argument("--only", default="")
    p.add_argument("--out", type=Path, default=TMP / "results.json")
    a = p.parse_args()
    if not BIN.is_file():
        sys.exit(f"missing {BIN}: cargo build --profile perf --bin brownnote-showcase")
    cfgs = [c for c in configs() if a.only in c[0]]
    runs = {name: [] for name, _ in cfgs}
    for k in range(a.runs):
        for name, args in cfgs:
            runs[name].append(one(name, args, k))
        print(f"run {k + 1}/{a.runs}", file=sys.stderr)
    a.out.write_text(json.dumps(runs, indent=1))
    print(f"{'config':28} {'Hz':>5} {'missed, first 1.2 s':>22} {'missed, after':>22} {'cpu p50':>8} {'cpu p95':>8} {'cpu max':>8}")
    for name, rs in runs.items():
        launch = " ".join(f"{r['missed_launch']}/{r['presented_launch']}" for r in rs)
        steady = " ".join(f"{r['missed']}/{r['presented']}" for r in rs)
        med = lambda key: statistics.median(r[key] for r in rs)
        print(f"{name:28} {med('hz'):5.0f} {launch:>22} {steady:>22} {med('cpu50'):8.2f} {med('cpu95'):8.2f} {med('cpumax'):8.1f}")


if __name__ == "__main__":
    main()
