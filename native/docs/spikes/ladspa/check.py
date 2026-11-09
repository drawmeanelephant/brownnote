"""Run the whole #97 LADSPA check under the shared advisory lock and target.

Builds the pinned CAPS library and the fixture plugins, lints and builds the
probe, renders every scenario in fresh processes, checks every failure path
leaves no output, and runs verify.py. Nothing here needs an audio device.
"""

from array import array
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import wave

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[3]
WORK = ROOT / ".work"
SOURCE = REPO / "recordings/fart_dry.wav"


def command(args, **kwargs):
    print("+ " + shlex.join(str(a) for a in args), flush=True)
    return subprocess.run(args, check=True, **kwargs)


def write_burst(path, rate):
    """100 ms of a decaying 440 Hz sine, PCM16 mono."""
    frames = rate // 10
    samples = [int(round(16384 * math.exp(-30 * i / rate) * math.sin(2 * math.pi * 440 * i / rate)))
               for i in range(frames)]
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(struct.pack(f"<{frames}h", *samples))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock-file", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--native-gates", action="store_true")
    parser.add_argument("--save-evidence", action="store_true")
    args = parser.parse_args()
    if not args.lock_file.is_absolute() or not args.target_dir.is_absolute():
        parser.error("lock-file and target-dir must be absolute")
    with args.lock_file.open("a") as lock:
        print(f"Waiting for whole-check lock {args.lock_file}", flush=True)
        fcntl.flock(lock, fcntl.LOCK_EX)
        os.environ.update(CARGO_TARGET_DIR=str(args.target_dir), CARGO_BUILD_JOBS="2")
        os.environ.pop("LADSPA_PATH", None)
        print("Whole-check lock acquired", flush=True)
        run_checks(args)


def run_checks(args):
    manifest = ROOT / "Cargo.toml"
    command([sys.executable, ROOT / "build_caps.py", "--expect-unpatched-failure"])
    build = json.loads((WORK / "caps-build.json").read_text())
    caps = Path(build["library"])
    fixtures = WORK / "fixture_plugins.so"
    command(["cc", "-std=c99", "-Wall", "-Wextra", "-Werror", "-O1", "-bundle", "-o", fixtures,
             ROOT / "fixtures/fixture_plugins.c"])
    command(["cargo", "fmt", "--manifest-path", manifest, "--check"])
    command(["cargo", "clippy", "--locked", "--manifest-path", manifest, "--all-targets",
             "--", "-D", "warnings"])
    command(["cargo", "build", "--locked", "--manifest-path", manifest])
    probe = args.target_dir / "debug/brownnote-ladspa-probe"

    (ROOT / "out").mkdir(exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=ROOT / "out"))
    lifecycle = run / "lifecycle.log"
    env = dict(os.environ, FIXTURE_LIFECYCLE_LOG=str(lifecycle))
    for rate in (44100, 48000):
        write_burst(run / f"burst_{rate}.wav", rate)

    def render(name, library, label, source, *extra, env=env):
        command([probe, "render", library, label, source, run / name, *extra], env=env)

    with (run / "caps-descriptors.json").open("w") as out:
        command([probe, "list", caps, "--rate", "44100"], stdout=out)
    with (run / "fixture-descriptors.json").open("w") as out:
        command([probe, "list", fixtures, "--rate", "44100"], stdout=out)

    render("plate_pad", caps, "Plate", SOURCE)
    render("plate_pad_block64", caps, "Plate", SOURCE, "--block", "64")
    render("plate_pad_repeat", caps, "Plate", SOURCE)
    render("plate_pad_ladspa_path", "caps.so", "Plate", SOURCE,
           env=dict(env, LADSPA_PATH=f"/nonexistent:{caps.parent}"))
    render("plate_pad_dry", caps, "Plate", SOURCE, "--set", "blend=0")
    render("plate_pad_wet", caps, "Plate", SOURCE, "--set", "blend=1")
    for rate in (44100, 48000):
        render(f"plate_burst_{rate}", caps, "Plate", run / f"burst_{rate}.wav")
    render("hints", fixtures, "hints", SOURCE)
    render("hints_run_adding", fixtures, "hints", SOURCE, "--run-adding",
           "--set", "int=5", "--set", "toggle=0")

    negatives = check_failures(run, probe, caps, fixtures, env)
    command([sys.executable, ROOT / "verify.py", run, SOURCE])
    report = json.loads((run / "verified.json").read_text())
    report["caps_build"] = {k: v for k, v in build.items() if k != "library"}
    report["plate"]["render_wav_sha256"] = hashlib.sha256(
        (run / "plate_pad/render.wav").read_bytes()).hexdigest()
    report["failure_paths"] = negatives
    (run / "measurements.json").write_text(json.dumps(report, indent=2) + "\n")

    if args.save_evidence:
        save_evidence(run)
    if args.native_gates:
        command(["make", "native-test"], cwd=REPO)
    print(f"PASS: LADSPA probe evidence at {run}")


