#!/usr/bin/env python3
"""Drive only an app started here. Never use headless `run` as app evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import shutil
import subprocess
import tempfile
import threading

import soundfile
import mido

from check_fixtures import check


class Probe:
    def __init__(self, app, cli, work, output):
        self.app, self.cli, self.work = app, cli, work
        self.output = output
        self.process = None
        self.records = []

    def clean(self, value):
        text = json.dumps(value)
        text = text.replace(str(self.work), "<RUN>")
        text = text.replace(str(Path(__file__).resolve().parents[4]), "<WORKTREE>")
        text = text.replace(str(Path.home()), "<HOME>")
        text = re.sub(r"127\.0\.0\.1:\d+", "127.0.0.1:<PORT>", text)
        return json.loads(text)

    def start(self):
        self.port = None
        env = {**os.environ, "SOUNDCRAFT_NO_PREFS": "1",
               "SOUNDCRAFT_CONFIG_DIR": str(self.work / "profile")}
        self.process = subprocess.Popen(
            [str(self.app), "--no-audio", "--control", "0"],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        lines = queue.Queue()
        self.log = []

        def read():
            for line in self.process.stdout:
                self.log.append(line.rstrip())
                lines.put(line)
            lines.put(None)

        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        while True:
            line = lines.get(timeout=45)
            if line is None:
                raise RuntimeError(f"GUI exited before control ready: {self.log}")
            match = re.search(r"SOUNDCRAFT_CONTROL_PORT=(\d+)", line)
            if match:
                self.port = match[1]
                break
        self.records.append({"launch": ["SoundCraft", "--no-audio", "--control", "0"],
                             "environment": {"SOUNDCRAFT_NO_PREFS": "1",
                                             "SOUNDCRAFT_CONFIG_DIR": "<RUN>/profile"}})
        self.call("ui.key", {"key": "Escape"})
        ui = self.call("ui.inspect")
        assert ui["audio_device"] is None and not ui["playing"]
        assert ui["edit_layout"]["timeline"][2] > 0, "No GUI layout pass"

    def call(self, method, params=None, expect_error=False):
        params = params or {}
        result = subprocess.run(
            [str(self.cli), "app", "--port", self.port, method, json.dumps(params)],
            capture_output=True, text=True, timeout=30)
        record = {"method": method, "params": params, "exit": result.returncode,
                  "stderr": result.stderr}
        try:
            record["reply"] = json.loads(result.stdout)
        except json.JSONDecodeError:
            record["stdout"] = result.stdout
        self.records.append(record)
        if expect_error:
            assert result.returncode != 0, record
            return record
        assert result.returncode == 0, record
        reply = record["reply"]
        if isinstance(reply, dict) and "ok" in reply:
            assert reply["ok"], reply
            return reply["result"]
        return reply

    def stop(self):
        if self.process is not None:
            if self.process.poll() is None:
                try:
                    if self.port is not None:
                        self.call("app.quit")
                    else:
                        self.process.terminate()
                    self.process.wait(timeout=15)
                except Exception as error:
                    self.records.append({"shutdown_error": str(error)})
                finally:
                    if self.process.poll() is None:
                        self.process.terminate()
                        try:
                            self.process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            self.process.kill()
                            self.process.wait(timeout=5)
            self.reader.join(timeout=5)
            self.records.append({"app_log": [
                re.sub(r"SOUNDCRAFT_CONTROL_PORT=\d+", "SOUNDCRAFT_CONTROL_PORT=<PORT>", x)
                for x in self.log], "app_exit": self.process.returncode})
            self.process = None
        self.output.write_text(json.dumps(self.clean(self.records), indent=2) + "\n")


def notes(probe, clip):
    return [tuple(n[k] for k in ("channel", "pitch", "start", "length", "velocity"))
            for n in probe.call("midi.notes", {"clip": clip})["notes"]]


def inspect(probe):
    return probe.call("session.inspect", {"detail": "full"})


def audio(probe, sources, name, at, **extra):
    return probe.call("file.import_audio", {
        "path": str(sources / name), "at": at, **extra})["imported"][0]


def read_pcm(path):
    values, rate = soundfile.read(path, always_2d=True)
    return rate, values[:, 0].tolist()


def variants(probe, sources, work):
    probe.call("session.new", {"name": "Sampler variants", "sample_rate": 48000})
    track = probe.call("track.new", {"kind": "instrument", "name": "Sampler"})["tracks"][0]
    probe.call("mix.sampler_load", {"track": track, "path": str(sources / "tone-44k1.wav"),
               "params": {"root_note": 60, "key_low": 60, "key_high": 72,
                          "play_mode": 1, "key_track": 1, "velocity": 100,
                          "attack": 0.1, "release": 1}})
    clip = probe.call("midi.new_clip", {"track": track, "start": 0, "length": 96000})["clip"]
    probe.call("midi.note_add", {"clip": clip, "pitch": 60, "start_ticks": 0,
                               "length_ticks": 60, "velocity": 127})
    results = {}
    for name, mode, pitch in [("one-shot", 1, 60), ("gate", 0, 60),
                              ("octave", 1, 72), ("outside-map", 1, 59)]:
        probe.call("mix.instrument_param", {"track": track, "param": "play_mode", "value": mode})
        probe.call("midi.note_edit", {"clip": clip, "index": 0, "pitch": pitch})
        path = work / f"{name}.wav"
        probe.call("file.bounce_mix", {"path": str(path), "start": 0, "end": 60000,
                                     "bit_depth": 16, "dither": False})
        rate, pcm = read_pcm(path)
        assert rate == 48000
        peak = max(abs(v) for v in pcm[9600:19200])
        crossings = sum(a < 0 <= b for a, b in zip(pcm[4800:19200], pcm[4801:19201]))
        results[name] = {"peak_0.2_to_0.4": peak, "crossings_0.1_to_0.4": crossings,
                         "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
    assert results["one-shot"]["peak_0.2_to_0.4"] > 0.1
    assert results["gate"]["peak_0.2_to_0.4"] == 0
    assert abs(results["one-shot"]["crossings_0.1_to_0.4"] - 132) <= 1
    assert abs(results["octave"]["crossings_0.1_to_0.4"] - 265) <= 1
    assert results["outside-map"]["peak_0.2_to_0.4"] == 0
    probe.call("mix.sampler_load", {"track": track, "path": str(sources / "tone-44k1.wav"),
                                   "params": {"choke_group": 1}}, expect_error=True)
    probe.records.append({"sampler_render_checks": results,
                          "scope": "Offline bounces requested from the real GUI, not listening"})


def scenario(probe, sources, work):
    # Demonstrate the destructive same-track case separately, not in the deliverable.
    probe.call("session.new", {"name": "Same-track overlap counterexample"})
    first = audio(probe, sources, "impulse-48k.wav", 0)
    audio(probe, sources, "impulse-48k.wav", 6000, track=first["track"])
    counterexample = inspect(probe)
    assert not any(c["start"] == 0 and c["length"] == 48000
                   for t in counterexample["tracks"] for c in t["clips"])
    probe.records.append({"assertion": "Same-track reimport does not retain the full first hit"})
    variants(probe, sources, work)
    probe.call("session.new", {"name": "Brownnote spike", "sample_rate": 48000})
    probe.call("event.tempo", {"bpm": 116})
    a = audio(probe, sources, "impulse-48k.wav", 0)
    b = audio(probe, sources, "impulse-48k.wav", 6000)
    tone = audio(probe, sources, "tone-44k1.wav", 24000)
    discarded = audio(probe, sources, "tone-44k1.wav", 96000)
    assert tone["frames"] == 48000
    midi_tracks = probe.call("file.import_midi", {
        "path": str(sources / "performance.mid"), "at": 48000})["tracks"]
    state = inspect(probe)
    midi_clips = [next(t for t in state["tracks"] if t["id"] == tid)["clips"][0]["id"]
                  for tid in midi_tracks]
    expected = [[(0, 60, 0, 960, 31), (0, 64, 480, 240, 79), (1, 67, 720, 1440, 113)],
                [(0, 60, 0, 960, 45), (0, 60, 240, 1200, 99)]]
    for clip, wanted in zip(midi_clips, expected):
        assert notes(probe, clip) == wanted
    exported = work / "same-pitch-export.mid"
    probe.call("file.export_midi", {"path": str(exported), "tracks": [midi_tracks[1]]})
    ticks, starts, exported_notes = 0, [], []
    for event in mido.MidiFile(exported).tracks[1]:
        ticks += event.time
        if event.type == "note_on" and event.velocity:
            starts.append(ticks)
        elif event.type == "note_off":
            start = starts.pop(0)
            exported_notes.append((start, ticks - start))
    # Clip starts at 48,000 samples = 1 s = 1,920 ticks at 120 BPM / 960 PPQ.
    assert exported_notes == [(1920, 240), (2160, 1200)], exported_notes
    probe.records.append({"independent_export_check": {
        "same_pitch_import_lengths": [960, 1200],
        "same_pitch_export_start_length": exported_notes,
        "limitation": "SoundCraft SMF export trims the earlier same-pitch overlap"}})
    assert state["tempo"] == [{"bpm": 120.0, "tick": 0}]
    probe.call("edit.move_clips", {"clips": [a["clip"]], "to": 12000})
    probe.call("edit.select", {"clips": [discarded["clip"]]})
    probe.call("edit.clear")
    probe.call("midi.note_edit", {"clip": midi_clips[0], "index": 1,
                                "pitch": 65, "start_ticks": 600, "length_ticks": 360,
                                "velocity": 88})
    expected[0][1] = (0, 65, 600, 360, 88)
    probe.call("mix.sampler_load", {"track": midi_tracks[0],
               "path": str(sources / "tone-44k1.wav"),
               "params": {"play_mode": 1, "key_track": 0, "root_note": 60}})
    for name, at in [("baked-octave.wav", 144000), ("baked-choke.wav", 168000)]:
        audio(probe, sources, name, at)
    before = inspect(probe)
    clips = {c["id"]: c for t in before["tracks"] for c in t["clips"]}
    assert clips[a["clip"]]["start"] == 12000
    assert clips[b["clip"]]["start"] == 6000
    assert discarded["clip"] not in clips
    for clip in midi_clips:
        assert clips[clip]["start"] == 48000
    session_dir = work / "original"
    session_dir.mkdir()
    session = session_dir / "spike.scraft"
    probe.call("session.save_as", {"path": str(session)})
    # Read the app's actual saved WAV media, not its private session JSON.
    media_facts = []
    expected_frames = {"impulse-48k.wav": 48000, "tone-44k1.wav": 48000,
                       "baked-octave.wav": 24000, "baked-choke.wav": 2400}
    for path in sorted((session_dir / "Audio Files").glob("*.wav")):
        rate, pcm = read_pcm(path)
        fact = {"name": path.name, "rate": rate, "frames": len(pcm)}
        assert rate == 48000
        assert len(pcm) == expected_frames[path.name]
        if "impulse" in path.name:
            nonzero = [i for i, v in enumerate(pcm) if v]
            assert nonzero == [0, 12000, 36000]
            fact["nonzero_frames"] = nonzero
        if path.name in ("tone-44k1.wav", "baked-octave.wav"):
            crossings = sum(a < 0 <= b for a, b in zip(pcm, pcm[1:]))
            assert crossings == 441, (path.name, crossings)
            fact["positive_crossings"] = crossings
        media_facts.append(fact)
    assert {f["name"] for f in media_facts} == expected_frames.keys()
    probe.records.append({"saved_media_checks": media_facts})
    probe.stop()
    probe.start()
    assert probe.call("session.open", {"path": str(session)})["missing"] == []
    reopened = inspect(probe)
    assert reopened["tracks"] == before["tracks"]
    for clip, wanted in zip(midi_clips, expected):
        assert notes(probe, clip) == wanted
    assert probe.call("session.audio_health")["healthy"]
    probe.call("session.close")
    relocated = work / "relocated"
    shutil.copytree(session_dir, relocated)
    shutil.rmtree(sources)
    shutil.rmtree(session_dir)
    probe.records.append({"filesystem": "Copied entire session directory; removed synthetic source directory and original session directory"})
    probe.stop()
    probe.start()
    assert probe.call("session.open", {"path": str(relocated / "spike.scraft")})["missing"] == []
    after = inspect(probe)
    assert after["tracks"] == before["tracks"]
    assert after["tempo"] == before["tempo"]
    assert after["sample_rate"] == 48000
    assert all(s["loaded"] for s in after["sources"])
    assert all(s["path"].startswith("Audio Files/") for s in after["sources"])
    for clip, wanted in zip(midi_clips, expected):
        assert notes(probe, clip) == wanted
    assert probe.call("session.audio_health")["healthy"]
    probe.records.append({"assertion": "PASS: edited positions, notes, media and tempo survived quit/reopen and source-free relocation"})
    probe.call("session.close")
    (relocated / "Audio Files/impulse-48k.wav").unlink()
    probe.records.append({"filesystem": "Removed only the relocated synthetic impulse WAV for the negative media-availability control"})
    probe.stop()
    probe.start()
    missing = probe.call("session.open", {"path": str(relocated / "spike.scraft")})["missing"]
    assert missing, "Negative control must detect the removed media"
    assert not probe.call("session.audio_health")["healthy"]
    probe.records.append({"assertion": "PASS: missing-media negative control is detected, not silently marked healthy"})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    work = Path(tempfile.mkdtemp(prefix="probe-", dir=root / ".work"))
    sources = work / "sources"
    shutil.copytree(root.parents[2] / "tests/fixtures/soundcraft", sources)
    probe = Probe(args.app.resolve(), args.cli.resolve(), work, args.output.resolve())
    try:
        probe.records.append({"independent_fixture_checks": check(sources)})
        probe.start()
        scenario(probe, sources, work)
    finally:
        probe.stop()
    assert all(r.get("app_exit", 0) == 0 and "shutdown_error" not in r for r in probe.records)


if __name__ == "__main__":
    main()
