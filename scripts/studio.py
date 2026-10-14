#!/usr/bin/env python3
"""
studio.py — Local HTTP studio backend for the Brownnote 16-Step Sequencer,
Stem Mixer, Jukebox, and Custom Pedalboard UI.

Serves build/studio.html at `/` plus workspace audio files and JSON APIs:
  GET  /api/state       -> available stems, preset MP3s, and effects list
  POST /api/render-fx   -> synthesize & pedal a custom phrase, return MP3/WAV URL
  POST /api/render-song -> render a full multi-track genre song or custom sequence to MP3

Extra views over the same stems (every page carries the same page-nav bar):
  /mixer    -> 6-channel console desk with VU meters, tap/loop per strip
  /pads     -> 16-pad MPC-style finger drummer (keys 1-4/QWER/ASDF/ZXCV)
  /library  -> crate-digger: every rendered MP3/WAV with drawn waveforms
"""
import http.server
import json
import math
import os
import shutil
import socketserver
import struct
import subprocess
import sys
import time
import urllib.parse
import wave
import array

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PORT = int(os.environ.get("STUDIO_PORT", "8765"))
HOST = os.environ.get("STUDIO_HOST", "127.0.0.1")

STEM_DEFS = [
    {"id": "kick",  "name": "Sub-Bass / Kick", "phrase": "bubu",                "effect": "Pitch Oct=22 + Klon + Cab", "url": "/build/tune/stem_bass_sub.wav",  "dryUrl": "/build/tune/stem_bass_dry.wav",  "color": "#ff6b35", "trimSec": 0.34, "defaultGain": 0.95, "defaultPan": 0.0,   "defaultPitch": 0},
    {"id": "snare", "name": "Splat Snare",     "phrase": "pa",                  "effect": "Comp + Boost + Reverb",     "url": "/build/tune/stem_snare.wav",     "dryUrl": "/build/tune/stem_snare_dry.wav", "color": "#f7c548", "trimSec": 0.22, "defaultGain": 0.88, "defaultPan": -0.10, "defaultPitch": 0},
    {"id": "hat",   "name": "Crispy Hi-Hat",   "phrase": "pa (+14 st)",         "effect": "Pitch +14st + Boost",       "url": "/build/tune/stem_snare.wav",     "dryUrl": "/build/tune/stem_snare_dry.wav", "color": "#4ecdc4", "trimSec": 0.08, "defaultGain": 0.45, "defaultPan": 0.30,  "defaultPitch": 14},
    {"id": "wah",   "name": "Auto-Wah Funk",   "phrase": "kujamba karibu sasa", "effect": "Auto-Wah + Phaser + Klon",  "url": "/build/tune/stem_funk_wah.wav",  "dryUrl": "/build/tune/stem_funk_dry.wav",  "color": "#a855f7", "trimSec": 0.48, "defaultGain": 0.82, "defaultPan": -0.40, "defaultPitch": 0},
    {"id": "lead",  "name": "Rat Fuzz Lead",   "phrase": "shuzi la moto sana",  "effect": "Rat Sketch + Flanger + Echo","url": "/build/tune/stem_lead_fuzz.wav", "dryUrl": "/build/tune/stem_lead_dry.wav", "color": "#ec4899", "trimSec": 0.55, "defaultGain": 0.85, "defaultPan": 0.35,  "defaultPitch": 0},
    {"id": "bomb",  "name": "Brown-Note Bomb", "phrase": "mamba mkubwa",        "effect": "Sub Pitch + Max Rat + Hall","url": "/build/tune/stem_bomb.wav",      "dryUrl": "/build/tune/stem_bomb_dry.wav",  "color": "#22c55e", "trimSec": 1.15, "defaultGain": 0.90, "defaultPan": 0.0,   "defaultPitch": -5},
]


def ensure_stems():
    tune_dir = os.path.join(ROOT, "build", "tune")
    needed = [
        "stem_bass_sub.wav", "stem_bass_dry.wav",
        "stem_snare.wav", "stem_snare_dry.wav",
        "stem_funk_wah.wav", "stem_funk_dry.wav",
        "stem_lead_fuzz.wav", "stem_lead_dry.wav",
        "stem_bomb.wav", "stem_bomb_dry.wav",
    ]
    if all(os.path.isfile(os.path.join(tune_dir, f)) for f in needed):
        return
    presets_sh = os.path.join(ROOT, "scripts", "presets.sh")
    subprocess.run([presets_sh, "stems", "--out-dir", tune_dir], cwd=ROOT, check=True)

def list_tracks():
    tracks = []
    presets_dir = os.path.join(ROOT, "build", "presets")
    if os.path.isdir(presets_dir):
        for fn in sorted(os.listdir(presets_dir)):
            if fn.endswith((".mp3", ".wav")):
                p = os.path.join(presets_dir, fn)
                kind = "Song" if fn.startswith("song_") else ("Custom" if fn.startswith("custom_") else "FX Preset")
                tracks.append({
                    "name": fn,
                    "kind": kind,
                    "url": f"/build/presets/{fn}",
                    "sizeKb": round(os.path.getsize(p) / 1024, 1),
                })
    rec_dir = os.path.join(ROOT, "recordings")
    if os.path.isdir(rec_dir):
        for fn in sorted(os.listdir(rec_dir)):
            if fn.endswith(".wav"):
                p = os.path.join(rec_dir, fn)
                tracks.append({
                    "name": fn,
                    "kind": "Recording",
                    "url": f"/recordings/{fn}",
                    "sizeKb": round(os.path.getsize(p) / 1024, 1),
                })
    return tracks

def render_sequence_to_mp3(payload):
    """Render the current 16-step sequencer grid + stem mixer settings to a stereo WAV & 320k MP3."""
    bpm = float(payload.get("bpm", 116))
    bars = int(payload.get("bars", 4))
    swing = float(payload.get("swing", 0.0))
    tracks_cfg = payload.get("tracks", {})
    SR = 44100

    def load_wav(rel_url):
        p = os.path.join(ROOT, rel_url.lstrip("/"))
        with wave.open(p, "rb") as w:
            ch = w.getnchannels()
            frames = w.readframes(w.getnframes())
            samples = array.array("h")
            samples.frombytes(frames)
            if ch == 2:
                samples = array.array("h", samples[0::2])
            return [s / 32768.0 for s in samples]

    def pitch_shift(buf, semitones):
        if abs(semitones) < 1e-3:
            return buf
        ratio = 2.0 ** (semitones / 12.0)
        out_len = max(1, int(len(buf) / ratio))
        out = [0.0] * out_len
        n = len(buf)
        for i in range(out_len):
            pos = i * ratio
            idx = int(pos)
            frac = pos - idx
            if idx + 1 < n:
                out[i] = buf[idx] * (1.0 - frac) + buf[idx + 1] * frac
            elif idx < n:
                out[i] = buf[idx]
        return out

    beat_sec = 60.0 / max(40.0, min(240.0, bpm))
    step_sec = beat_sec / 4.0
    bar_sec = beat_sec * 4.0
    total_sec = bars * bar_sec + 1.8
    N = int(total_sec * SR)
    L = [0.0] * N
    R = [0.0] * N

    any_solo = any(bool(tracks_cfg.get(sd["id"], {}).get("solo", False)) for sd in STEM_DEFS)

    for sd in STEM_DEFS:
        tid = sd["id"]
        tcfg = tracks_cfg.get(tid, {})
        if tcfg.get("mute", False):
            continue
        if any_solo and not tcfg.get("solo", False):
            continue

        use_dry = bool(tcfg.get("useDry", False))
        src_url = sd["dryUrl"] if use_dry else sd["url"]
        raw = load_wav(src_url)
        trim_len = min(len(raw), int(float(tcfg.get("trimSec", sd["trimSec"])) * SR))
        trimmed = raw[:trim_len]
        # Short 8 ms fade-out on trimmed slice so rapid steps don't click
        fade_n = min(len(trimmed), int(0.008 * SR))
        for i in range(fade_n):
            trimmed[len(trimmed) - 1 - i] *= (i / max(1, fade_n))

        base_pitch = float(tcfg.get("pitch", sd["defaultPitch"]))
        gain = float(tcfg.get("gain", sd["defaultGain"]))
        pan = max(-1.0, min(1.0, float(tcfg.get("pan", sd["defaultPan"]))))
        steps = tcfg.get("steps", [0] * 16)
        step_pitches = tcfg.get("stepPitches", [0] * 16)

        angle = (pan + 1.0) * (math.pi / 4.0)
        gl = gain * math.cos(angle)
        gr = gain * math.sin(angle)

        shifted_cache = {}
        for bar in range(bars):
            for s in range(16):
                vel = steps[s] if s < len(steps) else 0
                if not vel:
                    continue
                vel_scale = 1.0 if vel == 1 else (1.25 if vel == 2 else 0.65)
                sp = step_pitches[s] if s < len(step_pitches) else 0
                semi = base_pitch + sp
                if semi not in shifted_cache:
                    shifted_cache[semi] = pitch_shift(trimmed, semi)
                buf = shifted_cache[semi]

                swing_offset = (swing * 0.35 * step_sec) if (s % 2 == 1) else 0.0
                t0 = bar * bar_sec + s * step_sec + swing_offset
                idx0 = int(t0 * SR)
                for i, val in enumerate(buf):
                    idx = idx0 + i
                    if 0 <= idx < N:
                        L[idx] += val * gl * vel_scale
                        R[idx] += val * gr * vel_scale

    peak = max(max((abs(x) for x in L), default=0.0), max((abs(x) for x in R), default=0.0), 1e-6)
    out_pcm = bytearray()
    for i in range(N):
        l = math.tanh((L[i] / peak) * 1.4) / math.tanh(1.4) * 0.94
        r = math.tanh((R[i] / peak) * 1.4) / math.tanh(1.4) * 0.94
        out_pcm += struct.pack("<hh", int(l * 32767), int(r * 32767))

    presets_dir = os.path.join(ROOT, "build", "presets")
    os.makedirs(presets_dir, exist_ok=True)
    ts = int(time.time())
    wav_name = f"custom_seq_{int(bpm)}bpm.wav"
    mp3_name = f"custom_seq_{int(bpm)}bpm.mp3"
    wav_path = os.path.join(presets_dir, wav_name)
    mp3_path = os.path.join(presets_dir, mp3_name)

    with wave.open(wav_path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(bytes(out_pcm))

    if shutil.which("ffmpeg"):
        subprocess.run([
            "ffmpeg", "-y", "-loglevel", "error", "-i", wav_path,
            "-codec:a", "libmp3lame", "-b:a", "320k",
            "-metadata", f"title=Brownnote Custom Sequence ({int(bpm)} BPM)",
            "-metadata", "artist=kujamba x GuitarPedal",
            mp3_path
        ], check=True)
        # Also copy to workspace root so user can grab it easily
        shutil.copyfile(mp3_path, os.path.join(ROOT, "brownnote_custom_groove.mp3"))
        return {
            "ok": True,
            "url": f"/build/presets/{mp3_name}?t={ts}",
            "wavUrl": f"/build/presets/{wav_name}?t={ts}",
            "rootFile": "brownnote_custom_groove.mp3",
            "durationSec": round(N / SR, 2),
        }
    return {
        "ok": True,
        "url": f"/build/presets/{wav_name}?t={ts}",
        "wavUrl": f"/build/presets/{wav_name}?t={ts}",
        "durationSec": round(N / SR, 2),
    }


class StudioHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=ROOT, **kwargs)

    def log_message(self, fmt, *args):
        sys.stderr.write("[studio] " + (fmt % args) + "\n")

    def _send_json(self, code, data):
        body = json.dumps(data).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _send_html(self, html):
        body = html.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path in ("/", "/index.html"):
            return self._send_html(STUDIO_HTML)
        if parsed.path in EXTRA_PAGES:
            return self._send_html(EXTRA_PAGES[parsed.path])
        if parsed.path == "/api/state":
            try:
                ensure_stems()
            except Exception as e:
                return self._send_json(503, {"ok": False, "error": str(e)})
            return self._send_json(200, {
                "stems": STEM_DEFS,
                "tracks": list_tracks(),
            })
        return super().do_GET()

    def do_POST(self):
        parsed = urllib.parse.urlparse(self.path)
        length = int(self.headers.get("Content-Length", "0"))
        raw_body = self.rfile.read(length) if length > 0 else b"{}"
        try:
            payload = json.loads(raw_body.decode("utf-8"))
        except Exception as e:
            return self._send_json(400, {"ok": False, "error": str(e)})

        try:
            if parsed.path == "/api/render-sequence":
                res = render_sequence_to_mp3(payload)
                res["tracks"] = list_tracks()
                return self._send_json(200, res)

            if parsed.path == "/api/render-preset":
                mode = payload.get("mode", "fx")
                name = payload.get("name", "doom")
                phrase = payload.get("phrase", "").strip()
                bpm = str(int(payload.get("bpm", 120)))
                bars = str(int(payload.get("bars", 6)))
                seed = str(int(payload.get("seed", 42)))

                presets_sh = os.path.join(ROOT, "scripts", "presets.sh")
                if mode == "song":
                    out_rel = f"build/presets/song_{name}.mp3"
                    cmd = [presets_sh, "song", name, "--bpm", bpm, "--bars", bars, "--seed", seed, "--out", os.path.join(ROOT, out_rel)]
                else:
                    out_rel = f"build/presets/fx_{name}.mp3"
                    cmd = [presets_sh, "fx", name, "--seed", seed, "--out", os.path.join(ROOT, out_rel)]
                    if phrase:
                        cmd += ["--phrase", phrase]

                proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, timeout=45)
                if proc.returncode != 0:
                    return self._send_json(500, {"ok": False, "error": proc.stderr or proc.stdout})
                ts = int(time.time())
                return self._send_json(200, {
                    "ok": True,
                    "url": f"/{out_rel}?t={ts}",
                    "log": proc.stdout.strip(),
                    "tracks": list_tracks(),
                })

            return self._send_json(404, {"ok": False, "error": "unknown endpoint"})
        except Exception as e:
            return self._send_json(500, {"ok": False, "error": str(e)})


class ReusableTCPServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True


STUDIO_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>BROWNNOTE // 16-Step Fart Sequencer & Stem Mixer</title>
<style>
  :root {
    --bg: #0f0d0c;
    --panel: #191513;
    --panel-2: #221c19;
    --border: #362c26;
    --text: #f3ebe4;
    --muted: #9e8e82;
    --accent: #e07a3c;
    --accent-hi: #ffb347;
    --green: #22c55e;
    --red: #ef4444;
    --yellow: #f59e0b;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0;
    background: radial-gradient(circle at 50% 0%, #241b16 0%, var(--bg) 70%);
    color: var(--text);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12px;
    min-height: 100vh;
    padding: 14px 16px 36px;
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px 16px;
    margin-bottom: 14px;
    box-shadow: 0 8px 24px rgba(0,0,0,0.45);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .logo-badge {
    width: 38px;
    height: 38px;
    border-radius: 8px;
    background: linear-gradient(135deg, #7c3f1c, #3b1c0a);
    border: 1px solid var(--accent);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 20px;
  }
  .brand h1 {
    margin: 0;
    font-size: 15px;
    letter-spacing: 0.06em;
    color: var(--accent-hi);
  }
  .brand p {
    margin: 2px 0 0;
    font-size: 11px;
    color: var(--muted);
  }
  .page-nav { display: flex; flex-wrap: wrap; gap: 6px; }
  .page-nav a {
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--muted);
    text-decoration: none;
    border: 1px solid var(--border);
    background: var(--panel-2);
    padding: 6px 10px;
    border-radius: 6px;
    transition: all 0.12s ease;
  }
  .page-nav a:hover { color: var(--text); border-color: var(--accent); }
  .page-nav a.active { color: var(--accent-hi); border-color: var(--accent-hi); background: #3a2618; }
  .transport {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  button, select, input[type="text"], input[type="number"] {
    font-family: inherit;
    font-size: 11px;
    background: var(--panel-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 10px;
  }
  button {
    cursor: pointer;
    font-weight: 600;
    transition: all 0.12s ease;
  }
  button:hover {
    border-color: var(--accent);
    background: #2b231e;
  }
  button.primary {
    background: linear-gradient(180deg, #e07a3c, #b8561e);
    color: #fff;
    border-color: #f59e64;
  }
  button.primary:hover {
    filter: brightness(1.1);
  }
  button.playing {
    background: linear-gradient(180deg, #ef4444, #b91c1c);
    border-color: #fca5a5;
    color: #fff;
  }
  .ctrl-pill {
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    padding: 4px 9px;
    border-radius: 6px;
  }
  .ctrl-pill label {
    color: var(--muted);
    font-size: 10px;
    text-transform: uppercase;
  }
  .ctrl-pill input[type="range"] {
    width: 72px;
    accent-color: var(--accent);
  }
  .grid-layout {
    display: grid;
    grid-template-columns: 1fr;
    gap: 14px;
  }
  .card {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px 14px;
    box-shadow: 0 6px 18px rgba(0,0,0,0.35);
  }
  .card-header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border);
  }
  .card-title {
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--accent-hi);
  }
  .preset-bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .preset-btn {
    font-size: 11px;
    padding: 4px 9px;
  }
  .preset-btn.active {
    border-color: var(--accent-hi);
    background: #3a2618;
    color: var(--accent-hi);
  }

  /* Sequencer & Mixer Rows */
  .seq-header-row {
    display: grid;
    grid-template-columns: 270px 1fr;
    gap: 10px;
    align-items: center;
    margin-bottom: 6px;
    color: var(--muted);
    font-size: 10px;
  }
  .step-nums {
    display: grid;
    grid-template-columns: repeat(16, 1fr);
    gap: 4px;
    text-align: center;
  }
  .step-num {
    padding: 2px 0;
    border-radius: 4px;
    background: #14110f;
  }
  .step-num.beat-start {
    color: var(--accent-hi);
    font-weight: 700;
  }
  .step-num.active-col {
    background: var(--accent);
    color: #000;
    font-weight: 800;
  }

  .track-row {
    display: grid;
    grid-template-columns: 270px 1fr;
    gap: 10px;
    align-items: center;
    padding: 7px 8px;
    border-radius: 8px;
    background: var(--panel-2);
    border: 1px solid #2b231f;
    margin-bottom: 6px;
  }
  .track-info {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .track-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
  }
  .track-name-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
  }
  .stem-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .track-name {
    font-weight: 700;
    font-size: 11.5px;
  }
  .track-sub {
    font-size: 9.5px;
    color: var(--muted);
  }
  .track-btns {
    display: flex;
    gap: 4px;
  }
  .mini-btn {
    padding: 2px 6px;
    font-size: 9.5px;
    border-radius: 4px;
  }
  .mini-btn.mute-on {
    background: var(--red);
    color: #fff;
    border-color: #f87171;
  }
  .mini-btn.solo-on {
    background: var(--yellow);
    color: #000;
    border-color: #fcd34d;
  }
  .mini-btn.dry-on {
    background: #3b82f6;
    color: #fff;
    border-color: #93c5fd;
  }
  .track-Sliders {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 6px;
    align-items: center;
  }
  .mini-slider {
    display: flex;
    flex-direction: column;
    font-size: 9px;
    color: var(--muted);
  }
  .mini-slider input[type="range"] {
    width: 100%;
    height: 12px;
    margin: 0;
    accent-color: var(--accent);
  }

  .steps-grid {
    display: grid;
    grid-template-columns: repeat(16, 1fr);
    gap: 4px;
  }
  .step-cell {
    height: 36px;
    border-radius: 5px;
    background: #14110f;
    border: 1px solid #2d2520;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    user-select: none;
    position: relative;
    transition: transform 0.06s ease, border-color 0.08s ease;
  }
  .step-cell.beat-group {
    background: #181412;
  }
  .step-cell:hover {
    border-color: #6b574b;
  }
  .step-cell.on-1 {
    background: var(--stem-color, #e07a3c);
    color: #090807;
    font-weight: 800;
    border-color: rgba(255,255,255,0.45);
    box-shadow: 0 0 10px rgba(224,122,60,0.28);
  }
  .step-cell.on-2 {
    background: var(--accent-hi);
    color: #000;
    font-weight: 900;
    border-color: #fff;
    box-shadow: 0 0 14px rgba(255,179,71,0.55);
  }
  .step-cell.playhead {
    outline: 2px solid #fff;
    transform: scale(1.05);
    z-index: 2;
  }
  .step-semi {
    font-size: 8.5px;
    opacity: 0.85;
  }

  /* Bottom two-column section */
  .bottom-split {
    display: grid;
    grid-template-columns: 1.1fr 1fr;
    gap: 14px;
  }
  @media (max-width: 820px) {
    .seq-header-row, .track-row { grid-template-columns: 1fr; }
    .bottom-split { grid-template-columns: 1fr; }
  }
  .scope-wrap {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  canvas#scope {
    width: 150px;
    height: 36px;
    background: #0b0908;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .jukebox-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 250px;
    overflow-y: auto;
  }
  .jukebox-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 9px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .jukebox-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .tag {
    font-size: 9px;
    padding: 2px 5px;
    border-radius: 4px;
    background: #2f251f;
    color: var(--accent-hi);
    white-space: nowrap;
  }
  .form-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
    align-items: center;
  }
  .status-banner {
    margin-top: 8px;
    padding: 7px 10px;
    border-radius: 6px;
    background: #14110f;
    border: 1px solid var(--border);
    color: var(--accent-hi);
    font-size: 11px;
    min-height: 28px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  audio#masterPlayer {
    width: 100%;
    height: 32px;
    margin-top: 8px;
  }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE STUDIO // STEP SEQUENCER & STEM MIXER</h1>
      <p>kujamba (Zig Fart Synth) → Linus Torvalds' GuitarPedal Audio Core → WebAudio + 320kbps MP3</p>
    </div>
  </div>

  <div class="transport">
    <button id="playBtn" class="primary" onclick="togglePlay()">▶ PLAY SEQ</button>
    <button onclick="triggerOneShot('bomb')">💣 DROP BROWN NOTE</button>
    <div class="ctrl-pill">
      <label for="bpmInput">BPM</label>
      <input id="bpmInput" type="range" min="60" max="190" value="116" oninput="updateBpm(this.value)" />
      <span id="bpmVal">116</span>
    </div>
    <div class="ctrl-pill">
      <label for="swingInput">Swing</label>
      <input id="swingInput" type="range" min="0" max="70" value="12" oninput="updateSwing(this.value)" />
      <span id="swingVal">12%</span>
    </div>
    <div class="ctrl-pill">
      <label for="barsSelect">Export Bars</label>
      <select id="barsSelect">
        <option value="2">2 Bars</option>
        <option value="4" selected>4 Bars</option>
        <option value="8">8 Bars</option>
      </select>
    </div>
    <button class="primary" onclick="bounceSequenceToMp3()">⬇ BARF SEQ TO MP3</button>
    <div class="scope-wrap">
      <canvas id="scope" width="150" height="36"></canvas>
    </div>
  </div>

  <nav class="page-nav">
    <a href="/" class="active">SEQ+MIX</a>
    <a href="/mixer">DESK</a>
    <a href="/pads">PADS</a>
    <a href="/library">CRATES</a>
  </nav>
</header>

<div class="grid-layout">
  <section class="card">
    <div class="card-header">
      <div>
        <span class="card-title">16-Step Fart Sequencer & Stem Mixer</span>
        <span style="color:var(--muted); margin-left:8px; font-size:10.5px;">
          Left-click step: Normal → Accent (★) → Off &nbsp;|&nbsp; Shift+Click step: Cycle Pitch (+3/+5/+7/+12/-5 st) &nbsp;|&nbsp; Click track title to audition stem
        </span>
      </div>
      <div class="preset-bar">
        <button class="preset-btn active" id="pat-funk" onclick="loadPatternPreset('funk')">P-Funk 116</button>
        <button class="preset-btn" id="pat-metal" onclick="loadPatternPreset('metal')">Double-Kick Metal 168</button>
        <button class="preset-btn" id="pat-techno" onclick="loadPatternPreset('techno')">Acid 303 Techno 132</button>
        <button class="preset-btn" id="pat-doom" onclick="loadPatternPreset('doom')">Tectonic Doom 66</button>
        <button class="preset-btn" onclick="randomizePattern()">🎲 Randomize</button>
        <button class="preset-btn" onclick="clearPattern()">Clear</button>
      </div>
    </div>

    <div class="seq-header-row">
      <div>STEM CHANNEL / MIX CONTROLS (VOL · PAN · PITCH)</div>
      <div class="step-nums" id="stepHeaderNums"></div>
    </div>

    <div id="tracksContainer"></div>

    <div class="status-banner" id="seqStatus">
      <span id="statusText">Ready. Click ▶ PLAY SEQ to jam live in the browser, or ⬇ BARF SEQ TO MP3 to render a stereo MP3 file.</span>
      <span id="statusLink"></span>
    </div>
  </section>

  <div class="bottom-split">
    <!-- Left: Custom FX & Song Generator -->
    <section class="card">
      <div class="card-header">
        <span class="card-title">Custom Phrase & Song Generator (kujamba → brownnote)</span>
      </div>

      <div style="margin-bottom: 10px; color: var(--muted); font-size: 11px;">
        Synthesize any Swahili phrase with <code>kujamba</code> and run it through a <code>GuitarPedal</code> chain on the server:
      </div>

      <div class="form-row">
        <label>Preset:</label>
        <select id="fxPresetSelect">
          <option value="doom">Doom Sub-Octave + Rat Fuzz + Cab</option>
          <option value="cathedral">Wet Space Cathedral (Wah + Phaser + Echo + Reverb)</option>
          <option value="machinegun">Machine-Gun French Amp + Tremolo</option>
          <option value="swamp">Swamp Monster (Vibrato + Flanger + Rat)</option>
          <option value="klon-crunch">Klonlike Overdrive + Boost + Cab</option>
          <option value="cyber-laser">Cyber-Laser (High-Q Wah + Octave Up + Flanger)</option>
          <option value="dry">Dry Unprocessed Fart</option>
        </select>
        <label>Seed:</label>
        <input id="fxSeedInput" type="number" value="42" style="width:64px;" />
      </div>

      <div class="form-row">
        <label>Swahili Phrase:</label>
        <input id="fxPhraseInput" type="text" value="shuzi kubwa sana la radi" style="flex:1; min-width:180px;" />
        <button class="primary" onclick="renderServerPreset('fx')">⚡ Render FX to MP3</button>
      </div>

      <hr style="border:0; border-top:1px solid var(--border); margin: 12px 0;" />

      <div style="margin-bottom: 8px; color: var(--muted); font-size: 11px;">
        Or render a full multi-track arranged genre song via <code>scripts/presets.sh song</code>:
      </div>
      <div class="form-row">
        <label>Genre:</label>
        <select id="songGenreSelect">
          <option value="funk">P-Funk Symphony (116 BPM)</option>
          <option value="metal">Double-Kick Shred Metal (168 BPM)</option>
          <option value="techno">Acid 303 Fart Techno (132 BPM)</option>
          <option value="doom">Tectonic Sludge Doom (66 BPM)</option>
        </select>
        <label>Bars:</label>
        <input id="songBarsInput" type="number" value="6" min="2" max="16" style="width:54px;" />
        <button class="primary" onclick="renderServerPreset('song')">🎵 Render Full Song MP3</button>
      </div>

      <audio id="masterPlayer" controls></audio>
    </section>

    <!-- Right: Rendered MP3 & WAV Jukebox -->
    <section class="card">
      <div class="card-header">
        <span class="card-title">Rendered MP3 & WAV Jukebox</span>
        <button class="mini-btn" onclick="refreshState()">↻ Refresh</button>
      </div>
      <div class="jukebox-list" id="jukeboxList"></div>
    </section>
  </div>
</div>

<script>
let STEMS = [];
let trackState = {};
let audioCtx = null;
let masterAnalyser = null;
let decodedBuffers = {}; // key: url -> AudioBuffer

let isPlaying = false;
let currentStep = 0;
let bpm = 116;
let swing = 0.12;
let stepTimer = null;

const PITCH_CYCLE = [0, 3, 5, 7, 10, 12, -5, -12];

const PATTERN_PRESETS = {
  funk: {
    bpm: 116, swing: 16,
    tracks: {
      kick:  { steps: [2,0,1,1, 0,0,1,0, 2,0,1,0, 0,0,1,1], pitches: [0,0,0,3, 0,0,7,0, 0,0,10,0, 0,0,3,-2] },
      snare: { steps: [0,0,0,0, 2,0,0,1, 0,0,0,0, 2,0,0,1], pitches: [0,0,0,0, 0,0,0,3, 0,0,0,0, 0,0,0,5] },
      hat:   { steps: [1,0,1,0, 1,0,1,1, 1,0,1,0, 1,0,1,1], pitches: [0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0] },
      wah:   { steps: [1,0,0,0, 0,0,1,0, 0,0,1,0, 0,1,0,0], pitches: [0,0,0,0, 0,0,5,0, 0,0,7,0, 0,3,0,0] },
      lead:  { steps: [0,0,0,0, 1,0,0,0, 0,0,0,0, 2,0,1,0], pitches: [0,0,0,0, 7,0,0,0, 0,0,0,0, 12,0,10,0] },
      bomb:  { steps: [2,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0], pitches: [0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0] }
    }
  },
  metal: {
    bpm: 168, swing: 0,
    tracks: {
      kick:  { steps: [2,1,1,1, 2,1,1,1, 2,1,1,1, 2,1,1,1], pitches: [-5,-5,-5,-5, -5,-5,-5,-5, -5,-5,-5,-5, -5,-5,-5,-5] },
      snare: { steps: [0,0,2,0, 0,0,2,0, 0,0,2,0, 0,0,2,1], pitches: [0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0] },
      hat:   { steps: [2,0,1,0, 2,0,1,0, 2,0,1,0, 2,0,1,0], pitches: [0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0] },
      wah:   { steps: [1,1,1,0, 1,1,1,0, 1,1,1,0, 2,0,2,0], pitches: [0,0,0,0, 1,1,1,0, 0,0,0,0, 6,0,5,0] },
      lead:  { steps: [2,0,0,1, 0,0,2,0, 1,0,0,0, 2,0,1,0], pitches: [12,0,0,15, 0,0,18,0, 19,0,0,0, 15,0,12,0] },
      bomb:  { steps: [2,0,0,0, 0,0,0,0, 2,0,0,0, 0,0,0,0], pitches: [-5,0,0,0, 0,0,0,0, -7,0,0,0, 0,0,0,0] }
    }
  },
  techno: {
    bpm: 132, swing: 8,
    tracks: {
      kick:  { steps: [2,0,0,0, 2,0,0,0, 2,0,0,0, 2,0,0,0], pitches: [-5,0,0,0, -5,0,0,0, -5,0,0,0, -5,0,0,0] },
      snare: { steps: [0,0,0,0, 2,0,0,0, 0,0,0,0, 2,0,0,1], pitches: [0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,3] },
      hat:   { steps: [0,1,2,1, 0,1,2,1, 0,1,2,1, 0,1,2,1], pitches: [0,0,3,0, 0,0,3,0, 0,0,3,0, 0,0,3,0] },
      wah:   { steps: [1,1,2,1, 0,1,1,2, 1,0,2,1, 1,2,0,1], pitches: [0,0,12,0, 0,3,7,10, 0,0,12,15, 3,7,0,-2] },
      lead:  { steps: [0,0,0,0, 0,0,1,0, 0,0,0,0, 0,0,2,0], pitches: [0,0,0,0, 0,0,7,0, 0,0,0,0, 0,0,12,0] },
      bomb:  { steps: [1,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0], pitches: [-7,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0] }
    }
  },
  doom: {
    bpm: 66, swing: 0,
    tracks: {
      kick:  { steps: [2,0,0,0, 0,0,1,0, 0,0,2,0, 0,0,0,0], pitches: [-7,0,0,0, 0,0,-7,0, 0,0,-7,0, 0,0,0,0] },
      snare: { steps: [0,0,0,0, 0,0,0,0, 2,0,0,0, 0,0,0,0], pitches: [0,0,0,0, 0,0,0,0, -4,0,0,0, 0,0,0,0] },
      hat:   { steps: [1,0,0,0, 1,0,0,0, 1,0,0,0, 1,0,0,0], pitches: [-5,0,0,0, -5,0,0,0, -5,0,0,0, -5,0,0,0] },
      wah:   { steps: [2,0,0,0, 0,0,0,0, 1,0,0,0, 0,0,0,0], pitches: [-5,0,0,0, 0,0,0,0, -7,0,0,0, 0,0,0,0] },
      lead:  { steps: [0,0,0,0, 1,0,0,0, 0,0,0,0, 1,0,0,0], pitches: [0,0,0,0, 3,0,0,0, 0,0,0,0, 6,0,0,0] },
      bomb:  { steps: [2,0,0,0, 0,0,0,0, 2,0,0,0, 0,0,1,0], pitches: [0,0,0,0, 0,0,0,0, -2,0,0,0, 0,0,-5,0] }
    }
  }
};

function ensureAudio() {
  if (!audioCtx) {
    const AC = window.AudioContext || window.webkitAudioContext;
    audioCtx = new AC();
    masterAnalyser = audioCtx.createAnalyser();
    masterAnalyser.fftSize = 256;
    masterAnalyser.connect(audioCtx.destination);
    drawScope();
  }
  if (audioCtx.state === 'suspended') {
    audioCtx.resume();
  }
}

async function fetchBuffer(url) {
  if (decodedBuffers[url]) return decodedBuffers[url];
  ensureAudio();
  const res = await fetch(url);
  const arr = await res.arrayBuffer();
  const buf = await audioCtx.decodeAudioData(arr);
  decodedBuffers[url] = buf;
  return buf;
}

async function preloadAllStems() {
  for (const s of STEMS) {
    fetchBuffer(s.url).catch(() => {});
    fetchBuffer(s.dryUrl).catch(() => {});
  }
}

function initStepHeader() {
  const wrap = document.getElementById('stepHeaderNums');
  wrap.innerHTML = '';
  for (let i = 0; i < 16; i++) {
    const d = document.createElement('div');
    d.className = 'step-num' + (i % 4 === 0 ? ' beat-start' : '');
    d.id = `step-hdr-${i}`;
    d.textContent = i + 1;
    wrap.appendChild(d);
  }
}

function buildTracksUI() {
  const container = document.getElementById('tracksContainer');
  container.innerHTML = '';

  for (const stem of STEMS) {
    const st = trackState[stem.id];
    const row = document.createElement('div');
    row.className = 'track-row';
    row.style.setProperty('--stem-color', stem.color);

    const info = document.createElement('div');
    info.className = 'track-info';
    info.innerHTML = `
      <div class="track-top">
        <div class="track-name-wrap" onclick="triggerOneShot('${stem.id}')" title="Click to audition stem">
          <span class="stem-dot" style="background:${stem.color}"></span>
          <div>
            <div class="track-name">${stem.name}</div>
            <div class="track-sub">"${stem.phrase}" · ${stem.effect}</div>
          </div>
        </div>
        <div class="track-btns">
          <button class="mini-btn ${st.useDry ? 'dry-on' : ''}" id="btn-dry-${stem.id}" onclick="toggleDry('${stem.id}')" title="Toggle Dry vs Pedaled stem">DRY</button>
          <button class="mini-btn ${st.mute ? 'mute-on' : ''}" id="btn-mute-${stem.id}" onclick="toggleMute('${stem.id}')">M</button>
          <button class="mini-btn ${st.solo ? 'solo-on' : ''}" id="btn-solo-${stem.id}" onclick="toggleSolo('${stem.id}')">S</button>
        </div>
      </div>
      <div class="track-Sliders">
        <div class="mini-slider">
          <span>VOL <b id="lbl-gain-${stem.id}">${Math.round(st.gain * 100)}%</b></span>
          <input type="range" min="0" max="140" value="${Math.round(st.gain * 100)}"
            oninput="setTrackParam('${stem.id}','gain',this.value/100)" />
        </div>
        <div class="mini-slider">
          <span>PAN <b id="lbl-pan-${stem.id}">${formatPan(st.pan)}</b></span>
          <input type="range" min="-100" max="100" value="${Math.round(st.pan * 100)}"
            oninput="setTrackParam('${stem.id}','pan',this.value/100)" />
        </div>
        <div class="mini-slider">
          <span>PITCH <b id="lbl-pitch-${stem.id}">${st.pitch >= 0 ? '+'+st.pitch : st.pitch}st</b></span>
          <input type="range" min="-12" max="18" value="${st.pitch}"
            oninput="setTrackParam('${stem.id}','pitch',parseInt(this.value,10))" />
        </div>
      </div>
    `;

    const grid = document.createElement('div');
    grid.className = 'steps-grid';
    for (let s = 0; s < 16; s++) {
      const cell = document.createElement('div');
      cell.id = `cell-${stem.id}-${s}`;
      cell.className = 'step-cell' + (Math.floor(s / 4) % 2 === 1 ? ' beat-group' : '');
      cell.style.setProperty('--stem-color', stem.color);
      cell.onclick = (ev) => onStepClick(ev, stem.id, s);
      grid.appendChild(cell);
    }

    row.appendChild(info);
    row.appendChild(grid);
    container.appendChild(row);
    refreshTrackCells(stem.id);
  }
}

function formatPan(p) {
  if (Math.abs(p) < 0.05) return 'C';
  return p < 0 ? `${Math.round(Math.abs(p)*100)}L` : `${Math.round(p*100)}R`;
}

function setTrackParam(id, key, val) {
  trackState[id][key] = val;
  if (key === 'gain') document.getElementById(`lbl-gain-${id}`).textContent = `${Math.round(val*100)}%`;
  if (key === 'pan') document.getElementById(`lbl-pan-${id}`).textContent = formatPan(val);
  if (key === 'pitch') document.getElementById(`lbl-pitch-${id}`).textContent = `${val >= 0 ? '+'+val : val}st`;
}

function toggleMute(id) {
  trackState[id].mute = !trackState[id].mute;
  document.getElementById(`btn-mute-${id}`).classList.toggle('mute-on', trackState[id].mute);
}
function toggleSolo(id) {
  trackState[id].solo = !trackState[id].solo;
  document.getElementById(`btn-solo-${id}`).classList.toggle('solo-on', trackState[id].solo);
}
function toggleDry(id) {
  trackState[id].useDry = !trackState[id].useDry;
  document.getElementById(`btn-dry-${id}`).classList.toggle('dry-on', trackState[id].useDry);
  triggerOneShot(id);
}

function onStepClick(ev, id, s) {
  const st = trackState[id];
  if (ev.shiftKey) {
    if (st.steps[s] === 0) st.steps[s] = 1;
    const curIdx = PITCH_CYCLE.indexOf(st.stepPitches[s]);
    st.stepPitches[s] = PITCH_CYCLE[(curIdx + 1) % PITCH_CYCLE.length];
  } else {
    st.steps[s] = (st.steps[s] + 1) % 3;
  }
  refreshTrackCells(id);
  if (st.steps[s] > 0 && !isPlaying) {
    playStemSample(id, st.stepPitches[s], st.steps[s] === 2 ? 1.25 : 1.0);
  }
}

function refreshTrackCells(id) {
  const st = trackState[id];
  for (let s = 0; s < 16; s++) {
    const cell = document.getElementById(`cell-${id}-${s}`);
    if (!cell) continue;
    cell.classList.remove('on-1', 'on-2');
    const v = st.steps[s];
    const sp = st.stepPitches[s];
    const semiTxt = sp !== 0 ? (sp > 0 ? `+${sp}` : `${sp}`) : '';
    if (v === 1) {
      cell.classList.add('on-1');
      cell.innerHTML = `<span>●</span><span class="step-semi">${semiTxt}</span>`;
    } else if (v === 2) {
      cell.classList.add('on-2');
      cell.innerHTML = `<span>★</span><span class="step-semi">${semiTxt}</span>`;
    } else {
      cell.innerHTML = `<span class="step-semi" style="color:#463a33">${semiTxt}</span>`;
    }
  }
}

async function playStemSample(id, stepSemi = 0, velScale = 1.0) {
  const stem = STEMS.find(x => x.id === id);
  const st = trackState[id];
  if (!stem || !st) return;
  const url = st.useDry ? stem.dryUrl : stem.url;
  const buf = await fetchBuffer(url);
  if (!buf) return;

  const src = audioCtx.createBufferSource();
  src.buffer = buf;
  const totalSemi = (st.pitch || 0) + (stepSemi || 0);
  src.playbackRate.value = Math.pow(2, totalSemi / 12);

  const gainNode = audioCtx.createGain();
  const panner = audioCtx.createStereoPanner();
  panner.pan.value = st.pan || 0;

  const now = audioCtx.currentTime;
  const dur = Math.max(0.05, stem.trimSec / src.playbackRate.value);
  const peakGain = Math.max(0.0001, st.gain * velScale);
  gainNode.gain.setValueAtTime(peakGain, now);
  gainNode.gain.setValueAtTime(peakGain, now + dur * 0.8);
  gainNode.gain.linearRampToValueAtTime(0.0001, now + dur);

  src.connect(gainNode);
  gainNode.connect(panner);
  panner.connect(masterAnalyser);

  src.start(now);
  src.stop(now + dur + 0.02);
}

function triggerOneShot(id) {
  ensureAudio();
  playStemSample(id, 0, 1.0);
}

function stepTick() {
  if (!isPlaying) return;

  for (let i = 0; i < 16; i++) {
    const hdr = document.getElementById(`step-hdr-${i}`);
    if (hdr) hdr.classList.toggle('active-col', i === currentStep);
    for (const stem of STEMS) {
      const c = document.getElementById(`cell-${stem.id}-${i}`);
      if (c) c.classList.toggle('playhead', i === currentStep);
    }
  }

  const anySolo = STEMS.some(s => trackState[s.id].solo);
  for (const stem of STEMS) {
    const st = trackState[stem.id];
    if (st.mute) continue;
    if (anySolo && !st.solo) continue;
    const v = st.steps[currentStep];
    if (v > 0) {
      playStemSample(stem.id, st.stepPitches[currentStep], v === 2 ? 1.25 : 1.0);
    }
  }

  const baseStepMs = (60 / bpm / 4) * 1000;
  const isOdd = currentStep % 2 === 1;
  const delayMs = isOdd ? baseStepMs * (1 - swing * 0.35) : baseStepMs * (1 + swing * 0.35);

  currentStep = (currentStep + 1) % 16;
  stepTimer = setTimeout(stepTick, Math.max(25, delayMs));
}

function togglePlay() {
  ensureAudio();
  preloadAllStems();
  isPlaying = !isPlaying;
  const btn = document.getElementById('playBtn');
  if (isPlaying) {
    currentStep = 0;
    btn.textContent = '⏹ STOP SEQ';
    btn.classList.add('playing');
    stepTick();
  } else {
    clearTimeout(stepTimer);
    btn.textContent = '▶ PLAY SEQ';
    btn.classList.remove('playing');
    for (let i = 0; i < 16; i++) {
      const hdr = document.getElementById(`step-hdr-${i}`);
      if (hdr) hdr.classList.remove('active-col');
      for (const stem of STEMS) {
        const c = document.getElementById(`cell-${stem.id}-${i}`);
        if (c) c.classList.remove('playhead');
      }
    }
  }
}

function updateBpm(val) {
  bpm = parseInt(val, 10);
  document.getElementById('bpmVal').textContent = bpm;
}

function updateSwing(val) {
  swing = parseInt(val, 10) / 100;
  document.getElementById('swingVal').textContent = `${val}%`;
}

function loadPatternPreset(name) {
  const p = PATTERN_PRESETS[name];
  if (!p) return;
  document.querySelectorAll('.preset-btn').forEach(b => b.classList.remove('active'));
  const btn = document.getElementById(`pat-${name}`);
  if (btn) btn.classList.add('active');

  bpm = p.bpm;
  document.getElementById('bpmInput').value = bpm;
  document.getElementById('bpmVal').textContent = bpm;

  swing = p.swing / 100;
  document.getElementById('swingInput').value = p.swing;
  document.getElementById('swingVal').textContent = `${p.swing}%`;

  for (const stem of STEMS) {
    const pt = p.tracks[stem.id];
    if (pt) {
      trackState[stem.id].steps = [...pt.steps];
      trackState[stem.id].stepPitches = [...pt.pitches];
      refreshTrackCells(stem.id);
    }
  }
}

function clearPattern() {
  for (const stem of STEMS) {
    trackState[stem.id].steps = new Array(16).fill(0);
    trackState[stem.id].stepPitches = new Array(16).fill(0);
    refreshTrackCells(stem.id);
  }
}

function randomizePattern() {
  const densities = { kick: 0.35, snare: 0.25, hat: 0.55, wah: 0.30, lead: 0.22, bomb: 0.10 };
  for (const stem of STEMS) {
    const d = densities[stem.id] || 0.25;
    for (let s = 0; s < 16; s++) {
      if (Math.random() < d) {
        trackState[stem.id].steps[s] = Math.random() < 0.25 ? 2 : 1;
        trackState[stem.id].stepPitches[s] = PITCH_CYCLE[Math.floor(Math.random() * 5)];
      } else {
        trackState[stem.id].steps[s] = 0;
        trackState[stem.id].stepPitches[s] = 0;
      }
    }
    refreshTrackCells(stem.id);
  }
}

async function bounceSequenceToMp3() {
  const stText = document.getElementById('statusText');
  const stLink = document.getElementById('statusLink');
  const bars = parseInt(document.getElementById('barsSelect').value, 10);
  stText.textContent = `⏳ Rendering ${bars} bars at ${bpm} BPM through brownnote stereo mixer to 320kbps MP3...`;
  stLink.innerHTML = '';

  const res = await fetch('/api/render-sequence', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ bpm, bars, swing, tracks: trackState })
  });
  const data = await res.json();
  if (!data.ok) {
    stText.textContent = `❌ Error: ${data.error}`;
    return;
  }
  stText.textContent = `✓ Exported ${data.durationSec}s stereo MP3 (${bpm} BPM, ${bars} bars) -> ${data.rootFile || data.url}`;
  stLink.innerHTML = `<a href="${data.url}" download style="color:var(--accent-hi);font-weight:700;">⬇ Download MP3</a>`;
  playInMasterPlayer(data.url);
  renderJukebox(data.tracks || []);
}

