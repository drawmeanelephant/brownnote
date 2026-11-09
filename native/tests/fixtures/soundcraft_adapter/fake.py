#!/usr/bin/env python3
"""Strict fake released CLI/app boundary. Never imports SoundCraft code."""
import json
import os
from pathlib import Path
import shutil
import sys
import time
import wave

root = Path(__file__).resolve().parent
config = json.loads((root / "config.json").read_text())
args = sys.argv[1:]
role = Path(sys.argv[0]).name


def log(record):
    with (root / "calls.jsonl").open("a") as out:
        out.write(json.dumps(record) + "\n")


log({"role": role, "args": args})
if args == ["--version"]:
    print(f"{role} {config.get(role + '_version', '0.19.0')} (macos aarch64)")
    sys.exit(0)

if role == "soundcraft":
    assert args == ["--no-audio", "--control", "0"], args
    assert os.environ["SOUNDCRAFT_NO_PREFS"] == "1"
    profile = Path(os.environ["SOUNDCRAFT_CONFIG_DIR"])
    assert profile.name == "profile"
    profile.mkdir(exist_ok=True)
    log({"launch_pid": os.getpid(), "profile": str(profile)})
    (root / "stop").unlink(missing_ok=True)
    (root / "state.json").write_text(json.dumps({}))
    print("SOUNDCRAFT_CONTROL_PORT=31415", flush=True)
    while not (root / "stop").exists():
        time.sleep(0.01)
    sys.exit(0)

assert args[:3] == ["app", "--port", "31415"], args
assert len(args) == 5, args
method, params = args[3], json.loads(args[4])
log({"method": method, "params": params})
if config.get("fail") == method:
    print("injected partial failure", file=sys.stderr)
    sys.exit(17)
if config.get("malformed") == method:
    print("not json")
    sys.exit(0)
if config.get("envelope_error") == method:
    print(json.dumps({"ok": False, "error": "injected protocol failure"}))
    sys.exit(0)
if config.get("hang") == method:
    time.sleep(60)

state = json.loads((root / "state.json").read_text())
reply = {}


def ident():
    state["next"] += 1
    return state["next"]


def track(tid):
    return next(t for t in state["tracks"] if t["id"] == tid)


if method == "ui.key":
    assert params == {"key": "Escape"}
elif method == "ui.inspect":
    reply = {"audio_device": None, "playing": False,
             "edit_layout": {"timeline": [0, 0, 800, 600]}}
elif method == "session.new":
    assert params["sample_rate"] == 48000
    state = {"next": 0, "tracks": [], "sources": [], "notes": {},
             "sample_rate": 48000, "tempo": []}
elif method == "event.tempo":
    assert params["at"] == 0
    state["tempo"] = [{"bpm": params["bpm"], "tick": 0}]
elif method == "file.import_audio":
    assert set(params) == {"path", "at", "new_tracks"}
    assert params["new_tracks"] is True  # Same-track import is destructive.
    with wave.open(params["path"]) as wav:
        assert wav.getframerate() == 48000
        frames = wav.getnframes()
    tid, cid, sid = ident(), ident(), ident()
    state["tracks"].append({"id": tid, "kind": "audio", "mute": False,
                            "clips": [{"id": cid, "start": params["at"],
                                       "length": frames, "source": sid}]})
    state["sources"].append({"id": sid, "path": params["path"], "loaded": True})
    reply = {"imported": [{"track": tid, "clip": cid, "frames": frames}]}
elif method == "track.new":
    assert params["kind"] == "instrument"
    tid = ident()
    state["tracks"].append({"id": tid, "kind": "instrument", "mute": False,
                            "clips": []})
    reply = {"tracks": [tid]}
elif method == "mix.mute":
    assert set(params) == {"tracks", "value"} and params["value"] is True
    for tid in params["tracks"]:
        track(tid)["mute"] = True
elif method == "midi.new_clip":
    assert params["start"] == 0
    cid = ident()
    track(params["track"])["clips"].append(
        {"id": cid, "start": 0, "length": params["length"]})
    state["notes"][str(cid)] = []
    reply = {"clip": cid}
elif method == "midi.note_add":
    assert set(params) == {"clip", "pitch", "channel", "velocity",
                           "start_ticks", "length_ticks"}
    state["notes"][str(params["clip"])].append({
        "start": params["start_ticks"], "length": params["length_ticks"],
        "pitch": params["pitch"], "channel": params["channel"],
        "velocity": params["velocity"], "release_velocity": 64})
elif method == "mix.sampler_load":
    assert set(params) == {"track", "path", "params"}
    assert set(params["params"]) == {"root_note", "key_low", "key_high",
                                    "key_track", "play_mode"}
    assert Path(params["path"]).is_file()
    track(params["track"])["instrument"] = params
    state["sources"].append({"id": ident(), "path": params["path"], "loaded": True})
elif method == "session.save_as":
    session = Path(params["path"])
    audio = session.parent / "Audio Files"
    audio.mkdir(exist_ok=True)
    for source in state["sources"]:
        name = Path(source["path"]).name
        shutil.copyfile(source["path"], audio / name)
        source["path"] = "Audio Files/" + name
    session.write_text(json.dumps(state))
elif method == "session.open":
    state = json.loads(Path(params["path"]).read_text())
    reply = {"missing": []}
    if config.get("corrupt") == "position":
        state["tracks"][0]["clips"][0]["start"] += 1
    if config.get("corrupt") == "notes":
        next(iter(state["notes"].values()))[0]["length"] -= 1
    if config.get("corrupt") == "media":
        state["sources"][0]["path"] = "/outside.wav"
    if config.get("corrupt") == "mute":
        state["tracks"][-1]["mute"] = False
elif method == "session.inspect":
    assert params == {"detail": "full"}
    reply = state
elif method == "midi.notes":
    reply = {"notes": state["notes"][str(params["clip"])]}
elif method == "session.audio_health":
    reply = {"healthy": True}
elif method == "app.quit":
    (root / "stop").touch()
else:
    raise AssertionError(f"unproven command: {method}")
(root / "state.json").write_text(json.dumps(state))
print(json.dumps({"ok": True, "result": reply}))
