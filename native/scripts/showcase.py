#!/usr/bin/env python3
"""Showcase captures: the scene matrix per look, motion clips and the comparison sheet.

  python3 scripts/showcase.py stills   [--out DIR]   scenes x looks, default and minimum size, 1x and 2x
  python3 scripts/showcase.py clips    [--out DIR]   the performance scene per look, plus variants
  python3 scripts/showcase.py sheet    [--out DIR]   comparison sheets from the stills (needs Pillow)
  python3 scripts/showcase.py docs     [--out DIR]   copy the sheets and side-by-side clip to docs/showcase

Build first: cargo build --profile perf --bin brownnote-showcase. Every capture is a fresh
headless process on Rune's virtual clock, so frames are deterministic; they say nothing about
desktop smoothness.
"""
import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

NATIVE = Path(__file__).resolve().parent.parent
BIN = NATIVE / "target" / "perf" / "brownnote-showcase"
LOOKS = ["bunker", "space", "afterhours"]
# name, capture time (s), extra args
SCENES = [
    ("idle", 0.5, []),
    ("soft-hit", 0.06, []),
    ("hard-hit", 0.06, []),
    ("overlap", 0.30, []),
    ("panic", 0.45, []),
    ("focus", 0.6, ["--script", "SCRIPTS/focus.json"]),
    ("missing-samples", 0.5, []),
    ("audio-failure", 0.5, []),
]
# Pressed (pad 10 held) with keyboard focus on the master fader; pointer hover on pad 3.
STATES = [("pressed-focus", 0.6, ["--script", "SCRIPTS/states.json"]),
          ("hover", 0.4, ["--script", "SCRIPTS/hover.json"])]
SIZES = {"default": "1120x800", "min": "820x600"}


def run(args):
    args = [str(a).replace("SCRIPTS", str(NATIVE / "scripts")) for a in args]
    done = subprocess.run([str(BIN), *args], cwd=NATIVE, capture_output=True, text=True)
    if done.returncode != 0:
        sys.exit(f"failed: {' '.join(args)}\n{done.stderr}")
    return done.stderr


def scene_args(scene, extra):
    return ([] if "--script" in extra else ["--scene", scene]) + extra


def stills(out):
    out.mkdir(parents=True, exist_ok=True)
    jobs = []
    for look in LOOKS:
        for scene, at, extra in [*SCENES, *STATES]:
            for size, scale in [("default", 2), ("default", 1), ("min", 2), ("min", 1)]:
                name = f"{look}-{scene}-{size}@{scale}x.png"
                jobs.append([*scene_args(scene, extra), "--look", look, "--at", at,
                             "--size", SIZES[size], "--scale", scale, "--snap", out / name])
        for variant, flags in [("light", ["--fx", "light"]), ("off", ["--fx", "off"]),
                               ("reduced", ["--reduced-motion"])]:
            for scene, at in [("hard-hit", 0.06), ("overlap", 0.30)]:
                jobs.append(["--scene", scene, "--look", look, *flags, "--at", at,
                             "--scale", 2, "--snap", out / f"{look}-{scene}-{variant}@2x.png"])
        jobs.append(["--scene", "lab", "--look", look, "--at", 0.09, "--scale", 2,
                     "--snap", out / f"{look}-lab@2x.png"])
        jobs.append(["--scene", "storm", "--look", look, "--at", 1.0, "--scale", 2,
                     "--snap", out / f"{look}-storm@2x.png"])
    for i, job in enumerate(jobs):
        run(job)
        print(f"\r{i + 1}/{len(jobs)}", end="", flush=True)
    print(f"\n{len(jobs)} stills in {out}")


def clips(out):
    out.mkdir(parents=True, exist_ok=True)
    made = []
    for look in LOOKS:
        for variant, flags in [("", []), ("-reduced", ["--reduced-motion"]), ("-light", ["--fx", "light"])]:
            if variant and look != "afterhours" and variant != "-reduced":
                continue
            path = out / f"{look}-performance{variant}.mp4"
            run(["--scene", "performance", "--look", look, *flags, "--scale", 1, "--fps", 60,
                 "--duration", 3.0, "--video", path])
            made.append(path)
        path = out / f"{look}-lab.mp4"
        run(["--scene", "lab", "--look", look, "--scale", 1, "--fps", 60, "--duration", 1.2, "--video", path])
        made.append(path)
    side = out / "looks-side-by-side.mp4"
    inputs = [x for look in LOOKS for x in ("-i", str(out / f"{look}-performance.mp4"))]
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", *inputs, "-filter_complex",
                    "[0:v]scale=746:-2[a];[1:v]scale=746:-2[b];[2:v]scale=746:-2[c];[a][b][c]hstack=inputs=3",
                    "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", str(side)], check=True)
    made.append(side)
    for path in made:
        print(path, f"{path.stat().st_size / 1024:.0f} KiB")


