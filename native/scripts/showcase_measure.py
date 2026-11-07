#!/usr/bin/env python3
"""Frame cost of the looks, each run a fresh headless process of the perf build with --probe.

  python3 scripts/showcase_measure.py [--runs 5] [--only TEXT] [--vs OTHER_BINARY] [--out FILE.json]

`--vs` runs every config on a second build too, alternating with this one, for an A/B.
Configs run round-robin (run 1 of every config, then run 2, ...) so drift hits all alike; the
report gives medians across runs of each run's p50/p95/max. CPU is main-thread update (Rune's
events, poll, view, layout, paint) plus the compositor's encode; GPU is the time between two
timestamps that bracket the frame's passes on the GPU. Headless runs draw every frame on a
virtual clock: no presentation, no window server, no display timing.
"""
import argparse
import json
import re
import statistics
import subprocess
import sys
from pathlib import Path

NATIVE = Path(__file__).resolve().parent.parent
BIN = NATIVE / "target" / "perf" / "brownnote-showcase"
TMP = NATIVE / "target" / "showcase" / "measure"

LOOK = re.compile(
    r"look\[(?P<label>[^\]]+)\] frames=(?P<frames>\d+) live=(?P<live>\d+) \| cpu p50/p95/max (?P<cpu>[\d./naN]+) \| "
    r"update (?P<update>[\d./naN]+) \| encode (?P<encode>[\d./naN]+) \| gpu (?P<gpu>[\d./naN]+|unavailable) n=(?P<gpun>\d+) \| "
    r"fx layers max (?P<layers>\d+) px max (?P<px>\d+) \| pipeline compiles \[(?P<compiles>[^\]]*)\]")
RUNE = re.compile(r"first frame (?P<first>[\d.]+) ms")


def configs():
    out = []
    for look in ["bunker", "space", "afterhours"]:
        for fx in ["off", "light", "full"]:
            out.append((f"{look}/{fx}/storm", ["--look", look, "--fx", fx, "--scene", "storm", "--at", "2.5"]))
            out.append((f"{look}/{fx}/idle", ["--look", look, "--fx", fx, "--scene", "idle", "--at", "1.0"]))
        out.append((f"{look}/full/storm/reduced", ["--look", look, "--reduced-motion", "--scene", "storm", "--at", "2.5"]))
        out.append((f"{look}/full/first-hit", ["--look", look, "--scene", "performance", "--at", "0.4"]))
    for kind in ["plastic", "glow", "ripple", "metal", "grain"]:
        out.append((f"bunker/only-{kind}/storm", ["--look", "bunker", "--only", kind, "--scene", "storm", "--at", "2.5"]))
    return out


def triple(s):
    return [float(x) for x in s.split("/")] if s != "unavailable" else [float("nan")] * 3


def one(args, binary=BIN):
    TMP.mkdir(parents=True, exist_ok=True)
    done = subprocess.run([str(binary), *args, "--fps", "120", "--probe", "--snap", str(TMP / "last.png")],
                          cwd=NATIVE, capture_output=True, text=True)
    if done.returncode != 0:
        sys.exit(f"failed {args}: {done.stderr}")
    m = LOOK.search(done.stderr)
    if not m:
        sys.exit(f"no probe line for {args}:\n{done.stderr}")
    first = RUNE.search(done.stderr)
    compiles = [(int(i), float(ms), int(n)) for i, ms, n in re.findall(r"#(\d+):([\d.]+)ms\(\+(\d+)\)", m["compiles"])]
    return {"cpu": triple(m["cpu"]), "update": triple(m["update"]), "encode": triple(m["encode"]),
            "gpu": triple(m["gpu"]), "frames": int(m["frames"]), "live": int(m["live"]),
            "layers": int(m["layers"]), "px": int(m["px"]), "compiles": compiles,
            "first_frame_ms": float(first["first"]) if first else None}


def med(values):
    values = [v for v in values if v == v]
    return statistics.median(values) if values else float("nan")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--runs", type=int, default=5)
    p.add_argument("--only", default="")
    p.add_argument("--vs", type=Path)
    p.add_argument("--out", type=Path, default=TMP / "results.json")
    a = p.parse_args()
    if not BIN.is_file():
        sys.exit(f"missing {BIN}: cargo build --profile perf --bin brownnote-showcase")
    builds = [("", BIN)] + ([(f" vs {a.vs.name}", a.vs)] if a.vs else [])
    cfgs = [(name + tag, args, binary) for name, args in configs() if a.only in name for tag, binary in builds]
    runs = {name: [] for name, _, _ in cfgs}
    for r in range(a.runs):
        for name, args, binary in cfgs:
            runs[name].append(one(args, binary))
        print(f"run {r + 1}/{a.runs}", file=sys.stderr)
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(json.dumps(runs, indent=1))
    print(f"{'config':48} {'cpu p50':>8} {'cpu p95':>8} {'cpu max':>8} {'gpu p50':>8} {'gpu p95':>8} {'gpu max':>8} {'layers':>6} {'Mpx':>6}  first-use compiles (frame:cpu ms)")
    for name, rs in runs.items():
        c = [med([x["cpu"][i] for x in rs]) for i in range(3)]
        g = [med([x["gpu"][i] for x in rs]) for i in range(3)]
        layers = max(x["layers"] for x in rs)
        px = max(x["px"] for x in rs) / 1e6
        comp = rs[0]["compiles"]
        comp_ms = [med([next((ms for (i2, ms, _) in x["compiles"] if i2 == i), float("nan")) for x in rs]) for (i, _, _) in comp]
        comp_s = " ".join(f"#{i}:{ms:.1f}(+{n})" for (i, _, n), ms in zip(comp, comp_ms))
        print(f"{name:48} {c[0]:8.3f} {c[1]:8.3f} {c[2]:8.2f} {g[0]:8.3f} {g[1]:8.3f} {g[2]:8.3f} {layers:6d} {px:6.2f}  {comp_s}")


if __name__ == "__main__":
    main()