async function renderServerPreset(mode) {
  const stText = document.getElementById('statusText');
  const stLink = document.getElementById('statusLink');
  const payload = { mode };
  if (mode === 'fx') {
    payload.name = document.getElementById('fxPresetSelect').value;
    payload.phrase = document.getElementById('fxPhraseInput').value;
    payload.seed = parseInt(document.getElementById('fxSeedInput').value || '42', 10);
    stText.textContent = `⏳ Synthesizing "${payload.phrase}" + running GuitarPedal '${payload.name}' preset...`;
  } else {
    payload.name = document.getElementById('songGenreSelect').value;
    payload.bars = parseInt(document.getElementById('songBarsInput').value || '6', 10);
    payload.bpm = bpm;
    stText.textContent = `⏳ Rendering full '${payload.name}' song (${payload.bars} bars)...`;
  }

  const res = await fetch('/api/render-preset', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload)
  });
  const data = await res.json();
  if (!data.ok) {
    stText.textContent = `❌ Error: ${data.error}`;
    return;
  }
  stText.textContent = `✓ ${data.log || 'Rendered MP3!'}`;
  stLink.innerHTML = `<a href="${data.url}" download style="color:var(--accent-hi);font-weight:700;">⬇ Download MP3</a>`;
  playInMasterPlayer(data.url);
  renderJukebox(data.tracks || []);
}