def grid(path, cols, rows, cell_w):
    """rows: (title, subtitle, [image path per column]); cells keep the first image's aspect."""
    from PIL import Image, ImageDraw, ImageFont

    first = Image.open(rows[0][2][0])
    cell_h = round(cell_w * first.height / first.width)
    pad, head, side = 16, 44, 170
    w = side + len(cols) * (cell_w + pad) + pad
    h = head + len(rows) * (cell_h + pad) + pad
    img = Image.new("RGB", (w, h), (24, 24, 26))
    draw = ImageDraw.Draw(img)
    try:
        font = ImageFont.truetype("/System/Library/Fonts/SFNSMono.ttf", 18)
    except OSError:
        font = ImageFont.load_default()
    for c, title in enumerate(cols):
        draw.text((side + pad + c * (cell_w + pad), 14), title, fill=(235, 235, 235), font=font)
    for r, (title, sub, images) in enumerate(rows):
        y = head + r * (cell_h + pad)
        draw.text((pad, y + 8), title, fill=(235, 235, 235), font=font)
        draw.text((pad, y + 32), sub, fill=(150, 150, 150), font=font)
        for c, still in enumerate(images):
            still = Image.open(still).convert("RGB")
            img.paste(still.resize((cell_w, cell_h), Image.LANCZOS), (side + pad + c * (cell_w + pad), y))
    img.save(path, optimize=True)
    print(path)


def sheet(out):
    names = ["BUNKER", "SPACE PROGRAM", "AFTERHOURS"]
    every = [*SCENES, *STATES]
    rows = [(s.upper(), f"t = {at:.2f} s", [out / f"{look}-{s}-default@2x.png" for look in LOOKS])
            for s, at, _ in every]
    grid(out / "comparison-sheet.png", names, rows, 560)
    compact = ["idle", "hard-hit", "overlap", "pressed-focus", "audio-failure"]
    grid(out / "comparison-compact.png", names, [r for r in rows if r[0].lower() in compact], 440)
    tiers = [("full", "{look}-overlap-default@2x.png"), ("light", "{look}-overlap-light@2x.png"),
             ("off", "{look}-overlap-off@2x.png"), ("reduced", "{look}-overlap-reduced@2x.png")]
    grid(out / "comparison-tiers.png", [f"--fx {t}" if t != "reduced" else "--reduced-motion" for t, _ in tiers],
         [(n, "overlap 0.30 s", [out / f.format(look=look) for _, f in tiers]) for look, n in zip(LOOKS, names)],
         400)
    grid(out / "comparison-min.png", names,
         [(s.upper(), "820x600 @1x", [out / f"{look}-{s}-min@1x.png" for look in LOOKS])
          for s in ["pressed-focus", "missing-samples"]], 500)

def docs(out):
    """What SHOWCASE.md shows. JPEG at quality 85 keeps UI text sharp at a third of the PNG size."""
    from PIL import Image

    dest = NATIVE / "docs" / "showcase"
    dest.mkdir(parents=True, exist_ok=True)
    for name in ["comparison-compact", "comparison-tiers", "comparison-min", "comparison-sheet"]:
        Image.open(out / "stills" / f"{name}.png").convert("RGB").save(
            dest / f"{name}.jpg", "JPEG", quality=85, subsampling=0, optimize=True)
    shutil.copyfile(out / "clips" / "looks-side-by-side.mp4", dest / "looks-side-by-side.mp4")
    for path in sorted(dest.iterdir()):
        print(path, f"{path.stat().st_size / 1024:.0f} KiB")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("what", choices=["stills", "clips", "sheet", "docs"])
    p.add_argument("--out", type=Path, default=NATIVE / "target" / "showcase")
    a = p.parse_args()
    if not BIN.is_file():
        sys.exit(f"missing {BIN}: cargo build --profile perf --bin brownnote-showcase")
    {"stills": lambda: stills(a.out / "stills"), "clips": lambda: clips(a.out / "clips"),
     "sheet": lambda: sheet(a.out / "stills"), "docs": lambda: docs(a.out)}[a.what]()


if __name__ == "__main__":
    main()
