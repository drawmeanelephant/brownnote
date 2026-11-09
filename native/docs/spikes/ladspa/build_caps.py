"""Fetch, verify, patch and build the pinned CAPS LADSPA library.

The tarball is cached in .work/ and checked against pin.json before use.
The build tree is recreated from the tarball every time, so no earlier
patch or object file can leak into the result.
"""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent
WORK = ROOT / ".work"
PIN = json.loads((ROOT / "pin.json").read_text())


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fetch():
    WORK.mkdir(exist_ok=True)
    tarball = WORK / Path(PIN["source_url"]).name
    if not tarball.exists():
        print(f"Downloading {PIN['source_url']}", flush=True)
        with urllib.request.urlopen(PIN["source_url"], timeout=60) as response:
            data = response.read()
        partial = tarball.with_suffix(".partial")
        partial.write_bytes(data)
        partial.rename(tarball)
    digest = sha256(tarball)
    if digest != PIN["source_sha256"]:
        raise SystemExit(f"CAPS tarball hash mismatch: {digest}")
    return tarball


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
    parser = argparse.ArgumentParser()
    parser.add_argument("--expect-unpatched-failure", action="store_true",
                        help="also prove the unmodified source fails with Apple clang")
    args = parser.parse_args()
    tarball = fetch()
    record = {"source_sha256": PIN["source_sha256"], "host": f"{platform.machine()}-{platform.system()}"}
    record["compiler"] = subprocess.run(["c++", "--version"], capture_output=True,
                                        text=True, check=True).stdout.splitlines()[0]

    if args.expect_unpatched_failure:
        src = extract(tarball, "unpatched")
        result = make(src)
        log = result.stdout + result.stderr
        if result.returncode == 0 or "no matching function for call to 'v4f_map'" not in log:
            raise SystemExit("unpatched CAPS did not fail the documented way")
        record["unpatched_build"] = {
            "exit_code": result.returncode,
            "first_error": next(line for line in log.splitlines() if "error:" in line),
        }
        print("PASS: unpatched CAPS fails with Apple clang (v4f_map builtin)", flush=True)

    src = extract(tarball, "patched")
    subprocess.run(["patch", "-p1", "--forward", "-i", ROOT / PIN["patch"]], cwd=src,
                   check=True, capture_output=True)
    result = make(src)
    if result.returncode != 0:
        print(result.stdout[-4000:], result.stderr[-4000:])
        raise SystemExit("patched CAPS build failed")
    library = src / PIN["library"]
    kind = subprocess.run(["file", "-b", library], capture_output=True, text=True,
                          check=True).stdout.strip()
    if "arm64" not in kind and platform.machine() == "arm64":
        raise SystemExit(f"unexpected CAPS architecture: {kind}")
    record.update(library=str(library), library_sha256=sha256(library), library_file=kind,
                  patch_sha256=sha256(ROOT / PIN["patch"]))
    (WORK / "caps-build.json").write_text(json.dumps(record, indent=2) + "\n")
    print(f"PASS: built {library} ({kind})", flush=True)
    print(json.dumps(record, indent=2))


if __name__ == "__main__":
    main()
