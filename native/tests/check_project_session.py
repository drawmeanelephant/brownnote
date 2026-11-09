#!/usr/bin/env python3
"""Test exact proposed project UI wiring without editing integrator-owned files.

Run inside the swarm's whole-check lock with its shared CARGO_TARGET_DIR.
The generated integration target mounts the existing State/media/pedals/output
and the proposed module, field, poll, key and panel wiring. It is a probe, not
evidence that the shipped binary already mounts this feature.
"""
import os
from pathlib import Path
import re
import subprocess
import time

root = Path(__file__).resolve().parents[2]
source = root / "native/src/ui.rs"
mounted = root / "native/tests/project_session_mounted.rs"
host = root / "native/tests/project_session_host.rs.inc"
pedal_host = root / "native/tests/project_session_pedals.rs.inc"
device_host = root / "native/tests/project_session_devices.rs.inc"


def run(*args):
    print("$ " + " ".join(args), flush=True)
    result = subprocess.run(args, cwd=root, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        raise SystemExit(result.returncode)
    return result.stdout


def once(text, before, after):
    if text.count(before) != 1:
        raise SystemExit("Wiring anchor changed: " + before)
    return text.replace(before, after, 1)


def require_tests(output):
    names = re.findall(r"(?m)^fn (project_controls_\w+)\(", (
        root / "native/src/ui/projects/tests.rs").read_text())
    for name in names:
        if not re.search(r"(?m)^test [^\n]*::" + name + r" \.\.\. ok$", output):
            raise SystemExit("Stale/missing mounted project fixture: " + name)
    if not names:
        raise SystemExit("No mounted control fixtures")
    print(f"Verified {len(names)} checkout-local project control fixtures", flush=True)


if any(path.exists() for path in [mounted, host, pedal_host, device_host]):
    raise SystemExit("Refusing to overwrite an existing mounting probe")

text = source.read_text()
for name in ["audio", "control", "devices", "graphics", "media", "pedals", "view"]:
    path = root / "native/src/ui" / (name + ".rs")
    if not path.exists():
        path = root / "native/src/ui" / name / "mod.rs"
    visibility = "pub(crate) " if name in ["devices", "pedals"] else ""
    if name == "pedals":
        path = pedal_host
    if name == "devices":
        path = device_host
    text = once(text, f"{visibility}mod {name};",
                f'#[path = "{path}"]\n{visibility}mod {name};')
text = once(text, '#[path = "ui_import_tests.rs"]',
            f'#[path = "{root / "native/src/ui_import_tests.rs"}"]')
text = once(text, "mod audio;", 'mod audio;\n'
            f'#[path = "{root / "native/src/ui/projects.rs"}"]\npub mod projects;')
text = once(text, "pub struct State {", "pub struct State {\n    pub projects: projects::Projects,")
text = once(text, "            output,\n", "            projects: projects::Projects::default(),\n            output,\n")
text = once(text, "    let elapsed = (clock - s.now).max(0.0);",
            "    let projects_changed = projects::poll(s);\n"
            "    if s.projects.busy() { tx.wake_after(Duration::from_millis(10)); }\n"
            "    let elapsed = (clock - s.now).max(0.0);")
text = once(text, "    let changed = devices_changed",
            "    let changed = projects_changed || devices_changed")
text = once(text, "    if event.mods.cmd", "    if projects::on_key(s, event, tx) { return true; }\n    if event.mods.cmd")
text = once(text, "pub use view::view;", """pub fn view(s: &State, cx: &mut rune::Cx<State>) -> El {
    if s.projects.visible { projects::panel(s) } else { view::view(s, cx) }
}""")

try:
    device_text = (root / "native/src/ui/devices.rs").read_text()
    for name in ["preferences", "tests", "view"]:
        device_text = once(device_text, f"mod {name};",
                           f'#[path = "{root / "native/src/ui/devices" / (name + ".rs")}"]\nmod {name};')
    device_host.write_text(device_text)
    pedal_text = (root / "native/src/ui/pedals/mod.rs").read_text()
    pedal_text = once(pedal_text, "mod view;",
                      f'#[path = "{root / "native/src/ui/pedals/view.rs"}"]\nmod view;')
    pedal_text = once(pedal_text, 'include_str!("../../../tests/fixtures/bench-list.txt")',
                      f'include_str!("{root / "native/tests/fixtures/bench-list.txt"}")')
    pedal_text += f'\ninclude!("{root / "native/src/ui/projects/pedal_state.rs"}");\n'
    pedal_host.write_text(pedal_text)
    host.write_text(text)
    mounted.write_text(f'''#![allow(dead_code)]
#[path = "{host}"]
mod ui;
#[cfg(test)]
#[path = "{root / "native/src/ui_test_support.rs"}"]
mod ui_test_support;
''')
    # Shared artifacts may be newer than this checkout's source mtimes.
    for relative in ["native/src/lib.rs", "native/src/main.rs",
                     "native/src/project/session.rs", "native/src/kit.rs"]:
        os.utime(root / relative, None)
    run("git", "rev-parse", "HEAD")
    run("cargo", "rustc", "--manifest-path", "native/Cargo.toml", "--lib", "--",
        f"-Cmetadata=brownnote_project_session_{time.time_ns()}")
    require_tests(run("cargo", "test", "--manifest-path", "native/Cargo.toml",
                      "--test", "project_session_mounted", "project_controls_", "--",
                      "--nocapture"))
    run("cargo", "clippy", "--manifest-path", "native/Cargo.toml",
        "--test", "project_session_mounted", "--", "-D", "warnings")
    # The temporary harness has generated absolute paths and formatting. It is
    # deliberately excluded from cargo fmt, but linted/tested above as real code.
finally:
    mounted.unlink(missing_ok=True)
    host.unlink(missing_ok=True)
    pedal_host.unlink(missing_ok=True)
    device_host.unlink(missing_ok=True)

run("make", "native-test")
