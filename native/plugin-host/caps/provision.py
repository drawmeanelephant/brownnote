"""Fetch, verify, patch and build the pinned CAPS LADSPA library (#97, #114).

Brownnote does not bundle CAPS (GPL-3.0-or-later). This script builds the
pinned release for the tests and for users who want the Plate reverb:

    python3 native/plugin-host/caps/provision.py [--install DIR]

The tarball, patch and built library must all match pin.json, or the
script exits non-zero. The last line of stdout is the library's absolute
path, so callers can capture it:

    BROWNNOTE_LADSPA_PLATE="$(python3 native/plugin-host/caps/provision.py | tail -n 1)"

The build tree is recreated from the verified tarball every run. Work files
go to native/plugin-host/caps/.work/, which git ignores.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent
WORK = ROOT / ".work"
PIN = json.loads((ROOT / "pin.json").read_text())


def log(message):
    print(message, file=sys.stderr, flush=True)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def require(what, actual, expected):
    if actual != expected:
        raise SystemExit(f"{what} SHA-256 is {actual}, pin.json expects {expected}")


def fetch(tarball):
    WORK.mkdir(exist_ok=True)
    cached = WORK / Path(PIN["source_url"]).name
    if tarball:
        require("the given tarball", sha256(tarball), PIN["source_sha256"])
        shutil.copyfile(tarball, cached)
    if not cached.exists():
        for url in [PIN["source_url"], PIN["mirror_url"]]:
            try:
                log(f"Downloading {url}")
                with urllib.request.urlopen(url, timeout=60) as response:
                    data = response.read()
                break
            except OSError as error:
                log(f"  failed: {error}")
        else:
            raise SystemExit("could not download the CAPS tarball")
        partial = cached.with_suffix(".partial")
        partial.write_bytes(data)
        partial.rename(cached)
    require("the CAPS tarball", sha256(cached), PIN["source_sha256"])
    return cached


def extract(tarball, name):
    tree = WORK / name
    shutil.rmtree(tree, ignore_errors=True)
    tree.mkdir()
    with tarfile.open(tarball) as archive:
        archive.extractall(tree, filter="data")
    return tree / f"caps-{PIN['version']}"


def make(src):
    return subprocess.run(PIN["build"], cwd=src, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tarball", type=Path,
                        help="use this copy of the pinned tarball instead of downloading")
    parser.add_argument("--install", type=Path,
                        help="also copy the library into this LADSPA directory")
    parser.add_argument("--expect-unpatched-failure", action="store_true",
                        help="first prove the unmodified source fails with Apple clang")
    args = parser.parse_args()

    patch = ROOT / PIN["patch"]
    require("the arm64 patch", sha256(patch), PIN["patch_sha256"])
    tarball = fetch(args.tarball)

    if args.expect_unpatched_failure:
        result = make(extract(tarball, "unpatched"))
        if result.returncode == 0 or "no matching function for call to 'v4f_map'" not in (
                result.stdout + result.stderr):
            raise SystemExit("unpatched CAPS did not fail the documented way")
        log("PASS: unpatched CAPS fails with Apple clang (v4f_map builtin)")

    src = extract(tarball, "patched")
    subprocess.run(["patch", "-p1", "--forward", "-i", patch], cwd=src, check=True,
                   capture_output=True)
    result = make(src)
    if result.returncode != 0:
        log(result.stdout[-4000:] + result.stderr[-4000:])
        raise SystemExit("the patched CAPS build failed")
    library = (src / PIN["library"]).resolve()
    compiler = subprocess.run(["c++", "--version"], capture_output=True, text=True,
                              check=True).stdout.splitlines()[0]
    actual = sha256(library)
    if actual != PIN["library_sha256"]:
        raise SystemExit(
            f"the built {PIN['library']} has SHA-256 {actual}, pin.json expects "
            f"{PIN['library_sha256']} (built with {PIN['built_with']['compiler']}; "
            f"this machine has {compiler}). Re-pin deliberately; do not skip.")
    log(f"PASS: {library} matches the pin ({actual})")
    if args.install:
        args.install.mkdir(parents=True, exist_ok=True)
        target = args.install / PIN["library"]
        shutil.copyfile(library, target)
        require(f"the installed {target}", sha256(target), PIN["library_sha256"])
        log(f"Installed {target}")
    print(library)


if __name__ == "__main__":
    main()
