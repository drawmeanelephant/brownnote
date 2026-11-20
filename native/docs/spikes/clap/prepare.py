"""Fetch and verify the two reference plugins into .work/plugins.

Nothing here is installed system-wide. The Airwindows package is expanded
with pkgutil, never run as an installer. The caller must already hold the
whole-check lock and set CARGO_TARGET_DIR / CARGO_BUILD_JOBS; check.py does.
"""

import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent
WORK = ROOT / ".work"
PLUGINS = WORK / "plugins"
PIN = json.loads((ROOT / "pin.json").read_text())

SYNTH_BUNDLE = PLUGINS / "poly_mod_synth.clap"
EFFECT_BUNDLE = PLUGINS / "Airwindows Consolidated.clap"

INFO_PLIST = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>poly_mod_synth</string>
<key>CFBundleIdentifier</key><string>com.moist-plugins-gmbh.poly-mod-synth</string>
<key>CFBundlePackageType</key><string>BNDL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
</dict></plist>
"""


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(args, **kw):
    print("+ " + " ".join(str(a) for a in args), file=sys.stderr, flush=True)
    kw.setdefault("stdout", sys.stderr)
    return subprocess.run(args, check=True, **kw)


def fetch(url, dest, attempts=4, delay=5, timeout=60):
    """Write `url` to `dest`, retrying transient errors before failing."""
    for attempt in range(attempts):
        try:
            # OSError covers connect and mid-read drops (URLError,
            # ConnectionResetError, timeouts); HTTPException covers a
            # response cut short, e.g. IncompleteRead. A truncated body that
            # somehow gets through is still caught by the sha256 pin check.
            with urllib.request.urlopen(url, timeout=timeout) as r:
                dest.write_bytes(r.read())
            return
        except (OSError, http.client.HTTPException) as error:
            if attempt + 1 == attempts:
                raise
            print(f"download failed ({error}); retrying", file=sys.stderr, flush=True)
            time.sleep(delay * (attempt + 1))


def prepare_effect():
    pin = PIN["effect"]
    exe = EFFECT_BUNDLE / "Contents/MacOS/Airwindows Consolidated"
    if exe.exists() and sha256(exe) == pin["executable_sha256"]:
        return
    WORK.mkdir(exist_ok=True)
    dmg = WORK / "airwindows.dmg"
    if not dmg.exists() or sha256(dmg) != pin["dmg_sha256"]:
        # Upstream re-publishes the rolling DAWPlugin tag; a fetch can race the
        # replacement or hit a transient CDN error, so retry before failing.
        print(f"Downloading {pin['dmg_url']}", file=sys.stderr, flush=True)
        fetch(pin["dmg_url"], dmg)
    if sha256(dmg) != pin["dmg_sha256"]:
        raise SystemExit("Airwindows dmg hash mismatch; the rolling release moved. Re-pin explicitly.")
    with tempfile.TemporaryDirectory(dir=WORK) as tmp:
        mnt, expanded = Path(tmp) / "mnt", Path(tmp) / "pkg"
        run(["hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", mnt, dmg],
            stdout=subprocess.DEVNULL)
        try:
            pkg = next(mnt.glob("*.pkg"))
            run(["pkgutil", "--expand-full", pkg, expanded])
        finally:
            run(["hdiutil", "detach", mnt], stdout=subprocess.DEVNULL)
        src = expanded / pin["pkg_component"] / "Payload/Airwindows Consolidated.clap"
        shutil.rmtree(EFFECT_BUNDLE, ignore_errors=True)
        PLUGINS.mkdir(parents=True, exist_ok=True)
        shutil.copytree(src, EFFECT_BUNDLE, symlinks=True)
    if sha256(exe) != pin["executable_sha256"]:
        raise SystemExit("Airwindows executable hash mismatch")
    run(["codesign", "--verify", "--deep", "--strict", EFFECT_BUNDLE])


def prepare_synth():
    pin = PIN["instrument"]
    src = WORK / "nih-plug"
    if not src.exists():
        run(["git", "clone", "--filter=blob:none", "--no-checkout", pin["source"], src])
    run(["git", "-C", src, "checkout", "--quiet", "--detach", pin["revision"]])
    head = subprocess.run(["git", "-C", src, "rev-parse", "HEAD"], check=True,
                          capture_output=True, text=True).stdout.strip()
    if head != pin["revision"]:
        raise SystemExit(f"nih-plug at {head}, expected {pin['revision']}")
    dirty = subprocess.run(["git", "-C", src, "status", "--porcelain"], check=True,
                           capture_output=True, text=True).stdout
    if dirty.strip():
        raise SystemExit("nih-plug checkout has local changes")
    run(["cargo", "build", "--locked", "--release", "-p", pin["package"]], cwd=src)
    dylib = Path(os.environ["CARGO_TARGET_DIR"]) / "release/libpoly_mod_synth.dylib"
    macos = SYNTH_BUNDLE / "Contents/MacOS"
    macos.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(dylib, macos / "poly_mod_synth")
    (SYNTH_BUNDLE / "Contents/Info.plist").write_text(INFO_PLIST)


def main():
    if "CARGO_TARGET_DIR" not in os.environ:
        raise SystemExit("run through check.py (needs the shared CARGO_TARGET_DIR)")
    prepare_effect()
    prepare_synth()
    print(json.dumps({
        "synth_executable_sha256": sha256(SYNTH_BUNDLE / "Contents/MacOS/poly_mod_synth"),
        "effect_executable_sha256": sha256(EFFECT_BUNDLE / "Contents/MacOS/Airwindows Consolidated"),
    }))


if __name__ == "__main__":
    main()
