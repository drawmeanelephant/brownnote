#!/usr/bin/env python3
"""Opt-in only. Reuse #95's unmodified probe in a separate scratch directory."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

native = Path(__file__).resolve().parents[3]
pin = json.loads((native / "src/adapters/soundcraft/pin.json").read_text())
for name, path in [("SoundCraft", sys.argv[1]), ("soundcraft-cli", sys.argv[2])]:
    actual = hashlib.sha256(Path(path).read_bytes()).hexdigest()
    assert actual == pin["binary_sha256"][name], (name, "wrong pinned binary")

spike = native / "docs/spikes/soundcraft"
sys.path.insert(0, str(spike))
from probe import Probe, scenario  # noqa: E402
from check_fixtures import check  # noqa: E402

# No change to the spike, its pin, scripts, evidence, or .work directory.
work = Path(tempfile.mkdtemp(prefix="brownnote-soundcraft-pinned-"))
sources = work / "sources"
shutil.copytree(native / "tests/fixtures/soundcraft", sources)
output = Path(os.environ.get("BROWNNOTE_SOUNDCRAFT_EVIDENCE", str(work / "app.json")))
probe = Probe(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(), work, output)
try:
    probe.records.append({"independent_fixture_checks": check(sources),
                          "adapter_pin": pin["commit"]})
    probe.start()
    scenario(probe, sources, work)
finally:
    probe.stop()
assert all(r.get("app_exit", 0) == 0 and "shutdown_error" not in r for r in probe.records)
print(f"Actual-app recipe evidence: {output}")
