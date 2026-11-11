#!/usr/bin/env python3
"""Opt-in only. Bounce an adapter output's main mix from the actual pinned GUI.

Arguments: APP CLI SESSION FRAMES BOUNCE EVIDENCE. Opens SESSION in a fresh
no-audio GUI and bounces frames [0, FRAMES) of the main mix to BOUNCE as
32-bit float WAV, without dither or normalization. Uses #95's unmodified Probe
for process ownership; never touches the spike directory or a personal session.
"""
import hashlib
import json
from pathlib import Path
import sys

native = Path(__file__).resolve().parents[3]
pin = json.loads((native / "src/adapters/soundcraft/pin.json").read_text())
app, cli, session = (Path(x).resolve() for x in sys.argv[1:4])
frames = int(sys.argv[4])
bounce, evidence = (Path(x).resolve() for x in sys.argv[5:7])
for name, path in [("SoundCraft", app), ("soundcraft-cli", cli)]:
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    assert actual == pin["binary_sha256"][name], (name, "wrong pinned binary")
sys.path.insert(0, str(native / "docs/spikes/soundcraft"))
from probe import Probe  # noqa: E402

probe = Probe(app, cli, session.parent.parent, evidence)
try:
    probe.start()
    assert probe.call("session.open", {"path": str(session)})["missing"] == []
    assert probe.call("session.audio_health")["healthy"]
    probe.call("file.bounce_mix", {"path": str(bounce), "source": "main", "start": 0,
                                   "end": frames, "bit_depth": "32f", "dither": False,
                                   "normalize": False})
    probe.call("session.close")
finally:
    probe.stop()
assert all(r.get("app_exit", 0) == 0 and "shutdown_error" not in r for r in probe.records)
print(f"Actual-app bounce: {bounce}")