function playInMasterPlayer(url) {
  if (isPlaying) togglePlay();
  const player = document.getElementById('masterPlayer');
  player.src = url;
  player.play().catch(() => {});
}

function renderJukebox(tracks) {
  const list = document.getElementById('jukeboxList');
  list.innerHTML = '';
  for (const t of tracks) {
    const item = document.createElement('div');
    item.className = 'jukebox-item';
    item.innerHTML = `
      <div class="jukebox-meta">
        <span class="tag">${t.kind}</span>
        <span style="font-weight:600; overflow:hidden; text-overflow:ellipsis;">${t.name}</span>
        <span style="color:var(--muted); font-size:10px;">${t.sizeKb} KB</span>
      </div>
      <div style="display:flex; gap:6px; align-items:center;">
        <button class="mini-btn" onclick="playInMasterPlayer('${t.url}')">▶ Play</button>
        <a href="${t.url}" download style="color:var(--accent-hi); text-decoration:none; font-size:10.5px;">⬇</a>
      </div>
    `;
    list.appendChild(item);
  }
}

function drawScope() {
  const canvas = document.getElementById('scope');
  const ctx = canvas.getContext('2d');
  const bufLen = masterAnalyser ? masterAnalyser.frequencyBinCount : 128;
  const data = new Uint8Array(bufLen);

  function frame() {
    requestAnimationFrame(frame);
    ctx.fillStyle = '#0b0908';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    if (!masterAnalyser) return;
    masterAnalyser.getByteTimeDomainData(data);
    ctx.lineWidth = 1.6;
    ctx.strokeStyle = '#ffb347';
    ctx.beginPath();
    const slice = canvas.width / bufLen;
    let x = 0;
    for (let i = 0; i < bufLen; i++) {
      const v = data[i] / 128.0;
      const y = (v * canvas.height) / 2;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
      x += slice;
    }
    ctx.stroke();
  }
  frame();
}

