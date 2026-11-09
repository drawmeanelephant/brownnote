#!/usr/bin/env python3
"""Opt-in only. Edit adapter output in the actual pinned GUI, then reopen it.

Arguments: APP CLI OUTPUT BUNDLE EVIDENCE. Uses #95's unmodified Probe for
process ownership; never touches the spike directory or a personal session.
"""
import hashlib
import json
from pathlib import Path
import shutil
import sys

native = Path(__file__).resolve().parents[3]
pin = json.loads((native / "src/adapters/soundcraft/pin.json").read_text())
app, cli, output, bundle, evidence = (Path(x).resolve() for x in sys.argv[1:6])
for name, path in [("SoundCraft", app), ("soundcraft-cli", cli)]:
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    assert actual == pin["binary_sha256"][name], (name, "wrong pinned binary")
sys.path.insert(0, str(native / "docs/spikes/soundcraft"))
from probe import Probe  # noqa: E402

report = json.loads((output / "adapter-report.json").read_text())
assert len(report["audio"]) >= 2 and report["midi"], "needs two hits and one MIDI part"
work = output.parent
probe = Probe(app, cli, work, evidence)


def notes(clip):
    return probe.call("midi.notes", {"clip": clip})["notes"]


def session_state():
    state = probe.call("session.inspect", {"detail": "full"})
    return state, {str(m["clip"]): notes(m["clip"]) for m in report["midi"]}


def reopen(path):
    probe.stop()
    probe.start()
    return probe.call("session.open", {"path": str(path)})["missing"]


try:
    probe.records.append({"adapter_report": {
        "soundcraft_commit": report["soundcraft_commit"],
        "audio_clips": len(report["audio"]),
        "midi_notes": sum(len(m["notes"]) for m in report["midi"]),
        "record_kinds": sorted({r["kind"] for r in report["records"]})}})
    probe.start()
    assert probe.call("session.open", {"path": str(output / "session.scraft")})["missing"] == []
    moved, deleted = report["audio"][0], report["audio"][1]
    midi = report["midi"][0]
    probe.call("edit.move_clips", {"clips": [moved["clip"]], "to": moved["start"] + 2400})
    probe.call("edit.select", {"clips": [deleted["clip"]]})
    probe.call("edit.clear")
    original = notes(midi["clip"])[0]
    edited = {"pitch": min(original["pitch"] + 2, 127), "start_ticks": original["start"] + 120,
              "length_ticks": original["length"] + 240, "velocity": 101}
    probe.call("midi.note_edit", {"clip": midi["clip"], "index": 0, **edited})
    before, before_notes = session_state()
    clips = {c["id"]: c for t in before["tracks"] for c in t["clips"]}
    assert clips[moved["clip"]]["start"] == moved["start"] + 2400
    assert deleted["clip"] not in clips
    assert any((n["pitch"], n["start"], n["length"], n["velocity"]) ==
               (edited["pitch"], edited["start_ticks"], edited["length_ticks"], 101)
               for n in before_notes[str(midi["clip"])]), before_notes
    for other in report["audio"][2:]:
        assert (clips[other["clip"]]["start"], clips[other["clip"]]["length"]) == \
            (other["start"], other["length"])
    probe.call("session.save_as", {"path": str(output / "session.scraft")})
    probe.records.append({"assertion": "Moved one hit, deleted another and edited one note in the actual GUI, then saved"})

    assert reopen(output / "session.scraft") == []
    after, after_notes = session_state()
    assert after["tracks"] == before["tracks"] and after_notes == before_notes
    assert probe.call("session.audio_health")["healthy"]
    probe.records.append({"assertion": "PASS: edits survived quit/reopen"})
    probe.call("session.close")

    relocated = work / "relocated adapter output"
    shutil.copytree(output, relocated)
    shutil.rmtree(output)
    shutil.rmtree(bundle)
    probe.records.append({"filesystem": "Copied the entire adapter output; removed the original output and the source bundle"})
    assert reopen(relocated / "session.scraft") == []
    moved_state, moved_notes = session_state()
    assert moved_state["tracks"] == before["tracks"] and moved_notes == before_notes
    assert moved_state["tempo"] == before["tempo"] and moved_state["sample_rate"] == 48000
    assert moved_state["sources"] and all(
        s["loaded"] and s["path"].startswith("Audio Files/") for s in moved_state["sources"])
    assert probe.call("session.audio_health")["healthy"]
    probe.records.append({"assertion": "PASS: edited adapter session reopened after source-free relocation"})
    probe.call("session.close")

    # Remove media a remaining clip plays; the deleted clip's file is unused.
    source = next(c["source"] for t in moved_state["tracks"] for c in t["clips"]
                  if c["id"] == moved["clip"])
    victim = next(s["path"] for s in moved_state["sources"] if s["id"] == source)
    (relocated / victim).unlink()
    probe.records.append({"filesystem": "Removed the relocated WAV used by the moved clip for the negative control"})
    assert reopen(relocated / "session.scraft"), "negative control must report missing media"
    assert not probe.call("session.audio_health")["healthy"]
    probe.records.append({"assertion": "PASS: missing-media negative control detected"})
finally:
    probe.stop()
assert all(r.get("app_exit", 0) == 0 and "shutdown_error" not in r for r in probe.records)
print(f"Actual-app adapter edit evidence: {evidence}")