def check_failures(run, probe, caps, fixtures, env):
    empty = run / "empty-ladspa-dir"
    empty.mkdir()
    junk = run / "junk.so"
    junk.write_bytes(b"not a Mach-O or ELF file\n")
    plain_c = run / "plain.c"
    plain_c.write_text("int brownnote_not_ladspa(void) { return 0; }\n")
    plain = run / "plain.dylib"
    command(["cc", "-dynamiclib", "-o", plain, plain_c])
    existing = run / "already-there"
    existing.mkdir()
    (existing / "keep.txt").write_text("untouched\n")

    pad = SOURCE
    cases = [
        ("missing library path", [run / "absent.so", "Plate", pad], {}, "missing plugin library"),
        ("missing via LADSPA_PATH", ["caps.so", "Plate", pad], {"LADSPA_PATH": str(empty)},
         "missing plugin library: caps.so not found"),
        ("corrupt library", [junk, "Plate", pad], {}, "cannot load plugin library"),
        ("not a LADSPA library", [plain, "Plate", pad], {}, "no ladspa_descriptor"),
        ("missing label", [caps, "NoSuchPlate", pad], {}, "plugin label NoSuchPlate not found"),
        ("malformed port", [fixtures, "bad_port", pad], {}, "is not exactly input or output"),
        ("inverted bounds", [fixtures, "bad_hint", pad], {}, "lower bound above upper bound"),
        ("no audio output", [fixtures, "no_output", pad], {}, "unsupported channel layout 1 in / 0 out"),
        ("three outputs", [fixtures, "wide", pad], {}, "unsupported channel layout 1 in / 3 out"),
        ("non-finite output", [fixtures, "nan", pad], {}, "non-finite output at frame 1000"),
        ("unbounded tail", [fixtures, "endless", pad, "--max-tail", "1"], {},
         "tail did not fall below -90 dBFS within 1 s"),
        ("control above hint", [caps, "Plate", pad, "--set", "blend=2"], {}, "outside its hints"),
        ("unknown control", [caps, "Plate", pad, "--set", "size=1"], {}, "has no control input size"),
        ("run_adding absent", [caps, "Plate", pad, "--run-adding"], {}, "has no run_adding"),
        ("non-WAV input", [caps, "Plate", plain_c], {}, "unsupported WAV"),
    ]
    results = []
    for name, argv, extra_env, expected in cases:
        out = run / ("fail-" + name.replace(" ", "-"))
        result = subprocess.run([probe, "render", *argv[:3], out, *argv[3:]],
                                env=dict(env, **extra_env), capture_output=True, text=True)
        if result.returncode != 1 or expected not in result.stderr:
            raise AssertionError(f"{name}: exit {result.returncode}, stderr {result.stderr!r}")
        if out.exists():
            raise AssertionError(f"{name}: published output despite failure")
        print(f"PASS: {name}: {result.stderr.strip()}", flush=True)
        results.append({"case": name, "exit": 1, "stderr": result.stderr.strip()})

    result = subprocess.run([probe, "render", caps, "Plate", pad, existing], capture_output=True, text=True)
    if result.returncode != 1 or "output already exists" not in result.stderr:
        raise AssertionError("existing output was not refused")
    if sorted(p.name for p in existing.iterdir()) != ["keep.txt"]:
        raise AssertionError("existing output was modified")
    results.append({"case": "output exists", "exit": 1, "stderr": "output already exists (left untouched)"})
    leftovers = [p.name for p in run.iterdir() if ".partial-" in p.name]
    if leftovers:
        raise AssertionError(f"staging directories left behind: {leftovers}")
    check_existing_staging(run, probe, caps, pad)
    results.append({"case": "staging exists", "exit": 1,
                    "stderr": "existing staging directory left untouched"})
    print("PASS: no failure published output or left staging directories", flush=True)
    return results


def check_existing_staging(run, probe, caps, pad):
    out = run / "staging-collision"
    setup = (
        "import os, sys; from pathlib import Path; "
        "out = Path(sys.argv[4]); "
        "stage = out.parent / f'.{out.name}.partial-{os.getpid()}'; "
        "stage.mkdir(); (stage / 'keep.txt').write_text('untouched\\n'); "
        "os.execv(sys.argv[1], [sys.argv[1], 'render', sys.argv[2], 'Plate', sys.argv[3], sys.argv[4]])"
    )
    result = subprocess.run([sys.executable, "-c", setup, str(probe), str(caps), str(pad), str(out)],
                            capture_output=True, text=True)
    staging = list(run.glob(f".{out.name}.partial-*"))
    if result.returncode != 1 or out.exists() or len(staging) != 1:
        raise AssertionError(f"existing staging was removed or adopted: {result.stderr}")
    if (staging[0] / "keep.txt").read_text() != "untouched\n":
        raise AssertionError("existing staging contents were modified")
    shutil.rmtree(staging[0])
    print("PASS: existing staging is refused and left untouched", flush=True)


def save_evidence(run):
    evidence = ROOT / "evidence"
    shutil.rmtree(evidence, ignore_errors=True)
    evidence.mkdir()
    def portable(path):
        text = path.read_text().replace(str(run), "<run>").replace(str(REPO) + "/", "")
        return text.replace(str(Path.home()), "~")

    for name in ("measurements.json", "caps-descriptors.json", "fixture-descriptors.json"):
        (evidence / name).write_text(portable(run / name))
    for name in ("plate_pad", "hints"):
        (evidence / f"{name}.render.json").write_text(portable(run / name / "render.json"))
    # A playable PCM16 listening copy. The exact float32 render is
    # reproducible and identified by its SHA256 in measurements.json.
    raw = (run / "plate_pad/render.wav").read_bytes()
    floats = array("f")
    floats.frombytes(raw[raw.index(b"data") + 8:])
    pcm = array("h", (round(max(-1.0, min(1.0, v)) * 32767) for v in floats))
    with wave.open(str(evidence / "plate_pad.listen-pcm16.wav"), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(44100)
        w.writeframes(pcm.tobytes())


if __name__ == "__main__":
    main()