async function refreshState() {
  const res = await fetch('/api/state');
  const data = await res.json();
  if (STEMS.length === 0) {
    STEMS = data.stems;
    for (const s of STEMS) {
      trackState[s.id] = {
        gain: s.defaultGain,
        pan: s.defaultPan,
        pitch: s.defaultPitch,
        mute: false,
        solo: false,
        useDry: false,
        steps: new Array(16).fill(0),
        stepPitches: new Array(16).fill(0),
      };
    }
    initStepHeader();
    buildTracksUI();
    loadPatternPreset('funk');
  }
  renderJukebox(data.tracks || []);
}

window.addEventListener('DOMContentLoaded', refreshState);
</script>
</body>
</html>
"""


# ---------------------------------------------------------------------------
# Extra studio views: same stems + /api/state, different skins on the jam.
# ---------------------------------------------------------------------------

PAGE_CSS = r"""
  :root {
    --bg: #0f0d0c;
    --panel: #191513;
    --panel-2: #221c19;
    --border: #362c26;
    --text: #f3ebe4;
    --muted: #9e8e82;
    --accent: #e07a3c;
    --accent-hi: #ffb347;
    --green: #22c55e;
    --red: #ef4444;
    --yellow: #f59e0b;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0;
    background: radial-gradient(circle at 50% 0%, #241b16 0%, var(--bg) 70%);
    color: var(--text);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12px;
    min-height: 100vh;
    padding: 14px 16px 36px;
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px 16px;
    margin-bottom: 14px;
    box-shadow: 0 8px 24px rgba(0,0,0,0.45);
  }
  .brand { display: flex; align-items: center; gap: 12px; }
  .logo-badge {
    width: 38px;
    height: 38px;
    border-radius: 8px;
    background: linear-gradient(135deg, #7c3f1c, #3b1c0a);
    border: 1px solid var(--accent);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 20px;
  }
  .brand h1 { margin: 0; font-size: 15px; letter-spacing: 0.06em; color: var(--accent-hi); }
  .brand p { margin: 2px 0 0; font-size: 11px; color: var(--muted); }
  .page-nav { display: flex; flex-wrap: wrap; gap: 6px; }
  .page-nav a {
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--muted);
    text-decoration: none;
    border: 1px solid var(--border);
    background: var(--panel-2);
    padding: 6px 10px;
    border-radius: 6px;
    transition: all 0.12s ease;
  }
  .page-nav a:hover { color: var(--text); border-color: var(--accent); }
  .page-nav a.active { color: var(--accent-hi); border-color: var(--accent-hi); background: #3a2618; }
  button, select, input[type="text"], input[type="number"] {
    font-family: inherit;
    font-size: 11px;
    background: var(--panel-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 10px;
  }
  button { cursor: pointer; font-weight: 600; transition: all 0.12s ease; }
  button:hover { border-color: var(--accent); background: #2b231e; }
  button.primary {
    background: linear-gradient(180deg, #e07a3c, #b8561e);
    color: #fff;
    border-color: #f59e64;
  }
  button.primary:hover { filter: brightness(1.1); }
  .mini-btn { padding: 2px 6px; font-size: 9.5px; border-radius: 4px; }
  .mini-btn.mute-on { background: var(--red); color: #fff; border-color: #f87171; }
  .mini-btn.solo-on { background: var(--yellow); color: #000; border-color: #fcd34d; }
  .mini-btn.dry-on { background: #3b82f6; color: #fff; border-color: #93c5fd; }
  .card {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px 14px;
    box-shadow: 0 6px 18px rgba(0,0,0,0.35);
  }
  .card-header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border);
  }
  .card-title {
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--accent-hi);
  }
  .status-banner {
    margin-top: 8px;
    padding: 7px 10px;
    border-radius: 6px;
    background: #14110f;
    border: 1px solid var(--border);
    color: var(--accent-hi);
    font-size: 11px;
    min-height: 28px;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .tag {
    font-size: 9px;
    padding: 2px 5px;
    border-radius: 4px;
    background: #2f251f;
    color: var(--accent-hi);
    white-space: nowrap;
  }
"""


MIXER_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>BROWNNOTE // Console Mixer</title>
<style>""" + PAGE_CSS + r"""
  .desk {
    display: flex;
    gap: 10px;
    align-items: stretch;
  }
  .strips {
    display: grid;
    grid-template-columns: repeat(6, 1fr);
    gap: 10px;
    flex: 1;
  }
  .strip {
    background: var(--panel);
    border: 1px solid var(--border);
    border-top: 3px solid var(--stem-color, var(--accent));
    border-radius: 10px;
    padding: 10px 8px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    box-shadow: 0 6px 18px rgba(0,0,0,0.35);
  }
  .strip-name {
    font-size: 10.5px;
    font-weight: 800;
    letter-spacing: 0.04em;
    text-align: center;
    color: var(--text);
  }
  .strip-sub {
    font-size: 8.5px;
    color: var(--muted);
    text-align: center;
    min-height: 22px;
  }
  .strip-mid {
    display: flex;
    align-items: stretch;
    gap: 8px;
    height: 128px;
  }
  .fader {
    writing-mode: vertical-lr;
    direction: rtl;
    -webkit-appearance: slider-vertical;
    appearance: slider-vertical;
    width: 24px;
    height: 128px;
    margin: 0;
    accent-color: var(--stem-color, var(--accent));
    cursor: pointer;
  }
  .vu {
    width: 12px;
    height: 128px;
    background: #0b0908;
    border: 1px solid var(--border);
    border-radius: 3px;
  }
  .strip-slider {
    width: 100%;
    display: flex;
    flex-direction: column;
    font-size: 8.5px;
    color: var(--muted);
    text-align: center;
  }
  .strip-slider input[type="range"] {
    width: 100%;
    height: 12px;
    margin: 0;
    accent-color: var(--stem-color, var(--accent));
  }
  .strip-btns { display: flex; gap: 4px; }
  .strip .tap {
    width: 100%;
    padding: 9px 4px;
    font-size: 11px;
    font-weight: 800;
    border-radius: 7px;
    background: var(--panel-2);
    border-color: var(--stem-color, var(--accent));
    color: var(--text);
  }
  .strip .tap:active, .strip .tap.hit {
    background: var(--stem-color, var(--accent));
    color: #090807;
    transform: scale(0.97);
  }
  .strip .loop.on {
    background: var(--green);
    color: #04140a;
    border-color: #4ade80;
    width: 100%;
  }
  .strip .loop { width: 100%; }
  .master {
    width: 230px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-top: 3px solid var(--accent-hi);
    border-radius: 10px;
    padding: 10px 12px;
    box-shadow: 0 6px 18px rgba(0,0,0,0.35);
  }
  .master canvas#scope {
    width: 100%;
    height: 64px;
    background: #0b0908;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .master .mvol { display: flex; align-items: center; gap: 8px; font-size: 9px; color: var(--muted); }
  .master .mvol input { flex: 1; accent-color: var(--accent-hi); }
  @media (max-width: 1080px) {
    .desk { flex-direction: column; }
    .strips { grid-template-columns: repeat(3, 1fr); }
    .master { width: 100%; }
  }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE DESK // 6-CHANNEL STEM CONSOLE</h1>
      <p>Vertical strips over the same six stems · VU per channel · loop or tap each channel</p>
    </div>
  </div>
  <nav class="page-nav">
    <a href="/">SEQ+MIX</a>
    <a href="/mixer" class="active">DESK</a>
    <a href="/pads">PADS</a>
    <a href="/library">CRATES</a>
  </nav>
</header>

<div class="desk">
  <div class="strips" id="strips"></div>

  <div class="master">
    <span class="card-title">Master Bus</span>
    <canvas id="scope" width="204" height="64"></canvas>
    <div class="mvol">
      <span>OUT</span>
      <input id="masterVol" type="range" min="0" max="120" value="90" oninput="setMaster(this.value/100)" />
      <b id="masterVolLbl">90%</b>
    </div>
    <button onclick="panic()">⏹ PANIC / STOP ALL</button>
    <div class="status-banner" style="margin-top:0;" id="statusText">Ready. TAP a channel or hit LOOP to ride it.</div>
  </div>
</div>

<script>
let STEMS = [];
let ch = {};
let audioCtx = null;
let masterGain = null, masterAnalyser = null;
const rawBufs = {}, trimBufs = {};

function ensureAudio() {
  if (!audioCtx) {
    const AC = window.AudioContext || window.webkitAudioContext;
    audioCtx = new AC();
    masterGain = audioCtx.createGain();
    masterGain.gain.value = 0.9;
    masterAnalyser = audioCtx.createAnalyser();
    masterAnalyser.fftSize = 512;
    masterGain.connect(masterAnalyser);
    masterAnalyser.connect(audioCtx.destination);
    drawMeters();
  }
  if (audioCtx.state === 'suspended') audioCtx.resume();
  for (const s of STEMS) ensureChannel(s.id);
}

function ensureChannel(id) {
  if (ch[id].g) return;
  const g = audioCtx.createGain();
  const p = audioCtx.createStereoPanner();
  const a = audioCtx.createAnalyser();
  a.fftSize = 256;
  g.connect(p); p.connect(a); a.connect(masterGain);
  ch[id].g = g; ch[id].p = p; ch[id].a = a;
  applyGain(id);
  p.pan.value = ch[id].pan;
}

async function rawBuffer(stem, dry) {
  const url = dry ? stem.dryUrl : stem.url;
  if (rawBufs[url]) return rawBufs[url];
  const res = await fetch(url);
  const arr = await res.arrayBuffer();
  rawBufs[url] = await audioCtx.decodeAudioData(arr);
  return rawBufs[url];
}

async function trimmedBuffer(stem, dry) {
  const key = stem.id + (dry ? '|d' : '|w');
  if (trimBufs[key]) return trimBufs[key];
  const src = await rawBuffer(stem, dry);
  const n = Math.min(src.length, Math.floor(stem.trimSec * src.sampleRate));
  const out = audioCtx.createBuffer(src.numberOfChannels, n, src.sampleRate);
  const fade = Math.min(n, Math.floor(0.008 * src.sampleRate));
  for (let c = 0; c < src.numberOfChannels; c++) {
    const d = src.getChannelData(c).slice(0, n);
    for (let i = 0; i < fade; i++) d[n - 1 - i] *= i / Math.max(1, fade);
    out.copyToChannel(d, c);
  }
  trimBufs[key] = out;
  return out;
}

function fireSource(id, loop) {
  const stem = STEMS.find(x => x.id === id);
  const st = ch[id];
  return trimmedBuffer(stem, st.useDry).then(buf => {
    const src = audioCtx.createBufferSource();
    src.buffer = buf;
    src.playbackRate.value = Math.pow(2, (st.pitch || 0) / 12);
    src.loop = !!loop;
    src.connect(st.g);
    src.start();
    return src;
  });
}

function applyGain(id) {
  const st = ch[id];
  if (!st.g) return;
  const anySolo = STEMS.some(s => ch[s.id].solo);
  const eff = (st.mute || (anySolo && !st.solo)) ? 0 : st.gain;
  st.g.gain.setTargetAtTime(eff, audioCtx.currentTime, 0.02);
}

function setParam(id, key, val) {
  ch[id][key] = val;
  if (key === 'gain') {
    document.getElementById(`vlbl-${id}`).textContent = `${Math.round(val*100)}%`;
    applyGain(id);
  }
  if (key === 'pan') {
    document.getElementById(`plbl-${id}`).textContent =
      Math.abs(val) < 0.05 ? 'C' : (val < 0 ? `${Math.round(-val*100)}L` : `${Math.round(val*100)}R`);
    if (ch[id].p) ch[id].p.pan.value = val;
  }
  if (key === 'pitch') {
    document.getElementById(`tlbl-${id}`).textContent = `${val >= 0 ? '+'+val : val}st`;
  }
}

function toggleFlag(id, flag, cls) {
  ch[id][flag] = !ch[id][flag];
  document.getElementById(`btn-${flag}-${id}`).classList.toggle(cls, ch[id][flag]);
  if (flag === 'mute' || flag === 'solo') STEMS.forEach(s => applyGain(s.id));
  if (flag === 'useDry') { delete trimBufs[id + '|d']; delete trimBufs[id + '|w']; tap(id); }
}

async function tap(id) {
  ensureAudio();
  const el = document.getElementById(`tap-${id}`);
  el.classList.add('hit'); setTimeout(() => el.classList.remove('hit'), 130);
  const src = await fireSource(id, false);
  src.onended = () => { if (ch[id].loopSrc === src) ch[id].loopSrc = null; };
}

async function toggleLoop(id) {
  ensureAudio();
  const st = ch[id];
  const btn = document.getElementById(`loop-${id}`);
  if (st.loopSrc) { try { st.loopSrc.stop(); } catch(e) {} st.loopSrc = null; btn.classList.remove('on'); btn.textContent = 'LOOP'; return; }
  st.loopSrc = await fireSource(id, true);
  btn.classList.add('on'); btn.textContent = 'LOOPING';
}

function setMaster(v) {
  if (masterGain) masterGain.gain.value = v;
  document.getElementById('masterVolLbl').textContent = `${Math.round(v*100)}%`;
}

function panic() {
  for (const s of STEMS) {
    const st = ch[s.id];
    if (st.loopSrc) { try { st.loopSrc.stop(); } catch(e) {} st.loopSrc = null; }
    const btn = document.getElementById(`loop-${s.id}`);
    if (btn) { btn.classList.remove('on'); btn.textContent = 'LOOP'; }
  }
}

function buildStrips() {
  const wrap = document.getElementById('strips');
  wrap.innerHTML = '';
  for (const stem of STEMS) {
    const st = ch[stem.id];
    const d = document.createElement('div');
    d.className = 'strip';
    d.style.setProperty('--stem-color', stem.color);
    const panLbl = Math.abs(st.pan) < 0.05 ? 'C' : (st.pan < 0 ? `${Math.round(-st.pan*100)}L` : `${Math.round(st.pan*100)}R`);
    d.innerHTML = `
      <div class="strip-name">${stem.name}</div>
      <div class="strip-sub">"${stem.phrase}"<br>${stem.effect}</div>
      <div class="strip-mid">
        <input class="fader" type="range" orient="vertical" min="0" max="140" value="${Math.round(st.gain*100)}"
          oninput="setParam('${stem.id}','gain',this.value/100)" />
        <canvas class="vu" id="vu-${stem.id}" width="12" height="128"></canvas>
      </div>
      <div class="strip-slider"><span>VOL <b id="vlbl-${stem.id}">${Math.round(st.gain*100)}%</b></span></div>
      <div class="strip-slider">
        <span>PAN <b id="plbl-${stem.id}">${panLbl}</b></span>
        <input type="range" min="-100" max="100" value="${Math.round(st.pan*100)}"
          oninput="setParam('${stem.id}','pan',this.value/100)" />
      </div>
      <div class="strip-slider">
        <span>PITCH <b id="tlbl-${stem.id}">${st.pitch >= 0 ? '+'+st.pitch : st.pitch}st</b></span>
        <input type="range" min="-12" max="18" value="${st.pitch}"
          oninput="setParam('${stem.id}','pitch',parseInt(this.value,10))" />
      </div>
      <div class="strip-btns">
        <button class="mini-btn" id="btn-useDry-${stem.id}" onclick="toggleFlag('${stem.id}','useDry','dry-on')">DRY</button>
        <button class="mini-btn" id="btn-mute-${stem.id}" onclick="toggleFlag('${stem.id}','mute','mute-on')">M</button>
        <button class="mini-btn" id="btn-solo-${stem.id}" onclick="toggleFlag('${stem.id}','solo','solo-on')">S</button>
      </div>
      <button class="tap" id="tap-${stem.id}" onclick="tap('${stem.id}')">TAP</button>
      <button class="mini-btn loop" id="loop-${stem.id}" onclick="toggleLoop('${stem.id}')">LOOP</button>
    `;
    wrap.appendChild(d);
  }
}

function drawMeters() {
  const scope = document.getElementById('scope');
  const sctx = scope.getContext('2d');
  const sdata = new Uint8Array(masterAnalyser.frequencyBinCount);
  const vdata = new Uint8Array(256);

  function frame() {
    requestAnimationFrame(frame);
    for (const stem of STEMS) {
      const st = ch[stem.id];
      const cv = document.getElementById(`vu-${stem.id}`);
      if (!cv || !st.a) continue;
      const vc = cv.getContext('2d');
      st.a.getByteTimeDomainData(vdata);
      let sum = 0;
      for (let i = 0; i < 64; i++) { const v = (vdata[i*4] - 128) / 128; sum += v * v; }
      const lvl = Math.min(1, Math.sqrt(sum / 64) * 2.6);
      vc.fillStyle = '#0b0908';
      vc.fillRect(0, 0, cv.width, cv.height);
      const h = lvl * cv.height;
      vc.fillStyle = lvl > 0.9 ? '#ef4444' : stem.color;
      vc.fillRect(0, cv.height - h, cv.width, h);
    }
    masterAnalyser.getByteTimeDomainData(sdata);
    sctx.fillStyle = '#0b0908';
    sctx.fillRect(0, 0, scope.width, scope.height);
    sctx.lineWidth = 1.6;
    sctx.strokeStyle = '#ffb347';
    sctx.beginPath();
    const slice = scope.width / sdata.length;
    let x = 0;
    for (let i = 0; i < sdata.length; i++) {
      const y = (sdata[i] / 128.0) * scope.height / 2;
      if (i === 0) sctx.moveTo(x, y); else sctx.lineTo(x, y);
      x += slice;
    }
    sctx.stroke();
  }
  frame();
}

async function init() {
  const res = await fetch('/api/state');
  const data = await res.json();
  STEMS = data.stems;
  for (const s of STEMS) {
    ch[s.id] = {
      gain: s.defaultGain, pan: s.defaultPan, pitch: s.defaultPitch,
      mute: false, solo: false, useDry: false,
      g: null, p: null, a: null, loopSrc: null,
    };
  }
  buildStrips();
}

window.addEventListener('DOMContentLoaded', init);
</script>
</body>
</html>
"""


PADS_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>BROWNNOTE // Fart MPC Pads</title>
<style>""" + PAGE_CSS + r"""
  .pad-layout {
    display: grid;
    grid-template-columns: 1fr 260px;
    gap: 14px;
  }
  @media (max-width: 860px) { .pad-layout { grid-template-columns: 1fr; } }
  .pad-grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 10px;
  }
  .pad {
    aspect-ratio: 1.35;
    border-radius: 10px;
    border: 1px solid var(--border);
    border-bottom: 4px solid var(--stem-color, var(--accent));
    background: linear-gradient(180deg, var(--panel-2), #171210);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    cursor: pointer;
    user-select: none;
    transition: transform 0.05s ease, background 0.08s ease;
  }
  .pad:hover { border-color: var(--stem-color, var(--accent)); }
  .pad.hit {
    background: var(--stem-color, var(--accent));
    transform: scale(0.95);
  }
  .pad.hit .pad-label, .pad.hit .pad-key, .pad.hit .pad-sub { color: #090807; }
  .pad-label { font-size: 12px; font-weight: 800; letter-spacing: 0.04em; }
  .pad-sub { font-size: 9px; color: var(--muted); }
  .pad-key {
    font-size: 9px;
    color: var(--stem-color, var(--accent));
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 1px 6px;
  }
  .side { display: flex; flex-direction: column; gap: 10px; }
  .side canvas#scope { width: 100%; height: 64px; background: #0b0908; border: 1px solid var(--border); border-radius: 6px; }
  .legend { color: var(--muted); font-size: 10px; line-height: 1.7; }
  .legend b { color: var(--accent-hi); }
  .mvol { display: flex; align-items: center; gap: 8px; font-size: 9px; color: var(--muted); }
  .mvol input { flex: 1; accent-color: var(--accent); }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE PADS // FART MPC</h1>
      <p>16 finger-drum pads over the stems · choke per stem · hit high on the pad for max velocity</p>
    </div>
  </div>
  <nav class="page-nav">
    <a href="/">SEQ+MIX</a>
    <a href="/mixer">DESK</a>
    <a href="/pads" class="active">PADS</a>
    <a href="/library">CRATES</a>
  </nav>
</header>

<div class="pad-layout">
  <section class="card">
    <div class="card-header">
      <span class="card-title">Pad Grid — keys 1-4 / Q W E R / A S D F / Z X C V</span>
      <button class="mini-btn" id="dryBtn" onclick="toggleAllDry()">DRY KIT</button>
    </div>
    <div class="pad-grid" id="padGrid"></div>
    <div class="status-banner" id="statusText">Whack a pad. Same stem chokes its own tail — flam away.</div>
  </section>

  <div class="side">
    <section class="card">
      <div class="card-header"><span class="card-title">Output</span></div>
      <canvas id="scope" width="230" height="64"></canvas>
      <div class="mvol" style="margin-top:8px;">
        <span>OUT</span>
        <input type="range" min="0" max="120" value="95" oninput="setMaster(this.value/100)" />
        <b id="mvolLbl">95%</b>
      </div>
    </section>
    <section class="card legend">
      <b>PADS:</b> bombs on top row, lead second, wah/snare/hat third, kicks &amp; thuds on the bottom.<br>
      <b>VELOCITY:</b> click near the top edge of a pad to smack it, bottom edge for a soft poot.<br>
      <b>CHOKE:</b> retriggering a pad from the same stem cuts the previous tail.
    </section>
  </div>
</div>

<script>
const PADS = [
  { stem: 'bomb',  semi: 0,   key: '1', label: 'BROWN BOMB' },
  { stem: 'bomb',  semi: -5,  key: '2', label: 'BOMB -5' },
  { stem: 'bomb',  semi: -12, key: '3', label: 'SEWER BOMB' },
  { stem: 'bomb',  semi: 7,   key: '4', label: 'BOMB +7' },
  { stem: 'lead',  semi: 0,   key: 'q', label: 'FUZZ LEAD' },
  { stem: 'lead',  semi: 7,   key: 'w', label: 'LEAD +7' },
  { stem: 'lead',  semi: 12,  key: 'e', label: 'LEAD OCT' },
  { stem: 'lead',  semi: -7,  key: 'r', label: 'SLUDGE' },
  { stem: 'wah',   semi: 0,   key: 'a', label: 'WAH FUNK' },
  { stem: 'wah',   semi: 5,   key: 's', label: 'WAH +5' },
  { stem: 'snare', semi: 0,   key: 'd', label: 'SPLAT' },
  { stem: 'hat',   semi: 0,   key: 'f', label: 'HAT' },
  { stem: 'kick',  semi: 0,   key: 'z', label: 'SUB KICK' },
  { stem: 'kick',  semi: -5,  key: 'x', label: 'KICK -5' },
  { stem: 'snare', semi: -12, key: 'c', label: 'THUD' },
  { stem: 'wah',   semi: -7,  key: 'v', label: 'WAH -7' },
];

let STEMS = [];
let audioCtx = null;
let masterGain = null, masterAnalyser = null;
let allDry = false;
const rawBufs = {}, trimBufs = {};
const lastSrc = {};

function ensureAudio() {
  if (!audioCtx) {
    const AC = window.AudioContext || window.webkitAudioContext;
    audioCtx = new AC();
    masterGain = audioCtx.createGain();
    masterGain.gain.value = 0.95;
    masterAnalyser = audioCtx.createAnalyser();
    masterAnalyser.fftSize = 512;
    masterGain.connect(masterAnalyser);
    masterAnalyser.connect(audioCtx.destination);
    drawScope();
  }
  if (audioCtx.state === 'suspended') audioCtx.resume();
}

async function padBuffer(stem) {
  const url = allDry ? stem.dryUrl : stem.url;
  if (!rawBufs[url]) {
    const res = await fetch(url);
    const arr = await res.arrayBuffer();
    rawBufs[url] = await audioCtx.decodeAudioData(arr);
  }
  const src = rawBufs[url];
  const key = url + '|t';
  if (trimBufs[key]) return trimBufs[key];
  const n = Math.min(src.length, Math.floor(stem.trimSec * src.sampleRate));
  const out = audioCtx.createBuffer(src.numberOfChannels, n, src.sampleRate);
  const fade = Math.min(n, Math.floor(0.008 * src.sampleRate));
  for (let c = 0; c < src.numberOfChannels; c++) {
    const d = src.getChannelData(c).slice(0, n);
    for (let i = 0; i < fade; i++) d[n - 1 - i] *= i / Math.max(1, fade);
    out.copyToChannel(d, c);
  }
  trimBufs[key] = out;
  return out;
}

async function hitPad(i, vel) {
  ensureAudio();
  const pad = PADS[i];
  const stem = STEMS.find(s => s.id === pad.stem);
  if (!stem) return;

  const el = document.getElementById(`pad-${i}`);
  el.classList.add('hit');
  setTimeout(() => el.classList.remove('hit'), 130);

  if (lastSrc[pad.stem]) {
    try { lastSrc[pad.stem].stop(); } catch(e) {}
    lastSrc[pad.stem] = null;
  }

  const buf = await padBuffer(stem);
  const src = audioCtx.createBufferSource();
  src.buffer = buf;
  src.playbackRate.value = Math.pow(2, pad.semi / 12);
  const g = audioCtx.createGain();
  g.gain.value = vel;
  src.connect(g); g.connect(masterGain);
  src.start();
  lastSrc[pad.stem] = src;
  src.onended = () => { if (lastSrc[pad.stem] === src) lastSrc[pad.stem] = null; };
  document.getElementById('statusText').textContent = `💥 ${pad.label} @ vel ${Math.round(vel*100)}%`;
}

function toggleAllDry() {
  allDry = !allDry;
  document.getElementById('dryBtn').classList.toggle('dry-on', allDry);
  document.getElementById('statusText').textContent = allDry ? 'DRY KIT — raw kujamba, no pedals.' : 'WET KIT — full GuitarPedal chains.';
}

function setMaster(v) {
  if (masterGain) masterGain.gain.value = v;
  document.getElementById('mvolLbl').textContent = `${Math.round(v*100)}%`;
}

function buildPads() {
  const grid = document.getElementById('padGrid');
  grid.innerHTML = '';
  PADS.forEach((pad, i) => {
    const stem = STEMS.find(s => s.id === pad.stem);
    const el = document.createElement('div');
    el.className = 'pad';
    el.id = `pad-${i}`;
    el.style.setProperty('--stem-color', stem ? stem.color : '#e07a3c');
    el.innerHTML = `
      <span class="pad-label">${pad.label}</span>
      <span class="pad-sub">${stem ? stem.name : pad.stem} ${pad.semi ? (pad.semi>0?'+':'') + pad.semi + 'st' : ''}</span>
      <span class="pad-key">${pad.key.toUpperCase()}</span>
    `;
    el.addEventListener('pointerdown', (ev) => {
      const r = el.getBoundingClientRect();
      const frac = Math.min(1, Math.max(0, (ev.clientY - r.top) / r.height));
      hitPad(i, 1.15 - frac * 0.75);
    });
    grid.appendChild(el);
  });
}

document.addEventListener('keydown', (ev) => {
  if (ev.repeat) return;
  const i = PADS.findIndex(p => p.key === ev.key.toLowerCase());
  if (i >= 0) hitPad(i, 1.0);
});

function drawScope() {
  const cv = document.getElementById('scope');
  const ctx = cv.getContext('2d');
  const data = new Uint8Array(masterAnalyser.frequencyBinCount);
  function frame() {
    requestAnimationFrame(frame);
    masterAnalyser.getByteTimeDomainData(data);
    ctx.fillStyle = '#0b0908';
    ctx.fillRect(0, 0, cv.width, cv.height);
    ctx.lineWidth = 1.6;
    ctx.strokeStyle = '#ffb347';
    ctx.beginPath();
    const slice = cv.width / data.length;
    let x = 0;
    for (let i = 0; i < data.length; i++) {
      const y = (data[i] / 128.0) * cv.height / 2;
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
      x += slice;
    }
    ctx.stroke();
  }
  frame();
}

async function init() {
  const res = await fetch('/api/state');
  const data = await res.json();
  STEMS = data.stems;
  buildPads();
}

window.addEventListener('DOMContentLoaded', init);
</script>
</body>
</html>
"""


LIBRARY_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>BROWNNOTE // Crate Digger Library</title>
<style>""" + PAGE_CSS + r"""
  .filter-bar { display: flex; flex-wrap: wrap; gap: 6px; }
  .filter-btn.active { border-color: var(--accent-hi); background: #3a2618; color: var(--accent-hi); }
  .crate-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 10px;
  }
  .crate {
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-left: 3px solid var(--kind-color, var(--accent));
    border-radius: 8px;
    padding: 9px 10px;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .crate.playing { border-color: var(--kind-color, var(--accent)); box-shadow: 0 0 12px rgba(224,122,60,0.25); }
  .crate-top { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .crate-name { font-weight: 700; font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .crate-meta { display: flex; align-items: center; gap: 6px; font-size: 9.5px; color: var(--muted); }
  .crate canvas.wave {
    width: 100%;
    height: 44px;
    background: #0b0908;
    border: 1px solid var(--border);
    border-radius: 5px;
  }
  .crate-btns { display: flex; align-items: center; gap: 6px; }
  .crate-btns a { color: var(--accent-hi); text-decoration: none; font-size: 10.5px; }
  .kind-Song { --kind-color: #a855f7; }
  .kind-FX-Preset { --kind-color: #e07a3c; }
  .kind-Custom { --kind-color: #4ecdc4; }
  .kind-Recording { --kind-color: #22c55e; }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE CRATES // RENDERED LIBRARY</h1>
      <p>Every MP3 &amp; WAV in build/presets + recordings/ — waveforms drawn on first decode</p>
    </div>
  </div>
  <nav class="page-nav">
    <a href="/">SEQ+MIX</a>
    <a href="/mixer">DESK</a>
    <a href="/pads">PADS</a>
    <a href="/library" class="active">CRATES</a>
  </nav>
</header>

<section class="card">
  <div class="card-header">
    <div class="filter-bar" id="filterBar">
      <button class="filter-btn active" data-kind="all" onclick="setFilter('all', this)">All</button>
      <button class="filter-btn" data-kind="Song" onclick="setFilter('Song', this)">Songs</button>
      <button class="filter-btn" data-kind="FX Preset" onclick="setFilter('FX Preset', this)">FX Presets</button>
      <button class="filter-btn" data-kind="Custom" onclick="setFilter('Custom', this)">Custom</button>
      <button class="filter-btn" data-kind="Recording" onclick="setFilter('Recording', this)">Recordings</button>
    </div>
    <button class="mini-btn" onclick="loadCrates()">↻ Refresh</button>
  </div>
  <div class="crate-grid" id="crateGrid"></div>
  <div class="status-banner" id="statusText">Digging the crates…</div>
</section>

<audio id="player" style="display:none"></audio>

<script>
const player = document.getElementById('player');
let TRACKS = [];
let filter = 'all';
let audioCtx = null;
const peaks = {};   // url -> Float32Array of peaks 0..1
let playingUrl = null;

const KIND_CLASS = { 'Song': 'kind-Song', 'FX Preset': 'kind-FX-Preset', 'Custom': 'kind-Custom', 'Recording': 'kind-Recording' };

function ensureAudio() {
  if (!audioCtx) {
    const AC = window.AudioContext || window.webkitAudioContext;
    audioCtx = new AC();
  }
  if (audioCtx.state === 'suspended') audioCtx.resume();
}

async function decodePeaks(url) {
  if (peaks[url]) return peaks[url];
  ensureAudio();
  const res = await fetch(url);
  const arr = await res.arrayBuffer();
  const buf = await audioCtx.decodeAudioData(arr);
  const ch0 = buf.getChannelData(0);
  const buckets = 160;
  const out = new Float32Array(buckets);
  const step = Math.max(1, Math.floor(ch0.length / buckets));
  for (let b = 0; b < buckets; b++) {
    let m = 0;
    const off = b * step;
    for (let i = 0; i < step && off + i < ch0.length; i += 7) {
      const v = Math.abs(ch0[off + i]);
      if (v > m) m = v;
    }
    out[b] = m;
  }
  peaks[url] = out;
  return out;
}

function drawWave(url, progress) {
  const cv = document.querySelector(`canvas[data-url="${cssEscape(url)}"]`);
  if (!cv) return;
  const ctx = cv.getContext('2d');
  ctx.fillStyle = '#0b0908';
  ctx.fillRect(0, 0, cv.width, cv.height);
  const p = peaks[url];
  if (!p) {
    ctx.fillStyle = '#463a33';
    ctx.font = '9px monospace';
    ctx.fillText('press ▶ to decode waveform', 8, cv.height / 2 + 3);
    return;
  }
  const w = cv.width / p.length;
  for (let i = 0; i < p.length; i++) {
    const h = Math.max(1, p[i] * cv.height * 0.92);
    const frac = i / p.length;
    ctx.fillStyle = frac < progress ? '#ffb347' : '#6b574b';
    ctx.fillRect(i * w, (cv.height - h) / 2, Math.max(1, w - 0.5), h);
  }
}

function cssEscape(s) { return s.replace(/"/g, '\\"'); }

async function toggleTrack(url) {
  ensureAudio();
  if (playingUrl === url && !player.paused) {
    player.pause();
    return;
  }
  playingUrl = url;
  player.src = url;
  player.play().catch(() => {});
  try {
    await decodePeaks(url);
    drawWave(url, 0);
  } catch (e) {
    document.getElementById('statusText').textContent = `⚠ could not decode ${url}`;
  }
  markPlaying();
}

function markPlaying() {
  document.querySelectorAll('.crate').forEach(c => {
    c.classList.toggle('playing', c.dataset.url === playingUrl && !player.paused);
    const b = c.querySelector('.play-btn');
    if (b) b.textContent = (c.dataset.url === playingUrl && !player.paused) ? '⏸' : '▶';
  });
}

player.addEventListener('play', markPlaying);
player.addEventListener('pause', markPlaying);
player.addEventListener('ended', markPlaying);

(function playhead() {
  requestAnimationFrame(playhead);
  if (!playingUrl || player.paused || !peaks[playingUrl]) return;
  drawWave(playingUrl, player.duration ? player.currentTime / player.duration : 0);
})();

function setFilter(kind, btn) {
  filter = kind;
  document.querySelectorAll('.filter-btn').forEach(b => b.classList.remove('active'));
  btn.classList.add('active');
  renderCrates();
}

function renderCrates() {
  const grid = document.getElementById('crateGrid');
  grid.innerHTML = '';
  const shown = TRACKS.filter(t => filter === 'all' || t.kind === filter);
  document.getElementById('statusText').textContent =
    shown.length ? `${shown.length} record${shown.length === 1 ? '' : 's'} in the crate.` : 'Crate is empty — go render something on the SEQ page.';
  for (const t of shown) {
    const el = document.createElement('div');
    el.className = `crate ${KIND_CLASS[t.kind] || ''}`;
    el.dataset.url = t.url;
    el.innerHTML = `
      <div class="crate-top">
        <span class="crate-name" title="${t.name}">${t.name}</span>
        <span class="tag">${t.kind}</span>
      </div>
      <canvas class="wave" data-url="${t.url}" width="300" height="44"></canvas>
      <div class="crate-top">
        <div class="crate-meta"><span>${t.sizeKb} KB</span></div>
        <div class="crate-btns">
          <button class="mini-btn play-btn" onclick="toggleTrack('${t.url}')">▶</button>
          <a href="${t.url}" download>⬇</a>
        </div>
      </div>
    `;
    grid.appendChild(el);
    drawWave(t.url, 0);
  }
  markPlaying();
}

async function loadCrates() {
  const res = await fetch('/api/state');
  const data = await res.json();
  TRACKS = data.tracks || [];
  renderCrates();
}

window.addEventListener('DOMContentLoaded', loadCrates);
</script>
</body>
</html>
"""


EXTRA_PAGES = {
    "/mixer": MIXER_HTML,
    "/pads": PADS_HTML,
    "/library": LIBRARY_HTML,
}


if __name__ == "__main__":
    try:
        ensure_stems()
    except Exception as e:
        print(f"warning: stem build failed ({e}); /api/state will retry and 503", file=sys.stderr)
    with ReusableTCPServer((HOST, PORT), StudioHandler) as httpd:
        print(f"Brownnote Studio listening on http://{HOST}:{PORT}", flush=True)
        print("  pages: / (seq+mixer)  /mixer (desk)  /pads (MPC)  /library (crates)", flush=True)
        httpd.serve_forever()
