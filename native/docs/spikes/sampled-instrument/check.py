"""Run the whole check under one shared advisory lock and Cargo target."""

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
REPO = ROOT.parents[3]


def command(args, **kwargs):
    print("+ " + shlex.join(str(a) for a in args), flush=True)
    return subprocess.run(args, cwd=REPO, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock-file", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--native-gates", action="store_true")
    parser.add_argument("--save-evidence", action="store_true")
    parser.add_argument("--regenerate-fixtures", action="store_true")
    args = parser.parse_args()
    if not args.lock_file.is_absolute() or not args.target_dir.is_absolute():
        parser.error("lock-file and target-dir must be absolute")
    with args.lock_file.open("a") as lock:
        print(f"Waiting for whole-check lock {args.lock_file}", flush=True)
        fcntl.flock(lock, fcntl.LOCK_EX)
        os.environ.update(CARGO_TARGET_DIR=str(args.target_dir), CARGO_BUILD_JOBS="2")
        for path in ("native/src/lib.rs", "native/src/main.rs"):
            os.utime(REPO / path, None)
        print("Whole-check lock acquired", flush=True)
        manifest = ROOT / "Cargo.toml"
        if args.regenerate_fixtures:
            command([sys.executable, ROOT / "generate_fixtures.py"])
        command([sys.executable, ROOT / "generate_fixtures.py", "--check"])
        command(["cargo", "fmt", "--manifest-path", manifest, "--check"])
        command(["cargo", "clippy", "--locked", "--manifest-path", manifest,
                 "--all-targets", "--", "-D", "warnings"])
        (ROOT / "out").mkdir(exist_ok=True)
        out = Path(tempfile.mkdtemp(prefix="run-", dir=ROOT / "out"))
        inputs = [ROOT / "fixtures/mechanics.sf2", ROOT / "fixtures/config.txt",
                  ROOT / "fixtures/events.tsv"]
        command(["cargo", "run", "--locked", "--manifest-path", manifest, "--", *inputs, out])
        command([sys.executable, ROOT / "verify.py", out])
        # A fresh process repeats every render. No cached output is accepted.
        repeat = out / "repeat"
        command(["cargo", "run", "--locked", "--manifest-path", manifest, "--", *inputs, repeat])
        for path in out.glob("*.wav"):
            if path.read_bytes() != (repeat / path.name).read_bytes():
                raise AssertionError(f"fresh-process render differs: {path.name}")
        binary = args.target_dir / "debug/brownnote-sampled-instrument-probe"
        with tempfile.TemporaryDirectory(prefix="failures-", dir=ROOT / "out") as temp:
            temp = Path(temp)
            bad = temp / "corrupt.sf2"
            bad.write_bytes(b"RIFF\0\0\0\0not-a-soundfont")
            for asset in [temp / "absent.sf2", bad]:
                result = subprocess.run([binary, asset, *inputs[1:], temp / "unexpected"],
                                        capture_output=True, text=True)
                if result.returncode == 0 or (temp / "unexpected").exists():
                    raise AssertionError("missing/corrupt SF2 did not fail before output")
                print(f"PASS: {asset.name} fails before creating output", flush=True)
            # Detached retained audio remains decodable with NO backend/SF2
            # invocation. This is file portability only, not project fallback.
            retained = temp / "retained.wav"
            retained.write_bytes((out / "sustain.wav").read_bytes())
            command([sys.executable, "-c",
                     "import wave,sys; w=wave.open(sys.argv[1]); " +
                     "assert w.getnframes()==72000; assert any(w.readframes(72000))",
                     retained], capture_output=True)
        report = json.loads((out / "measurements.json").read_text())
        report["fresh_process_runs"] = 2
        report["fresh_process_pcm_differences"] = 0
        report["negative_asset_checks"] = ["absent SF2 fails", "corrupt SF2 fails"]
        report["retained_wav_decodes_without_backend_or_sf2"] = True
        report["host"] = platform.machine() + "-" + platform.system()
        report["rustc"] = command(["rustc", "--version"], capture_output=True,
                                  text=True).stdout.strip()
        report["fixture_sha256"] = hashlib.sha256(inputs[0].read_bytes()).hexdigest()
        (out / "measurements.json").write_text(json.dumps(report, indent=2) + "\n")
        if args.save_evidence:
            evidence = ROOT / "evidence"
            evidence.mkdir(exist_ok=True)
            for filename in ("measurements.json", "trace.tsv"):
                (evidence / filename).write_bytes((out / filename).read_bytes())
            # Preserve every actual WAV compactly. mtime=0 makes gzip repeatable.
            for wav in out.glob("*.wav"):
                compressed = evidence / (wav.name + ".gz")
                compressed.write_bytes(gzip.compress(wav.read_bytes(), mtime=0))
                if gzip.decompress(compressed.read_bytes()) != wav.read_bytes():
                    raise AssertionError(f"preserved WAV differs: {wav.name}")
        if args.native_gates:
            command(["make", "native-test"])
        print(f"PASS: standalone evidence at {out}; D14 not satisfied; native workflow untested")


if __name__ == "__main__":
    main()
