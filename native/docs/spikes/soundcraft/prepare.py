#!/usr/bin/env python3
"""Download/check the public pin and extract only inside this spike's .work."""
import hashlib
import json
from pathlib import Path
import platform
import subprocess


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def main():
    root = Path(__file__).resolve().parent
    pin = json.loads((root / "pin.json").read_text())
    latest = json.loads(run("gh", "api", f"repos/{pin['repository']}/releases/latest"))
    assert latest["tag_name"] == pin["tag"], (
        "Newest release changed. Review its docs, update pin.json and rerun the full probe.",
        latest["tag_name"])
    ref = json.loads(run("gh", "api",
                        f"repos/{pin['repository']}/git/ref/tags/{pin['tag']}"))["object"]
    if ref["type"] == "tag":
        ref = json.loads(run("gh", "api",
                            f"repos/{pin['repository']}/git/tags/{ref['sha']}"))["object"]
    assert ref["type"] == "commit" and ref["sha"] == pin["commit"]
    work = root / ".work"
    downloads, install, mount = [work / p for p in ("downloads", "install", "mount")]
    for path in (downloads, install, mount):
        path.mkdir(parents=True, exist_ok=True)
    args = ["gh", "release", "download", pin["tag"], "--repo", pin["repository"],
            "--dir", str(downloads), "--skip-existing"]
    for name in pin["artifacts"]:
        args.extend(["--pattern", name])
    subprocess.run(args, check=True)
    assets = {a["name"]: a for a in latest["assets"]}
    for name, digest in pin["artifacts"].items():
        assert assets[name]["digest"] == "sha256:" + digest
        assert hashlib.sha256((downloads / name).read_bytes()).hexdigest() == digest, name
    sums = (downloads / "SHA256SUMS.txt").read_text()
    for name, digest in pin["artifacts"].items():
        if name != "SHA256SUMS.txt":
            assert any(line.split() == [digest, name] for line in sums.splitlines()), name
    app = install / "SoundCraft.app/Contents/MacOS/SoundCraft"
    if not app.exists():
        subprocess.run(["hdiutil", "attach", str(downloads / "soundcraft-0.18.0-macos-universal.dmg"),
                        "-readonly", "-nobrowse", "-mountpoint", str(mount)], check=True)
        try:
            subprocess.run(["ditto", str(mount / "SoundCraft.app"),
                            str(install / "SoundCraft.app")], check=True)
        finally:
            subprocess.run(["hdiutil", "detach", str(mount)], check=True)
    cli = install / "soundcraft-cli-0.18.0-macos-universal/soundcraft-cli"
    if not cli.exists():
        subprocess.run(["ditto", "-x", "-k",
                        str(downloads / "soundcraft-cli-0.18.0-macos-universal.zip"),
                        str(install)], check=True)
    for name, path in [("SoundCraft", app), ("soundcraft-cli", cli)]:
        assert hashlib.sha256(path.read_bytes()).hexdigest() == pin["binary_sha256"][name]
    evidence = {
        "pin": pin, "latest_published_at": latest["published_at"],
        "os": run("sw_vers", "-productVersion"), "architecture": platform.machine(),
        "app_version": run(str(app), "--version"), "cli_version": run(str(cli), "--version"),
        "binary_sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest()
                          for name, path in [("SoundCraft", app), ("soundcraft-cli", cli)]},
        "binary_architectures": {name: run("lipo", "-archs", str(path))
                                for name, path in [("SoundCraft", app), ("soundcraft-cli", cli)]},
    }
    (work / "release.json").write_text(json.dumps(evidence, indent=2) + "\n")
    (work / "commands.json").write_text(run(str(cli), "commands", "--json") + "\n")
    print(json.dumps(evidence, indent=2))


if __name__ == "__main__":
    main()
