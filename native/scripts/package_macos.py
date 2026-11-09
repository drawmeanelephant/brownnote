#!/usr/bin/env python3
"""Build and exercise the real macOS binary, never a substitute path harness."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import wave

ROOT = Path(__file__).resolve().parents[2]
LAYOUT = "Brownnote Native Test"
FILES = ["fart_dry.wav", "fart_fuzz.wav", "fart_delay.wav",
         "fart_chorus.wav", "fart_chain.wav"]
NOTICE_PREFIXES = ("license", "licence", "copying", "notice", "unlicense", "copyright")
REVIEWED_LICENSES = {
    "MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib",
    "Unicode-3.0", "MPL-2.0", "Unlicense", "CC0-1.0", "MIT-0", "0BSD",
    "LLVM-exception",
}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, **kwargs)


def output(args):
    return subprocess.check_output([str(a) for a in args], cwd=ROOT, text=True).strip()


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def mac_arm():
    require(platform.system() == "Darwin" and platform.machine() == "arm64",
            "Only native macOS arm64 packaging/smoke is supported.")


def inventory(metadata, layout):
    """Preserve upstream notices; accompany every MPL component with its source."""
    supplements = json.loads((ROOT / "native/scripts/package-notices.json").read_text())
    supplied = {p: item for item in supplements for p in item["packages"]}
    entries = []
    licenses = layout / "licenses"
    licenses.mkdir()
    shutil.copy2(ROOT / "LICENSE", licenses / "Brownnote-MIT.txt")
    rust_docs = Path(output(["rustc", "--print", "sysroot"])) / "share/doc/rust"
    require((rust_docs / "COPYRIGHT-library.html").is_file(),
            "Rust standard-library notices missing; install the rust-docs component first.")
    shutil.copy2(rust_docs / "COPYRIGHT-library.html", licenses / "Rust-COPYRIGHT-library.html")
    shutil.copytree(rust_docs / "licenses", licenses / "Rust-license-texts")
    # Native Rune embeds Inter; Plex is conservatively credited too, not advertised.
    for font in ("inter", "ibmplexmono"):
        (licenses / f"{font}-OFL.txt").write_text(supplied[f"font:{font}"]["notices"]["OFL.txt"])
    (licenses / "Inter-copyright.txt").write_text(
        "Embedded font metadata: Copyright 2016 The Inter Project Authors\n"
        "Unmodified Inter Regular, Medium and SemiBold from the pinned Rune revision.\n")
    with tarfile.open(layout / "corresponding-source.tar.gz", "w:gz") as sources:
        for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
            key = f'{package["name"]}-{package["version"]}'
            declared = package.get("license") or ""
            tokens = set(re.findall(r"[A-Za-z0-9.-]+", declared)) - {"AND", "OR", "WITH"}
            require(tokens and tokens <= REVIEWED_LICENSES, f"Unreviewed license: {key}: {declared}")
            source = Path(package["manifest_path"]).parent
            notices = [p for p in source.rglob("*") if p.is_file()
                       and p.name.lower().startswith(NOTICE_PREFIXES)]
            # Workspace crates inherit Rune's root MIT license.
            if (package.get("source") or "").startswith("git+https://github.com/anomalyco/rune"):
                notices.append(source.parents[1] / "LICENSE")
            if package["name"] == "brownnote-native":
                notices = [ROOT / "LICENSE"]
            dest = licenses / key
            dest.mkdir()
            for index, notice in enumerate(sorted(set(notices))):
                shutil.copyfile(notice, dest / f"{index}-{notice.name}")
            extra = supplied.get(key)
            if extra:
                for name, body in extra["notices"].items():
                    (dest / name).write_text(body)
            # dispatch declares MIT in Cargo.toml, but publishes no standalone notice.
            # Preserve its complete upstream source/attribution, never invent a copyright.
            source_included = "MPL-2.0" in tokens or key == "dispatch-0.2.0"
            if source_included:
                sources.add(source, arcname=key, filter=source_filter)
            require(any(dest.iterdir()) or key == "dispatch-0.2.0",
                    f"No upstream license notice for {key}; review before bundling.")
            entries.append({
                "package": key, "declared_license": declared,
                "source": package.get("source"), "repository": package.get("repository"),
                "authors": package.get("authors", []),
                "supplement_revision": extra["revision"] if extra else None,
                "corresponding_source_included": source_included,
                "notices": sorted(p.name for p in dest.iterdir()),
            })
    write_json(layout / "dependency-inventory.json", {
        "scope": "Conservative locked macOS dependency graph, including build/dev dependencies; not a link map",
        "packages": entries,
    })


def source_filter(member):
    require(member.isfile() or member.isdir(), f"Non-regular source member: {member.name}")
    member.uid = member.gid = 0
    member.uname = member.gname = ""
    return member


def tones(directory):
    directory.mkdir(parents=True)
    for index, name in enumerate(FILES):
        with wave.open(str(directory / name), "wb") as wav:
            wav.setparams((1, 2, 44100, 0, "NONE", "not compressed"))
            wav.writeframes(b"".join(struct.pack("<h", round(
                4096 * min(i / 220, 1, (44100 - i) / 220)
                * math.sin(2 * math.pi * (220 + 55 * index) * i / 44100)))
                for i in range(44100)))


def package(destination):
    mac_arm()
    destination = Path(destination).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    head = output(["git", "rev-parse", "HEAD"])
    dirty = bool(output(["git", "status", "--porcelain", "--untracked-files=normal"]))
    archive = destination / f"brownnote-native-macos-arm64-{head[:12]}-unsigned-test.tar.gz"
    require(not archive.exists(), f"Refusing to replace existing artifact: {archive}")
    manifest = ROOT / "native/Cargo.toml"
    metadata = json.loads(output(["cargo", "metadata", "--locked", "--format-version", "1",
                                  "--filter-platform", "aarch64-apple-darwin",
                                  "--manifest-path", manifest]))
    with tempfile.TemporaryDirectory(prefix="brownnote-package-") as temp:
        layout = Path(temp) / LAYOUT
        layout.mkdir()
        # Inventory and notices are generated before any executable or audio is bundled.
        inventory(metadata, layout)
        run(["cargo", "build", "--locked", "--release", "--manifest-path", manifest,
             "--bin", "brownnote-native"], cwd=ROOT)
        binary = Path(metadata["target_directory"]) / "release/brownnote-native"
        require(output(["lipo", "-archs", binary]) == "arm64", "Expected arm64-only executable")
        dependencies = output(["otool", "-L", binary]).splitlines()[1:]
        require(all(line.strip().startswith(("/System/Library/", "/usr/lib/"))
                    for line in dependencies), "Non-system dylib dependency; do not distribute.")
        shutil.copy2(binary, layout / "brownnote-native")
        # Apple silicon toolchains may emit an ad-hoc signature. There is no
        # Developer ID signature, notarization, installer, or Gatekeeper claim.
        tones(layout / "resources/recordings")
        shutil.copy2(ROOT / "native/docs/macos-test-package.md", layout / "README.md")
        shutil.copy2(ROOT / "native/docs/distribution-inventory.md", layout / "DISTRIBUTION.md")
        shutil.copy2(ROOT / "native/Cargo.lock", layout / "Cargo.lock")
        write_json(layout / "build-record.json", {
            "source_commit": head, "source_dirty": dirty,
            "target": "aarch64-apple-darwin", "profile": "release",
            "rustc": output(["rustc", "--version"]), "build_os": platform.mac_ver()[0],
            "binary_sha256": digest(layout / "brownnote-native"),
            "cargo_lock_sha256": digest(manifest.with_name("Cargo.lock")),
            "dylibs": [line.strip() for line in dependencies],
            "kit": "Generated one-second test tones, NOT the Brownnote recordings",
            "kit_sha256": {name: digest(layout / "resources/recordings" / name) for name in FILES},
            "pedals": "Unavailable: base-pad-only artifact; no C CLI or GuitarPedal bench",
            "signing": "No Developer ID or notarization; linker ad-hoc signature may be present",
            "validation": "Unvalidated until the matching external .smoke.json reports passed",
        })
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(layout, arcname=LAYOUT, filter=source_filter)
    archive.with_name(archive.name + ".sha256").write_text(f"{digest(archive)}  {archive.name}\n")
    print(archive)


def extract(archive, destination):
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        require(sum(m.size for m in members) < 1024**3, "Artifact exceeds 1 GiB smoke limit")
        for member in members:
            path = Path(member.name)
            require(not path.is_absolute() and ".." not in path.parts
                    and path.parts and path.parts[0] == LAYOUT
                    and (member.isfile() or member.isdir()),
                    f"Unsafe or unexpected artifact member: {member.name}")
        tar.extractall(destination, members=members, filter="data")
    return destination / LAYOUT


def custom_hit(path):
    """A distinctive quarter-second user sample, unlike any bundled tone."""
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, 44100, 0, "NONE", "not compressed"))
        wav.writeframes(b"".join(struct.pack("<h", round(
            6000 * math.sin(2 * math.pi * 660 * i / 44100))) for i in range(11025)))


def set_writable(root, writable):
    for directory, names, files in os.walk(root):
        for path in [Path(directory)] + [Path(directory) / n for n in names + files]:
            mode = path.stat().st_mode
            path.chmod(mode | 0o200 if writable else mode & ~0o222)


def snapshot(root):
    return {str(p.relative_to(root)): digest(p) for p in sorted(root.rglob("*")) if p.is_file()}


def project_workflow(tree, layout, temp, record):
    """Drive the packaged app's real import and project controls, then move both
    the project and the installation before reopening. Returns the new layout."""
    def scripted(name, events, at):
        script = temp / f"{name} script.json"
        script.write_text(json.dumps(events))
        return tree(layout, name, "--script", script, "--at", at, "--size", "820x600")

    # Headless frames advance a virtual clock; the store worker runs in real
    # time. Typing takes one frame per character, so waits are generous.
    def open_events(root):
        return [{"t": 0.1, "type": "click", "key": "projects-toggle"},
                {"t": 0.4, "type": "click", "key": "projects-path"},
                {"t": 0.5, "type": "type", "text": str(root)},
                {"t": 5.0, "type": "click", "key": "projects-open"}]

    def opened(name, root, generation):
        view = scripted(name, open_events(root), 40)
        require(not view.get("projects-error")
                and view.get("projects-status", "").startswith(f"Opened generation {generation}."),
                f"{name}: {view.get('projects-error') or view.get('projects-status')}")

    def rejected(name, events, at, expected):
        view = scripted(name, events, at)
        error = view.get("projects-error") or ""
        require(expected in error, f"{name}: expected {expected!r}, got {error or view.get('projects-status')!r}")
        return view

    sample = temp / "user samples" / "my custom hit.wav"
    sample.parent.mkdir()
    custom_hit(sample)
    asset = digest(sample)
    project = temp / "projects with spaces" / "kit one"
    project.parent.mkdir()
    view = scripted("import-save", [
        {"t": 0.1, "type": "click", "key": "import-toggle"},
        {"t": 0.4, "type": "click", "key": "import-path"},
        {"t": 0.5, "type": "type", "text": str(sample)},
        {"t": 5.0, "type": "click", "key": "import-submit"},
        {"t": 8.0, "type": "click", "key": "import-cancel"},
        {"t": 8.3, "type": "click", "key": "projects-toggle"},
        {"t": 8.6, "type": "click", "key": "projects-path"},
        {"t": 8.7, "type": "type", "text": str(project)},
        {"t": 13.0, "type": "click", "key": "projects-save"}], 60)
    status = view.get("projects-status", "")
    require(not view.get("projects-error") and status.startswith("Saved generation "),
            f"Packaged save failed: {view.get('projects-error') or status}")
    generation = status.split()[2].rstrip(".")
    document = json.loads((project / "project.json").read_text())
    require(document["pads"][0]["sample"]["source"] == asset
            and (project / "media" / f"{asset}.wav").read_bytes() == sample.read_bytes(),
            "Saved project does not own the imported sample")
    sample.unlink()  # The project, not the user's original file, must supply it.
    moved = temp / "moved projects" / "kit one"
    moved.parent.mkdir()
    project.rename(moved)
    relocated = temp / "relocated after save" / LAYOUT
    shutil.copytree(layout, relocated)
    shutil.rmtree(layout)  # Owned temporary installation only.
    layout = relocated
    opened("reopen-moved", moved, generation)
    view = scripted("reopen-play", open_events(moved) + [
        {"t": 40.0, "type": "click", "key": "projects-toggle"},
        {"t": 40.5, "type": "key", "name": "1"}], 40.6)
    require(view.get("selected-name") == f"{asset}.wav" and view.get("pad-0-voices") == "▶ 1",
            f"Reopened custom pad did not play: {view.get('selected-name')!r} {view.get('pad-0-voices')!r}")
    record["checks"].append("Packaged UI: import custom WAV, save, delete source, move project and installation, "
                            "quit/reopen, custom pad plays from project media (silent mixer)")

    read_only = temp / "read-only copy" / "kit one"
    shutil.copytree(moved, read_only)
    before = snapshot(read_only)
    set_writable(read_only, False)
    try:
        rejected("failed-save", open_events(read_only) + [
            {"t": 30.0, "type": "click", "key": "projects-save"}], 60, "saved document unchanged")
    finally:
        set_writable(read_only, True)
    require(snapshot(read_only) == before, "Failed save changed project files")
    opened("after-failed-save", read_only, generation)
    record["checks"].append("Packaged UI: failed save into a read-only project reports unchanged; "
                            "files byte-identical and the last generation reopens")

    missing = temp / "missing media copy" / "kit one"
    shutil.copytree(moved, missing)
    (missing / "media" / f"{asset}.wav").unlink()
    rejected("missing-media", open_events(missing), 40, "Missing")
    view = scripted("missing-media-play", open_events(missing) + [
        {"t": 40.0, "type": "click", "key": "projects-toggle"},
        {"t": 40.5, "type": "key", "name": "1"}], 40.6)
    require(view.get("selected-name") != f"{asset}.wav" and view.get("pad-0-voices") == "▶ 1",
            "Rejected missing-media project replaced or silenced the current kit")
    record["checks"].append("Packaged UI: project with missing media is rejected whole; current kit keeps playing")

    corrupt = temp / "corrupt document copy" / "kit one"
    shutil.copytree(moved, corrupt)
    (corrupt / "project.json").write_text("{not json")
    rejected("corrupt-open", open_events(corrupt), 40, "MalformedJson")
    listing = open_events(corrupt)[:3] + [{"t": 5.0, "type": "click", "key": "projects-recover"}]
    view = scripted("recovery-list", listing, 40)
    choice = f"projects-generation-{generation}"
    require(choice in view, f"Recovery did not offer generation {generation}: {view.get('projects-error')}")
    view = scripted("recovery-open", listing + [{"t": 40.0, "type": "click", "key": choice}], 80)
    require(view.get("projects-status", "").startswith(f"Opened generation {generation}."),
            f"Explicit recovery failed: {view.get('projects-error') or view.get('projects-status')}")
    require((corrupt / "project.json").read_text() == "{not json", "Recovery overwrote the corrupt original")
    record["checks"].append("Packaged UI: corrupt document rejected; recovery lists and opens the committed "
                            "generation explicitly without overwriting the original")
    return layout

def smoke(artifact):
    mac_arm()
    artifact = Path(artifact).resolve(strict=True)
    record = {"artifact": artifact.name, "sha256": digest(artifact), "status": "failed",
              "mode": "Actual release executable, Rune GPU headless, silent virtual mixer",
              "os": platform.mac_ver()[0], "architecture": platform.machine(), "checks": [],
              "not_proven": ["Clean-machine/Gatekeeper install", "Audible audio/listening",
                             "Real device loss/recovery", "Audible playback of a reopened project",
                             "Live or packaged pedal renders"]}
    try:
        expected = artifact.with_name(artifact.name + ".sha256").read_text().split()
        require(len(expected) == 2 and expected[1] == artifact.name
                and expected[0] == record["sha256"], "Artifact/checksum mismatch")
        with tempfile.TemporaryDirectory(prefix="brownnote-relocation-") as temp:
            temp = Path(temp)
            original = extract(artifact, temp / "temporary source fixtures")
            build = json.loads((original / "build-record.json").read_text())
            require(digest(original / "brownnote-native") == build["binary_sha256"],
                    "Packaged executable does not match build record")
            require(not (original / "tools").exists(), "Base-pad artifact unexpectedly includes tools")
            for name, expected_hash in build["kit_sha256"].items():
                require(name in FILES and digest(original / "resources/recordings" / name) == expected_hash,
                        f"Kit checksum mismatch: {name}")
            require(set(build["kit_sha256"]) == set(FILES), "Incomplete kit hash inventory")
            record["build"] = build
            cwd = temp / "unrelated working directory"
            cwd.mkdir()
            # A wrong cwd kit must not supply resources. Keep it invalid on purpose.
            (cwd / "recordings").mkdir()
            for name in FILES:
                (cwd / "recordings" / name).write_bytes(b"not a WAV")
            home = temp / "isolated user home"
            home.mkdir()
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(("BROWNNOTE_", "DYLD_"))}
            env["HOME"] = str(home)
            env["TMPDIR"] = str(temp)
            script = temp / "all sources.json"
            script.write_text(json.dumps([
                {"t": 0.02, "type": "key", "name": key}
                for key in ["1", "q", "a", "d", "z"]]))

            def app(layout, *args, code=0):
                result = subprocess.run([str(layout / "brownnote-native"), *map(str, args)],
                                        cwd=cwd, env=env, capture_output=True, text=True, timeout=120)
                require(result.returncode == code,
                        f"Packaged executable exit {result.returncode}, expected {code}:\n{result.stderr}")
                return result

            def tree(layout, name, *args):
                path = temp / f"{name}.json"
                app(layout, *args, "--tree", path)
                value = json.loads(path.read_text())
                require(not value["warnings"], f"Rune warnings: {value['warnings']}")
                return {e["key"]: e.get("text", "") for e in value["elements"] if "key" in e}

            previous = original
            for index in range(3):
                layout = previous if index == 0 else temp / f"relocated {index} with spaces" / LAYOUT
                if index:
                    shutil.copytree(previous, layout)
                    shutil.rmtree(previous)  # Owned temporary fixtures only, never a real checkout.
                    require(not previous.exists(), "Previous temporary installation still exists")
                view = tree(layout, f"playing-{index}", "--script", script, "--at", "0.08")
                require("asset-errors" not in view, f"Bundled kit failed: {view.get('asset-errors')}")
                require(view["audio-status"] == "Headless: no audio device", "Expected virtual mixer")
                for pad in [0, 4, 8, 10, 12]:
                    require(view[f"pad-{pad}-voices"] == "▶ 1", f"Source pad {pad} did not play")
                require(view["meter"].endswith(" dB") and view["meter"] != "−∞ dB",
                        "No virtual mixer signal")
                result = app(layout, "--list-pedals", code=3)
                require("tools/brownnote" in result.stderr
                        and "Pedals need brownnote" in result.stderr
                        and "Base pads still play." in result.stderr,
                        f"Missing pedal error was not explicit: {result.stderr}")
                record["checks"].append(f"Location {index + 1}: five sources, virtual signal, no overrides, missing-pedal exit 3")
                previous = layout
            previous = project_workflow(tree, previous, temp, record)
            # Prove no hidden checkout/cwd fallback, not just success with some kit.
            shutil.rmtree(previous / "resources/recordings")
            missing = tree(previous, "missing", "--scene", "playing", "--at", "0.08")
            require("asset-errors" in missing, "Missing bundled samples silently fell back")
            require(all(not missing[f"pad-{p}-voices"] for p in [0, 4, 10]),
                    "Missing kit still played from an undeclared location")
            record["checks"].append("Removed only temporary bundled kit: errors, disabled voices, no checkout/cwd fallback")
            for scene in ["output-selection", "output-missing", "output-invalid"]:
                view = tree(previous, scene, "--scene", scene, "--size", "820x600")
                require("output-panel" in view and view["output-status"].startswith("SILENT"),
                        f"Output fixture scene failed: {scene}")
                if scene == "output-invalid":
                    require(view.get("output-error"), "Invalid output fixture did not report an error")
            record["checks"].append("Three deterministic output UI scenes; NOT hardware recovery")
            # The current app does not consume Recovery, or create Cache without a
            # render. These are filesystem probes, not production store evidence.
            for relative in ["Library/Caches/Brownnote/pedals",
                             "Library/Application Support/Brownnote/recovery",
                             "Library/Application Support/Brownnote/preferences"]:
                directory = home / relative
                directory.mkdir(parents=True, exist_ok=True)
                probe = directory / "package-filesystem-write-probe"
                probe.write_bytes(b"writable")
                require(probe.read_bytes() == b"writable", f"Not writable: {relative}")
                probe.unlink()
            record["checks"].append("Documented user locations writable: filesystem probes only, not saves")
            record["status"] = "passed"
    except Exception as error:
        record["error"] = str(error)
        raise
    finally:
        report = artifact.with_name(artifact.name + ".smoke.json")
        write_json(report, record)
        print(json.dumps(record, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("package").add_argument("output_directory", nargs="?", default=ROOT / "build/packages")
    commands.add_parser("smoke").add_argument("artifact")
    arguments = parser.parse_args()
    if arguments.command == "package":
        package(arguments.output_directory)
    else:
        smoke(arguments.artifact)
