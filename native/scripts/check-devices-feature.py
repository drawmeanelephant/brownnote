#!/usr/bin/env python3
"""Bootstrap check only: stage integrator wiring in an owned target directory.

Never edits the real shared files. Run from any directory:
  python3 native/scripts/check-devices-feature.py focused
  python3 native/scripts/check-devices-feature.py full
This does not replace make native-test on the integrator's final tree.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys

NATIVE = Path(__file__).resolve().parents[1]
ROOT = NATIVE / "target" / "devices-feature-bootstrap"
MARKER = ROOT / ".devices-bootstrap-owned"


def replace(path, before, after):
    body = path.read_text()
    if body.count(before) != 1:
        raise RuntimeError(f"Integrator seam changed in {path.name}: {before!r}")
    path.write_text(body.replace(before, after))


def prepare():
    if ROOT.exists():
        if not MARKER.is_file():
            raise RuntimeError("Refusing to replace an unowned bootstrap directory")
        shutil.rmtree(ROOT)
    ROOT.mkdir(parents=True)
    MARKER.touch()
    for name in ("src", "tests", "scripts"):
        shutil.copytree(NATIVE / name, ROOT / name)
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copyfile(NATIVE / name, ROOT / name)
    # Compile-time default sample/bin paths still refer to this checkout,
    # not the bootstrap's parent. No files in the source tree are modified.
    for folder in ("src", "tests"):
        for path in (ROOT / folder).rglob("*.rs"):
            body = path.read_text()
            body = body.replace('env!("CARGO_MANIFEST_DIR")', f'"{NATIVE}"')
            path.write_text(body)
    # Reuse the regular serde/serde_json dependencies landed by project PR120.
    ui = ROOT / "src" / "ui.rs"
    replace(ui, "mod audio;", "mod audio;\npub(crate) mod devices;")
    replace(ui, "    pub pedals: pedals::Pedals,",
            "    pub pedals: pedals::Pedals,\n    pub devices: devices::Devices,")
    replace(ui, "            pedals,\n", "            pedals,\n            devices,\n")
    old_output = '''        let output = if headless || no_audio {
            Output::virtual_driver(
                slots.clone(),
                if headless {
                    "Headless: no audio device".into()
                } else {
                    "Audio off (--no-audio)".into()
                },
                false,
            )
        } else {
            Output::open(slots.clone())
                .unwrap_or_else(|e| Output::virtual_driver(slots.clone(), e.to_string(), true))
        };'''
    new_output = f'''        let devices = if headless || no_audio {{
            devices::Devices::disabled()
        }} else {{
            devices::Devices::load(std::path::PathBuf::from("{ROOT / "preferences"}"))
        }};
        let output = devices.initial_output(slots.clone(), if headless {{
            "Headless: no audio device".into()
        }} else {{ "Audio off (--no-audio)".into() }});'''
    replace(ui, old_output, new_output)
    replace(ui, "    let failed = s.recover_stream_error();",
            "    let failed = s.recover_stream_error();\n"
            "    let devices_changed = devices::poll(s);\n"
            "    if s.devices.busy() { tx.wake_after(Duration::from_millis(1)); }")
    replace(ui, "    let changed = failed", "    let changed = devices_changed || failed")
    replace(ui, '    ("playing", |s| {',
            '    ("output-selection", |s| devices::scene(s, devices::SceneKind::Selection)),\n'
            '    ("output-missing", |s| devices::scene(s, devices::SceneKind::Missing)),\n'
            '    ("output-invalid", |s| devices::scene(s, devices::SceneKind::Invalid)),\n'
            '    ("playing", |s| {')
    view = ROOT / "src" / "ui" / "view.rs"
    replace(view, "    ink, pedals,", "    ink, pedals, devices,")
    replace(view, '    if s.output.failed {', '    controls = controls.child(devices::header(s));\n    if s.output.failed {')
    replace(view, 'button("retry-audio", "RETRY AUDIO")',
            'button("retry-audio", "RETRY AUDIO").disabled(s.devices.busy()).focusable(!s.devices.busy())')
    replace(view, "        .child(grid)\n        .child(editor(s))",
            "        .children(if s.devices.visible { vec![devices::panel(s)] } else { vec![grid, editor(s)] })")
    # Startup's placeholder machine root is owned by this bootstrap. Headless
    # callers never load it; the integrator supplies the real path policy.
    return ROOT / "Cargo.toml"


def main():
    mode = sys.argv[1] if len(sys.argv) == 2 else ""
    if mode not in ("focused", "full"):
        raise SystemExit("usage: check-devices-feature.py focused|full")
    manifest = prepare()
    env = os.environ.copy()
    env["CARGO_BUILD_JOBS"] = "1"
    env["CARGO_TARGET_DIR"] = str(NATIVE / "target" / "devices-feature-build")
    env["PATH"] = "/opt/homebrew/opt/rustup/bin:" + env.get("PATH", "")
    base = ["cargo"]
    commands = (
        [["test", "--bin", "brownnote-native", "ui::devices::"]]
        if mode == "focused" else
        [["clippy", "--all-targets", "--", "-D", "warnings"],
         ["test", "--", "--include-ignored"]]
    )
    print("FEATURE/BOOTSTRAP CHECK ONLY, integrator wiring is staged in target/", flush=True)
    for command in commands:
        args = base + [command[0], "--manifest-path", str(manifest)] + command[1:]
        subprocess.run(args, env=env, cwd=NATIVE.parent, check=True)


if __name__ == "__main__":
    main()
