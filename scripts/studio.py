#!/usr/bin/env python3
"""
studio.py — Local HTTP studio backend for the Brownnote 16-Step Sequencer,
Stem Mixer, Jukebox, and Custom Pedalboard UI.

Serves build/studio.html at `/` plus workspace audio files and JSON APIs:
  GET  /api/state       -> available stems, preset MP3s, and effects list
  POST /api/render-fx   -> synthesize & pedal a custom phrase, return MP3/WAV URL
  POST /api/render-song -> render a full multi-track genre song or custom sequence to MP3
  GET  /api/patches     -> [{name, mtime}] for every saved studio patch
  GET  /api/patch?name= -> the named patch JSON
  POST /api/patch       -> {name, patch} saves; {name, delete: true} deletes
                           (whole-studio snapshot, schema v1)
  POST /api/upload-sample?name=<name>
                        -> Content-Type: audio/wav, raw 44.1 kHz mono s16
                           WAV body (10 MiB cap): saves build/user/<slug>.wav
                           + a stem def and returns it; JSON {name|id,
                           delete: true} removes both.  Anything but
                           44.1k/mono/s16 PCM 400s — the browser resamples
                           before sending.
  GET  /api/jam       -> NINJAM session status: state, bpm/bpi, last RESULT
  POST /api/jam       -> {host, user, pass[, seq]} spawns kujamba join and
                           feeds it build/jam/loop.wav; {seq} re-renders the
                           loop; {stop: true} leaves and wipes build/jam/

Library (#53): build/library.json is the flatulence manifest — one catalog
of every usable WAV/MP3 under build/{tune,presets,library,user} and
recordings/, each entry flagged with the stem it backs when playable.
build/library/ is the intake dir: a conforming 44.1 kHz mono s16 WAV
dropped there becomes a lib-* stem on rescan; anything else is cataloged
raw and can be normalized into a stem by promote.  The manifest's
"default" names THE fart; /api/state resolves it, falling back to "bomb"
then the first stem when it's gone.

  GET  /api/library     -> {ok, entries, default, defaultSet}
  POST /api/library/rescan   -> rebuild the manifest (runs intake)
  POST /api/library/promote  -> {id, name?} makes a catalog WAV a stem
  POST /api/library/default  -> {id|null} picks the canonical fart
  POST /api/library/intake?name=<name>
                        -> Content-Type: audio/wav — same rules as
                           upload-sample, but lands in build/library/

Extra views over the same stems (every page carries the same page-nav bar):
  /mixer    -> console desk with VU meters, tap/loop per strip (+ user stems)
               (Web MIDI: CC20-25 ch gain, CC30-35 ch pan, CC7 master)
  /pads     -> 16-pad MPC-style finger drummer (keys 1-4/QWER/ASDF/ZXCV)
               plus the in-browser recorder that produces user stems,
              + event looper: record pad hits quantized to 16ths, overdub
              stacked takes, mute/clear per layer, bounce the loop to MP3
               (Web MIDI: notes base..base+15 hit pads, default base 36)
  /library  -> crate-digger: every rendered MP3/WAV with drawn waveforms
  /jam      -> NINJAM bridge console (#49): join a server, stream the
               current sequence as the interval loop via kujamba
"""
import http.server
import io
import json
import math
import os
import posixpath
import re
import shutil
import signal
import socketserver
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import wave
import array
import zlib

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
    if os.path.isdir(USER_DIR):
        for fn in sorted(os.listdir(USER_DIR)):
            if fn.endswith(".wav"):
                p = os.path.join(USER_DIR, fn)
                label = fn
                try:
                    with open(os.path.join(USER_DIR, fn[:-4] + ".json"),
                              "r", encoding="utf-8") as f:
                        label = json.load(f).get("name") or fn
                except Exception:
                    pass
                tracks.append({
                    "name": label,
                    "kind": "Sample",
                    "url": f"/build/user/{fn}",
                    "sizeKb": round(os.path.getsize(p) / 1024, 1),
                })
    return tracks


PATCHES_DIR = os.path.join(ROOT, "build", "patches")
PATCH_NAME_RE = re.compile(r"^[A-Za-z0-9 _+\-]{1,64}$")
PATCH_MAX_BYTES = 256 * 1024
PATCH_KEYS = {"v", "name", "seq", "desk", "pads", "key", "seed"}


def patch_path(name):
    """Validated patch name -> path inside build/patches/, else None."""
    if not isinstance(name, str) or not PATCH_NAME_RE.match(name):
        return None
    path = os.path.join(PATCHES_DIR, name + ".json")
    if os.path.dirname(os.path.abspath(path)) != os.path.abspath(PATCHES_DIR):
        return None
    return path


def list_patches():
    out = []
    if os.path.isdir(PATCHES_DIR):
        for fn in sorted(os.listdir(PATCHES_DIR)):
            if not fn.endswith(".json"):
                continue
            p = os.path.join(PATCHES_DIR, fn)
            if os.path.isfile(p):
                out.append({"name": fn[:-5], "mtime": os.path.getmtime(p)})
    return out


def save_patch(name, patch):
    path = patch_path(name)
    if path is None:
        raise ValueError("bad patch name")
    patch = dict(patch)
    patch["v"] = 1
    patch["name"] = name
    os.makedirs(PATCHES_DIR, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=PATCHES_DIR, prefix=".tmp-", suffix=".json")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(json.dumps(patch, indent=2) + "\n")
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


# User samples (#43): recorded in the browser, uploaded as 44.1 kHz mono
# s16 WAV (the only shape load_wav + the WebAudio pages assume).  Each one
# is a stem def under build/user/ so /api/state, /pads, /mixer, and the
# CRATES page pick it up; they are not sequencer lanes in v1.
USER_DIR = os.path.join(ROOT, "build", "user")
SAMPLE_NAME_RE = re.compile(r"^[A-Za-z0-9 _\-]{1,48}$")
SAMPLE_MAX_BYTES = 10 * 1024 * 1024
USER_WAV_TYPES = {"audio/wav", "audio/x-wav", "audio/wave", "audio/vnd.wave"}
USER_COLORS = [
    "#38bdf8", "#f472b6", "#facc15", "#34d399",
    "#fb923c", "#c084fc", "#f87171", "#a3e635",
]


def user_slug(name):
    return re.sub(r"[ _]+", "_", name.strip().lower())


def user_paths(name):
    """Validated sample name -> (wav, json) paths inside build/user/, else None."""
    if not isinstance(name, str) or not SAMPLE_NAME_RE.match(name):
        return None
    slug = user_slug(name)
    if not slug:
        return None
    wav = os.path.join(USER_DIR, slug + ".wav")
    js = os.path.join(USER_DIR, slug + ".json")
    if os.path.dirname(os.path.abspath(wav)) != os.path.abspath(USER_DIR):
        return None
    return wav, js


def _stem_defs_in(path, flag):
    """Every <dir>/*.json stem def whose audio file still exists."""
    defs = []
    if os.path.isdir(path):
        for fn in sorted(os.listdir(path)):
            if not fn.endswith(".json"):
                continue
            try:
                with open(os.path.join(path, fn), "r",
                          encoding="utf-8") as f:
                    d = json.load(f)
            except Exception:
                continue   # a corrupt def file shouldn't break /api/state
            if not (isinstance(d, dict) and d.get("id") and d.get("url")):
                continue
            if not os.path.isfile(
                    os.path.join(ROOT, d["url"].lstrip("/"))):
                continue   # the audio is gone — don't offer a dead stem
            d[flag] = True
            defs.append(d)
    return defs


def user_stem_defs():
    """Every build/user/*.json stem def, flagged user:true."""
    return _stem_defs_in(USER_DIR, "user")


def stem_defs():
    synth = [s for s in STEM_DEFS
             if os.path.isfile(os.path.join(ROOT, s["url"].lstrip("/")))]
    return synth + user_stem_defs() + lib_stem_defs()


def _atomic_write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".tmp-")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def check_sample_wav(body):
    """Raw WAV body -> frame count; raises ValueError unless it is the
    44.1 kHz mono s16 shape the pages assume (#43)."""
    try:
        with wave.open(io.BytesIO(body), "rb") as w:
            ch = w.getnchannels()
            width = w.getsampwidth()
            rate = w.getframerate()
            frames = w.getnframes()
    except Exception as e:
        raise ValueError(f"not a readable PCM WAV: {e}")
    if (rate, ch, width) != (44100, 1, 2):
        raise ValueError(
            f"WAV must be 44.1 kHz mono s16 "
            f"(got {rate} Hz, {ch} channel(s), {width * 8}-bit)")
    if frames <= 0:
        raise ValueError("empty clip")
    return frames


def save_user_sample(name, body):
    """Validate a raw WAV body + name, write build/user/<slug>.{wav,json},
    and return the stem def.  Raises ValueError for a 400 reply."""
    paths = user_paths(name)
    if paths is None:
        raise ValueError("bad sample name (want [A-Za-z0-9 _-], 1-48 chars)")
    wav_path, def_path = paths
    frames = check_sample_wav(body)

    slug = user_slug(name)
    stem_def = {
        "id": f"user-{slug}",
        "name": name.strip(),
        "phrase": "recorded",
        "effect": "user sample",
        "url": f"/build/user/{slug}.wav",
        "dryUrl": f"/build/user/{slug}.wav",
        "color": USER_COLORS[
            zlib.crc32(slug.encode("utf-8")) % len(USER_COLORS)],
        "trimSec": round(frames / 44100.0, 3),
        "defaultGain": 0.9,
        "defaultPan": 0.0,
        "defaultPitch": 0,
        "user": True,
    }
    os.makedirs(USER_DIR, exist_ok=True)
    _atomic_write(wav_path, body)
    _atomic_write(def_path,
                  (json.dumps(stem_def, indent=2) + "\n").encode("utf-8"))
    return stem_def


def delete_user_sample(name):
    """Remove build/user/<slug>.{wav,json}; True if anything was there."""
    wav_path, def_path = user_paths(name)
    gone = False
    for p in (wav_path, def_path):
        if os.path.isfile(p):
            os.unlink(p)
            gone = True
    return gone


# Flatulence library (#53): build/library.json catalogs every WAV/MP3
# under the buckets below.  Stem defs own their backing files; everything
# else is a catalog-only entry until promote (or a conforming drop into
# build/library/, which rescan turns into a lib-* stem automatically).
# "retired" holds stem ids deleted on purpose so intake doesn't resurrect
# them.
LIB_DIR = os.path.join(ROOT, "build", "library")
LIB_MANIFEST = os.path.join(ROOT, "build", "library.json")
LIB_BUCKETS = (
    ("tune", os.path.join("build", "tune"), "render"),
    ("presets", os.path.join("build", "presets"), "render"),
    ("recordings", "recordings", "recorded"),
    ("library", os.path.join("build", "library"), "recorded"),
    ("user", os.path.join("build", "user"), "recorded"),
)


def lib_stem_defs():
    """Every build/library/*.json stem def, flagged lib:true."""
    defs = _stem_defs_in(LIB_DIR, "lib")
    for d in defs:
        d["user"] = True   # playable on PADS/DESK like recorder samples
    return defs


def lib_slug(text):
    """Any string -> a kebab-case slug safe for stem ids and filenames."""
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-") or "clip"


def _wav_format(path):
    """-> (rate, channels, sampwidth, frames) for a PCM wav, else None."""
    try:
        with wave.open(path, "rb") as w:
            return (w.getframerate(), w.getnchannels(),
                    w.getsampwidth(), w.getnframes())
    except Exception:
        return None


def _decode_wav(path):
    """PCM wav -> (rate, mono floats).  Raises ValueError."""
    try:
        with wave.open(path, "rb") as w:
            ch, width, rate = (w.getnchannels(), w.getsampwidth(),
                               w.getframerate())
            raw = w.readframes(w.getnframes())
    except Exception as e:
        raise ValueError(f"not a readable PCM WAV: {e}")
    n = len(raw) // width
    if width == 1:
        vals = [(b - 128) / 128.0 for b in raw]
    elif width == 2:
        a = array.array("h")
        a.frombytes(raw)
        vals = [v / 32768.0 for v in a]
    elif width == 3:
        vals = [int.from_bytes(raw[3 * i:3 * i + 3], "little", signed=True)
                / 8388608.0 for i in range(n)]
    elif width == 4:
        a = array.array("i")
        a.frombytes(raw)
        vals = [v / 2147483648.0 for v in a]
    else:
        raise ValueError(f"unsupported {width * 8}-bit PCM")
    if ch > 1:
        vals = [sum(vals[i:i + ch]) / ch for i in range(0, n - n % ch, ch)]
    return rate, vals


def normalize_wav(src, dst):
    """Any readable PCM wav -> 44.1 kHz mono s16 at dst (linear interp).
    Raises ValueError for a 400 reply."""
    rate, vals = _decode_wav(src)
    if not vals:
        raise ValueError("empty clip")
    if rate != 44100:
        ratio = rate / 44100.0
        n = len(vals)
        out = []
        for i in range(max(1, int(n / ratio))):
            pos = i * ratio
            j = int(pos)
            frac = pos - j
            if j + 1 < n:
                out.append(vals[j] * (1.0 - frac) + vals[j + 1] * frac)
            elif j < n:
                out.append(vals[j])
        vals = out
    pcm = bytearray()
    for v in vals:
        pcm += struct.pack("<h", int(max(-1.0, min(1.0, v)) * 32767))
    with wave.open(dst, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(44100)
        w.writeframes(bytes(pcm))


def load_manifest():
    try:
        with open(LIB_MANIFEST, "r", encoding="utf-8") as f:
            m = json.load(f)
        if isinstance(m, dict) and isinstance(m.get("entries"), list):
            return m
    except Exception:
        pass
    return {"v": 1, "default": None, "retired": [], "entries": []}


def save_manifest(m):
    _atomic_write(LIB_MANIFEST,
                  (json.dumps(m, indent=2) + "\n").encode("utf-8"))


def _lib_def(sid, name, url, dur, kind):
    return {
        "id": sid,
        "name": name,
        "phrase": "library",
        "effect": "library WAV" if kind == "recorded" else "promoted render",
        "url": url,
        "dryUrl": url,
        "color": USER_COLORS[zlib.crc32(sid.encode("utf-8")) % len(USER_COLORS)],
        "trimSec": dur,
        "defaultGain": 0.9,
        "defaultPan": 0.0,
        "defaultPitch": 0,
        "user": True,
        "lib": True,
        "kind": kind,
    }


def scan_library():
    """Rebuild build/library.json: claim files owned by stem defs, turn
    conforming build/library/ drops into lib-* stems, then catalog every
    remaining WAV/MP3 in the buckets.  Cached stats carry over for files
    whose mtime+size are unchanged."""
    old = load_manifest()
    retired = set(old.get("retired") or [])
    prev = {}
    for e in old.get("entries") or []:
        if isinstance(e, dict) and e.get("file"):
            prev[e["file"]] = e

    claimed = {}
    for sd in stem_defs():
        for k in ("url", "dryUrl"):
            if sd.get(k):
                claimed[sd[k].lstrip("/")] = sd["id"]

    def probe(rel):
        """-> (stat, durationSec, loudness, conform) or None."""
        p = os.path.join(ROOT, rel)
        try:
            st = os.stat(p)
        except OSError:
            return None
        e = prev.get(rel)
        if e and e.get("mtime") == st.st_mtime \
                and e.get("sizeKb") == round(st.st_size / 1024, 1):
            return st, e.get("durationSec"), e.get("loudness"), \
                e.get("conform")
        conform = False
        dur = loud = None
        if rel.endswith(".wav"):
            fmt = _wav_format(p)
            if fmt:
                conform = fmt[:3] == (44100, 1, 2) and fmt[3] > 0
                try:
                    rate, vals = _decode_wav(p)
                except ValueError:
                    vals = None
                if vals is not None:
                    dur = round(len(vals) / float(rate), 3)
                    loud = round(
                        max((abs(v) for v in vals), default=0.0), 3)
        return st, dur, loud, conform

    # intake: a conforming WAV dropped in build/library/ becomes playable
    if os.path.isdir(LIB_DIR):
        for fn in sorted(os.listdir(LIB_DIR)):
            rel = f"build/library/{fn}"
            if not fn.endswith(".wav") or rel in claimed:
                continue
            p = os.path.join(ROOT, rel)
            fmt = _wav_format(p)
            if not fmt or fmt[:3] != (44100, 1, 2) or fmt[3] <= 0:
                continue   # stays catalog-only; promote can normalize it
            sid = f"lib-{lib_slug(fn[:-4])}"
            def_path = os.path.join(LIB_DIR, sid + ".json")
            if sid in retired or os.path.isfile(def_path):
                continue
            stem_def = _lib_def(sid, fn[:-4], "/" + rel,
                                round(fmt[3] / 44100.0, 3), "recorded")
            _atomic_write(def_path,
                          (json.dumps(stem_def, indent=2) + "\n")
                          .encode("utf-8"))
            claimed[rel] = sid

    entries = []
    synth_ids = {s["id"] for s in STEM_DEFS}
    for sd in stem_defs():
        rel = sd["url"].lstrip("/")
        got = probe(rel)
        st, dur, loud, _ = got if got else (None, None, None, None)
        kind = "synth" if sd["id"] in synth_ids else sd.get("kind",
                                                           "recorded")
        entries.append({
            "id": sd["id"],
            "name": sd.get("name") or sd["id"],
            "file": rel,
            "url": sd["url"],
            "kind": kind,
            "stem": sd["id"],
            "conform": True,
            "durationSec": dur,
            "trimSec": sd.get("trimSec"),
            "loudness": loud,
            "tags": [kind],
            "user": bool(sd.get("user")),
            "createdAt": st.st_mtime if st else None,
            "mtime": st.st_mtime if st else None,
            "sizeKb": round(st.st_size / 1024, 1) if st else None,
        })

    for bucket, rel_dir, kind in LIB_BUCKETS:
        d = os.path.join(ROOT, rel_dir)
        if not os.path.isdir(d):
            continue
        for fn in sorted(os.listdir(d)):
            if not fn.endswith((".wav", ".mp3")):
                continue
            rel = f"{rel_dir}/{fn}"
            if rel in claimed:
                continue
            got = probe(rel)
            if got is None:
                continue
            st, dur, loud, conform = got
            entries.append({
                "id": f"{bucket}-{lib_slug(fn)}",
                "name": fn,
                "file": rel,
                "url": "/" + rel,
                "kind": kind,
                "stem": None,
                "conform": conform,
                "durationSec": dur,
                "trimSec": dur,
                "loudness": loud,
                "tags": [bucket],
                "user": False,
                "createdAt": st.st_mtime,
                "mtime": st.st_mtime,
                "sizeKb": round(st.st_size / 1024, 1),
            })

    manifest = {
        "v": 1,
        "builtAt": round(time.time(), 3),
        "default": old.get("default"),
        "retired": sorted(retired),
        "entries": entries,
    }
    save_manifest(manifest)
    return manifest


def get_manifest():
    if os.path.isfile(LIB_MANIFEST):
        return load_manifest()
    return scan_library()


def resolve_default(manifest, stems):
    """The manifest's pick when it's still playable, else 'bomb', else
    the first stem — a dead default must never produce a dead pad."""
    ids = [s["id"] for s in stems]
    if manifest.get("default") in ids:
        return manifest["default"]
    if "bomb" in ids:
        return "bomb"
    return ids[0] if ids else None


def promote_entry(entry_id, name=None):
    """Catalog entry -> lib-* stem.  Conforming WAVs promote in place;
    the rest get normalized into build/library/lib-<slug>.wav.  Returns
    the stem id; raises KeyError for an unknown entry, ValueError for a
    400 reply."""
    if not isinstance(entry_id, str) or not entry_id:
        raise ValueError("missing entry 'id'")
    manifest = scan_library()
    entry = next((e for e in manifest["entries"]
                  if e["id"] == entry_id), None)
    if entry is None:
        raise KeyError(entry_id)
    if entry.get("stem"):
        return entry["stem"]
    rel = entry["file"]
    if not rel.endswith(".wav"):
        raise ValueError("only WAV material promotes — transcode MP3s first")
    src = os.path.join(ROOT, rel)
    if name is None:
        base = os.path.basename(rel).rsplit(".", 1)[0]
        name = base if SAMPLE_NAME_RE.match(base) else lib_slug(base)
    if not isinstance(name, str) or not SAMPLE_NAME_RE.match(name):
        raise ValueError("bad stem name (want [A-Za-z0-9 _-], 1-48 chars)")

    sid = f"lib-{lib_slug(name)}"
    def_path = os.path.join(LIB_DIR, sid + ".json")
    if os.path.isfile(def_path):
        try:
            with open(def_path, "r", encoding="utf-8") as f:
                existing = json.load(f)
        except Exception:
            existing = {}
        if existing.get("url") == entry["url"]:
            return sid
        raise ValueError(f"stem id '{sid}' already backs "
                         f"{existing.get('url')!r} — pass another name")

    os.makedirs(LIB_DIR, exist_ok=True)
    if entry.get("conform"):
        url = entry["url"]
        dur = entry.get("durationSec") or 0
    else:
        wav_rel = f"build/library/{sid}.wav"
        normalize_wav(src, os.path.join(ROOT, wav_rel))
        url = "/" + wav_rel
        fmt = _wav_format(os.path.join(ROOT, wav_rel))
        dur = round(fmt[3] / 44100.0, 3) if fmt else 0
    stem_def = _lib_def(sid, name.strip(), url, dur, entry["kind"])
    _atomic_write(def_path,
                  (json.dumps(stem_def, indent=2) + "\n").encode("utf-8"))
    scan_library()
    m = load_manifest()
    if sid in set(m.get("retired") or []):
        m["retired"] = sorted(set(m["retired"]) - {sid})
        save_manifest(m)
    return sid


def set_default_stem(stem_id):
    m = get_manifest()
    if stem_id is None:
        m["default"] = None
    else:
        if stem_id not in {s["id"] for s in stem_defs()}:
            raise ValueError(f"'{stem_id}' is not a playable stem")
        m["default"] = stem_id
    save_manifest(m)
    return resolve_default(m, stem_defs())


def delete_stem(stem_id):
    """Remove a stem def by id (user or library) plus any audio file the
    def owns — a WAV sharing the def's basename in the same dir.  The id
    is retired so intake never resurrects it."""
    if not isinstance(stem_id, str):
        return False
    for d in (USER_DIR, LIB_DIR):
        if not os.path.isdir(d):
            continue
        for fn in sorted(os.listdir(d)):
            if not fn.endswith(".json"):
                continue
            p = os.path.join(d, fn)
            try:
                with open(p, "r", encoding="utf-8") as f:
                    doc = json.load(f)
            except Exception:
                continue
            if not isinstance(doc, dict) or doc.get("id") != stem_id:
                continue
            os.unlink(p)
            wav = os.path.join(os.path.dirname(p), fn[:-5] + ".wav")
            if os.path.isfile(wav):
                os.unlink(wav)
            m = load_manifest()
            m["retired"] = sorted(set(m.get("retired") or []) | {stem_id})
            save_manifest(m)
            scan_library()   # drop the stale stem pointer from its entry
            return True
    return False


def intake_sample(name, body):
    """HTTP intake: a conforming WAV -> build/library/<slug>.wav, which
    the rescan below turns into a lib-* stem.  Raises ValueError for a
    400 reply."""
    if not isinstance(name, str) or not SAMPLE_NAME_RE.match(name):
        raise ValueError("bad sample name (want [A-Za-z0-9 _-], 1-48 chars)")
    check_sample_wav(body)
    slug = lib_slug(user_slug(name))
    sid = f"lib-{slug}"
    wav_path = os.path.join(LIB_DIR, slug + ".wav")
    _atomic_write(wav_path, body)
    def_path = os.path.join(LIB_DIR, sid + ".json")
    if os.path.isfile(def_path):
        os.unlink(def_path)   # rewrite so trimSec tracks the new take
    m = load_manifest()
    retired = set(m.get("retired") or [])
    if sid in retired:
        retired.discard(sid)
        m["retired"] = sorted(retired)
        save_manifest(m)
    scan_library()
    return sid


def _sequence_mix(payload, tail_sec):
    """Mix the sequencer grid to stereo float buffers -> (L, R, SR).

    `steps`/`stepPitches` arrays of length 16 are a single bar repeated for
    every bar (the `/` sequencer's format). Any other length is treated as a
    flat `16 * bars` grid — the /pads looper's export format — with
    out-of-range steps read as silent.  `tail_sec` pads the render past the
    last bar so ring-out isn't cut.
    """
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
    total_sec = bars * bar_sec + tail_sec
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

        def grid_val(arr, s, i):
            if len(arr) == 16:
                return arr[s]
            return arr[i] if i < len(arr) else 0

        angle = (pan + 1.0) * (math.pi / 4.0)
        gl = gain * math.cos(angle)
        gr = gain * math.sin(angle)

        shifted_cache = {}
        for bar in range(bars):
            for s in range(16):
                i = bar * 16 + s
                vel = grid_val(steps, s, i)
                if not vel:
                    continue
                vel_scale = 1.0 if vel == 1 else (1.25 if vel == 2 else 0.65)
                sp = grid_val(step_pitches, s, i)
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
    return L, R, SR


def _s16_stereo(L, R):
    """Tanh-normalize stereo float buffers -> interleaved s16 PCM bytes."""
    peak = max(max((abs(x) for x in L), default=0.0), max((abs(x) for x in R), default=0.0), 1e-6)
    out_pcm = bytearray()
    for i in range(len(L)):
        l = math.tanh((L[i] / peak) * 1.4) / math.tanh(1.4) * 0.94
        r = math.tanh((R[i] / peak) * 1.4) / math.tanh(1.4) * 0.94
        out_pcm += struct.pack("<hh", int(l * 32767), int(r * 32767))
    return bytes(out_pcm)


def render_sequence_to_mp3(payload):
    """Render the current 16-step sequencer grid + stem mixer settings to a stereo WAV & 320k MP3."""
    bpm = float(payload.get("bpm", 116))
    L, R, SR = _sequence_mix(payload, tail_sec=1.8)
    out_pcm = _s16_stereo(L, R)
    N = len(L)

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


# NINJAM bridge (#49): kujamba (fart-app#98) owns the protocol; we spawn
# `kujamba join` against build/jam/loop.wav, tail its transcript for the
# server CONFIG (bpm/bpi), and atomically swap the loop to match the
# interval.  Credentials go to the child only through a 0600 config file —
# never argv, URLs, or any status surface.
JAM_DIR = os.path.join(ROOT, "build", "jam")
JAM_MAX_BYTES = 64 * 1024
# A hostile server can claim any bpm/bpi; cap the rendered interval so the
# pure-Python mix can't be talked into a multi-minute, multi-hundred-MB loop.
JAM_MAX_INTERVAL_SEC = 64.0
JAM_HOST_RE = re.compile(r"^[A-Za-z0-9.\-]{1,253}(:\d{1,5})?$")
JAM_USER_RE = re.compile(r"^[A-Za-z0-9._@-]{1,64}$")
JAM_CONFIG_RE = re.compile(r"S>C\s+0x02\s+CONFIG\b")
JAM_KV_RE = re.compile(r"([A-Za-z_]\w*)=(\S+)")

JAM_LOCK = threading.Lock()
JAM_RENDER_LOCK = threading.Lock()
JAM = {
    "proc": None, "tail": None, "state": "off",
    "host": None, "user": None, "seq": None,
    "bpm": None, "bpi": None, "bars": None,
    "intervalSec": None, "loopSec": None, "intervalClamped": False,
    "result": {}, "resultLine": None, "error": None,
}


# Resolved once: KUJAMBA is fixed at process start, and which() on every
# 1s status poll is wasted work.
_KUJAMBA_UNKNOWN = object()
_kujamba_cache = _KUJAMBA_UNKNOWN


def _kujamba_bin():
    global _kujamba_cache
    if _kujamba_cache is _KUJAMBA_UNKNOWN:
        env = os.environ.get("KUJAMBA")
        if env:
            p = env if os.path.isabs(env) else os.path.join(ROOT, env)
            _kujamba_cache = \
                p if os.path.isfile(p) and os.access(p, os.X_OK) else None
        else:
            _kujamba_cache = shutil.which("kujamba")
    return _kujamba_cache


def _toml_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _write_jam_config(path, host, user, password):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(f"host = {_toml_str(host)}\n"
                f"user = {_toml_str(user)}\n"
                f"pass = {_toml_str(password)}\n")


def _jam_clean_dir():
    """Remove the creds file, transcript, loop, and stray tmp renders.
    Callers hold JAM_LOCK so the wipe can't land on a fresh session's files."""
    if not os.path.isdir(JAM_DIR):
        return
    for fn in os.listdir(JAM_DIR):
        if fn in ("kujamba.toml", "transcript.log", "loop.wav") \
                or fn.startswith(".tmp-"):
            try:
                os.unlink(os.path.join(JAM_DIR, fn))
            except OSError:
                pass


def render_jam_loop(seq, bpm, bpi, out_path):
    """Render the sequencer state to a 44.1 kHz stereo s16 WAV exactly one
    NINJAM interval long (bpi beats at bpm).  The grid renders in whole
    4/4 bars, so bars = ceil(bpi / 4) always covers the interval: a longer
    mix is truncated down (a 6-beat interval renders 2 bars, cut at beat
    6) and a shorter one is zero-padded up.  bpm/bpi are clamped to sane
    ranges and the interval to JAM_MAX_INTERVAL_SEC so a hostile CONFIG
    can't force a minutes-long render."""
    bpm = max(40.0, min(240.0, float(bpm)))
    bpi = max(1, min(256, int(bpi)))
    bpi = max(1, min(bpi, int(JAM_MAX_INTERVAL_SEC * bpm / 60.0)))
    bars = max(1, math.ceil(bpi / 4))
    payload = {
        "bpm": bpm,
        "bars": bars,
        "swing": float((seq or {}).get("swing", 0.0)),
        "tracks": (seq or {}).get("tracks") or {},
    }
    L, R, SR = _sequence_mix(payload, tail_sec=0.0)
    want = max(1, int(round(bpi * 60.0 / bpm * SR)))
    if len(L) > want:
        L, R = L[:want], R[:want]
    elif len(L) < want:
        L += [0.0] * (want - len(L))
        R += [0.0] * (want - len(R))
    pcm = _s16_stereo(L, R)
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(out_path),
                               prefix=".tmp-", suffix=".wav")
    os.close(fd)
    try:
        with wave.open(tmp, "wb") as w:
            w.setnchannels(2)
            w.setsampwidth(2)
            w.setframerate(SR)
            w.writeframes(pcm)
        os.replace(tmp, out_path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise
    return round(want / SR, 3)


def _jam_render(proc, seq, bpm, bpi):
    if not seq:
        return
    with JAM_RENDER_LOCK:
        with JAM_LOCK:
            if JAM["proc"] is not proc:
                return
        try:
            sec = render_jam_loop(seq, bpm, bpi,
                                  os.path.join(JAM_DIR, "loop.wav"))
        except Exception as e:
            with JAM_LOCK:
                if JAM["proc"] is proc:
                    JAM["error"] = f"loop render failed: {e}"
            return
        with JAM_LOCK:
            if JAM["proc"] is not proc:
                try:
                    os.unlink(os.path.join(JAM_DIR, "loop.wav"))
                except OSError:
                    pass
                return
            JAM["loopSec"] = sec
            if JAM["error"] and JAM["error"].startswith("loop render"):
                JAM["error"] = None


def _jam_config(proc, bpm, bpi):
    if bpm <= 0 or bpi <= 0:
        return
    bpm = max(40.0, min(240.0, bpm))
    bpi = min(256, int(bpi))
    clamped = bpi * 60.0 / bpm > JAM_MAX_INTERVAL_SEC
    if clamped:
        bpi = max(1, int(JAM_MAX_INTERVAL_SEC * bpm / 60.0))
    with JAM_LOCK:
        if JAM["proc"] is not proc:
            return
        changed = JAM["bpm"] != bpm or JAM["bpi"] != bpi
        JAM["bpm"], JAM["bpi"] = bpm, bpi
        JAM["bars"] = max(1, math.ceil(bpi / 4))
        JAM["intervalSec"] = round(bpi * 60.0 / bpm, 3)
        JAM["intervalClamped"] = clamped
        JAM["state"] = "live"
        seq = JAM["seq"]
    if changed:
        _jam_render(proc, seq, bpm, bpi)


def _jam_line(proc, line, secret):
    line = line.strip()
    if not line:
        return
    if secret and secret in line:
        line = line.replace(secret, "***")
    if JAM_CONFIG_RE.search(line):
        bpm = re.search(r"\bbpm=(\d+(?:\.\d+)?)", line)
        bpi = re.search(r"\bbpi=(\d+)", line)
        if bpm and bpi:
            _jam_config(proc, float(bpm.group(1)), int(bpi.group(1)))
        return
    if "RESULT" in line:
        kv = {k: v for k, v in JAM_KV_RE.findall(line)
              if "pass" not in k.lower() and "secret" not in k.lower()}
        safe_line = JAM_KV_RE.sub(
            lambda m: m.group(1) + "=***"
            if "pass" in m.group(1).lower()
            or "secret" in m.group(1).lower()
            else m.group(0), line)
        with JAM_LOCK:
            if JAM["proc"] is proc:
                JAM["result"] = kv
                JAM["resultLine"] = safe_line[:200]


def _jam_tail(proc, log_path, secret):
    pos = 0
    buf = b""

    def drain():
        nonlocal pos, buf
        try:
            size = os.path.getsize(log_path)
        except OSError:
            size = 0
        if size < pos:
            pos, buf = 0, b""
        if size <= pos:
            return
        try:
            with open(log_path, "rb") as f:
                f.seek(pos)
                chunk = f.read(size - pos)
        except OSError:
            return
        pos += len(chunk)
        buf += chunk
        *lines, buf = buf.split(b"\n")
        for raw in lines:
            _jam_line(proc, raw.decode("utf-8", "replace"), secret)

    while True:
        drain()
        with JAM_LOCK:
            mine = JAM["proc"] is proc
        if not mine:
            return
        if proc.poll() is not None:
            time.sleep(0.1)
            drain()
            with JAM_LOCK:
                mine = JAM["proc"] is proc
                if mine:
                    JAM["proc"] = None
                    JAM["state"] = "error"
                    JAM["error"] = f"kujamba exited (rc {proc.returncode})"
                    _jam_clean_dir()
            return
        time.sleep(0.2)


def _jam_kill(proc):
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except Exception:
        proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except Exception:
            proc.kill()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            pass


def jam_status():
    with JAM_LOCK:
        st = {
            "ok": True,
            "state": JAM["state"],
            "host": JAM["host"],
            "user": JAM["user"],
            "bpm": JAM["bpm"],
            "bpi": JAM["bpi"],
            "bars": JAM["bars"],
            "intervalSec": JAM["intervalSec"],
            "intervalClamped": JAM["intervalClamped"],
            "loopSec": JAM["loopSec"],
            "result": dict(JAM["result"]),
            "resultLine": JAM["resultLine"],
            "error": JAM["error"],
        }
    st["loopUrl"] = "/build/jam/loop.wav" if os.path.isfile(
        os.path.join(JAM_DIR, "loop.wav")) else None
    st["kujamba"] = bool(_kujamba_bin())
    return st


def jam_join(host, user, password, seq):
    """-> (http code, response dict).  ValueError maps to a 400."""
    if not isinstance(host, str) or not JAM_HOST_RE.match(host):
        raise ValueError("bad host (want host[:port], [A-Za-z0-9.-])")
    if not isinstance(user, str) or not JAM_USER_RE.match(user):
        raise ValueError("bad user (want [A-Za-z0-9._@-], 1-64 chars)")
    if not isinstance(password, str) or len(password) > 128 \
            or any(ord(c) < 0x20 or ord(c) == 0x7f for c in password):
        raise ValueError(
            "bad pass (string, up to 128 chars, no control chars)")
    if seq is not None and not isinstance(seq, dict):
        raise ValueError("seq must be an object")
    binpath = _kujamba_bin()
    if not binpath:
        return 503, {"ok": False,
                     "error": "kujamba not found — install it or point the "
                              "KUJAMBA env var at the binary"}
    with JAM_LOCK:
        proc = JAM["proc"]
        if proc is not None and proc.poll() is None:
            return 409, {"ok": False,
                         "error": "a jam session is already live — "
                                  "POST {stop: true} first"}
        os.makedirs(JAM_DIR, exist_ok=True)
        _jam_clean_dir()
        cfg_rel = "build/jam/kujamba.toml"
        _write_jam_config(os.path.join(JAM_DIR, "kujamba.toml"),
                          host, user, password)
        argv = [binpath, "join",
                "--wav", "build/jam/loop.wav", "--wav-reload",
                "--pattern", "1", "--reconnect",
                "--transcript", "build/jam/transcript.log",
                "--config", cfg_rel]
        try:
            proc = subprocess.Popen(
                argv, cwd=ROOT,
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, start_new_session=True)
        except Exception as e:
            _jam_clean_dir()
            return 500, {"ok": False,
                         "error": f"could not spawn kujamba: {e}"}
        JAM.update({
            "proc": proc, "state": "starting", "host": host, "user": user,
            "seq": seq or {},
            "bpm": None, "bpi": None, "bars": None,
            "intervalSec": None, "loopSec": None, "intervalClamped": False,
            "result": {}, "resultLine": None, "error": None,
        })
        JAM["tail"] = threading.Thread(
            target=_jam_tail,
            args=(proc, os.path.join(JAM_DIR, "transcript.log"), password),
            daemon=True)
        JAM["tail"].start()
    res = jam_status()
    return 200, res


def jam_push_seq(seq):
    """Swap the loop source mid-session -> (http code, response dict)."""
    if not isinstance(seq, dict):
        raise ValueError("seq must be an object")
    with JAM_LOCK:
        proc = JAM["proc"]
        if proc is None or proc.poll() is not None:
            return 409, {"ok": False, "error": "no live jam session"}
        JAM["seq"] = seq
        bpm, bpi = JAM["bpm"], JAM["bpi"]
    if bpm and bpi:
        _jam_render(proc, seq, bpm, bpi)
    return 200, jam_status()


def jam_stop():
    # Kill inside the lock too: a join that lands mid-stop must not have
    # its fresh files wiped by this session's deferred clean, and the
    # child must be dead before the wipe so it can't rewrite them after.
    with JAM_LOCK:
        proc = JAM["proc"]
        JAM.update({
            "proc": None, "state": "off", "seq": None,
            "host": None, "user": None,
            "bpm": None, "bpi": None, "bars": None,
            "intervalSec": None, "loopSec": None, "intervalClamped": False,
            "result": {}, "resultLine": None, "error": None,
        })
        if proc is not None:
            _jam_kill(proc)
        _jam_clean_dir()


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
            stems = stem_defs()
            return self._send_json(200, {
                "stems": stems,
                "tracks": list_tracks(),
                "default": resolve_default(get_manifest(), stems),
            })
        if parsed.path == "/api/library":
            m = get_manifest()
            return self._send_json(200, {
                "ok": True,
                "v": m.get("v", 1),
                "builtAt": m.get("builtAt"),
                "default": resolve_default(m, stem_defs()),
                "defaultSet": m.get("default"),
                "entries": m["entries"],
            })
        if parsed.path == "/api/jam":
            return self._send_json(200, jam_status())
        if parsed.path == "/api/patches":
            return self._send_json(200, list_patches())
        if parsed.path == "/api/patch":
            qs = urllib.parse.parse_qs(parsed.query)
            name = qs.get("name", [""])[0]
            path = patch_path(name)
            if path is None:
                return self._send_json(400, {
                    "ok": False,
                    "error": "bad patch name (want [A-Za-z0-9 _+-], 1-64 chars)",
                })
            if not os.path.isfile(path):
                return self._send_json(404, {"ok": False, "error": f"no patch named '{name}'"})
            try:
                with open(path, "r", encoding="utf-8") as f:
                    return self._send_json(200, json.load(f))
            except Exception as e:
                return self._send_json(500, {"ok": False, "error": f"corrupt patch file: {e}"})
        # build/jam/ holds the 0600 creds file and the child transcript;
        # only the rendered loop may cross HTTP.  Normalize like
        # translate_path does so %XX/case/dot variants can't slip past.
        norm = posixpath.normpath(
            urllib.parse.unquote(parsed.path)).lower()
        if norm != "/build/jam/loop.wav" and \
                (norm == "/build/jam" or norm.startswith("/build/jam/")):
            return self.send_error(404)
        return super().do_GET()

    def _handle_upload_sample(self, parsed):
        length = int(self.headers.get("Content-Length", "0"))
        if length > SAMPLE_MAX_BYTES:
            return self._send_json(413, {
                "ok": False, "error": "sample body over the 10 MiB cap"})
        raw_body = self.rfile.read(length) if length > 0 else b""
        ctype = (self.headers.get("Content-Type") or "")
        ctype = ctype.split(";")[0].strip().lower()

        if ctype in USER_WAV_TYPES:
            name = urllib.parse.parse_qs(parsed.query).get("name", [""])[0]
            try:
                stem_def = save_user_sample(name, raw_body)
            except ValueError as e:
                return self._send_json(400, {"ok": False, "error": str(e)})
            return self._send_json(200, stem_def)

        try:
            payload = json.loads(raw_body.decode("utf-8"))
        except Exception:
            return self._send_json(400, {
                "ok": False,
                "error": "expected a WAV body (Content-Type: audio/wav) "
                         "or JSON {name, delete: true}"})
        if not isinstance(payload, dict) or payload.get("delete") is not True:
            return self._send_json(400, {
                "ok": False,
                "error": "POST raw WAV with Content-Type: audio/wav, or "
                         "JSON {\"name\": ..., \"delete\": true}"})
        sid = payload.get("id")
        if sid is not None:
            if not delete_stem(sid):
                return self._send_json(404, {
                    "ok": False, "error": f"no stem with id '{sid}'"})
            return self._send_json(200, {"ok": True, "deleted": sid})
        name = payload.get("name")
        if user_paths(name) is None:
            return self._send_json(400, {
                "ok": False,
                "error": "bad sample name (want [A-Za-z0-9 _-], 1-48 chars)"})
        if not delete_user_sample(name):
            return self._send_json(404, {
                "ok": False, "error": f"no sample named '{name}'"})
        return self._send_json(200, {"ok": True, "deleted": name})

    def _handle_jam(self, payload):
        if not isinstance(payload, dict):
            return self._send_json(400, {
                "ok": False, "error": "expected a JSON object"})
        if payload.get("stop") is True:
            jam_stop()
            return self._send_json(200, jam_status())
        try:
            if any(k in payload for k in ("host", "user", "pass")):
                code, res = jam_join(payload.get("host"), payload.get("user"),
                                     payload.get("pass"), payload.get("seq"))
                return self._send_json(code, res)
            if "seq" in payload:
                code, res = jam_push_seq(payload.get("seq"))
                return self._send_json(code, res)
        except ValueError as e:
            return self._send_json(400, {"ok": False, "error": str(e)})
        return self._send_json(400, {
            "ok": False,
            "error": "expected {host, user, pass[, seq]} to join, "
                     "{seq} to push a new loop, or {stop: true} to leave"})

    def _handle_library_intake(self, parsed):
        length = int(self.headers.get("Content-Length", "0"))
        if length > SAMPLE_MAX_BYTES:
            return self._send_json(413, {
                "ok": False, "error": "sample body over the 10 MiB cap"})
        raw_body = self.rfile.read(length) if length > 0 else b""
        ctype = (self.headers.get("Content-Type") or "")
        ctype = ctype.split(";")[0].strip().lower()
        if ctype not in USER_WAV_TYPES:
            return self._send_json(400, {
                "ok": False,
                "error": "expected a WAV body (Content-Type: audio/wav)"})
        name = urllib.parse.parse_qs(parsed.query).get("name", [""])[0]
        try:
            sid = intake_sample(name, raw_body)
        except ValueError as e:
            return self._send_json(400, {"ok": False, "error": str(e)})
        stem = next((s for s in stem_defs() if s["id"] == sid), None)
        return self._send_json(200, {"ok": True, "stem": stem})

    def do_POST(self):
        parsed = urllib.parse.urlparse(self.path)
        length = int(self.headers.get("Content-Length", "0"))
        if parsed.path == "/api/upload-sample":
            return self._handle_upload_sample(parsed)
        if parsed.path == "/api/library/intake":
            return self._handle_library_intake(parsed)
        if parsed.path == "/api/patch" and length > PATCH_MAX_BYTES:
            return self._send_json(400, {"ok": False, "error": "patch body over the 256 KiB cap"})
        if parsed.path == "/api/jam" and length > JAM_MAX_BYTES:
            return self._send_json(400, {"ok": False, "error": "jam body over the 64 KiB cap"})
        if parsed.path in ("/api/library/rescan",
                           "/api/library/promote", "/api/library/default") \
                and length > PATCH_MAX_BYTES:
            return self._send_json(400, {"ok": False, "error": "body over the 256 KiB cap"})
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

            if parsed.path == "/api/jam":
                return self._handle_jam(payload)

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

            if parsed.path == "/api/patch":
                name = payload.get("name")
                path = patch_path(name)
                if path is None:
                    return self._send_json(400, {
                        "ok": False,
                        "error": "bad patch name (want [A-Za-z0-9 _+-], 1-64 chars)",
                    })
                if payload.get("delete") is True:
                    if not os.path.isfile(path):
                        return self._send_json(404, {"ok": False, "error": f"no patch named '{name}'"})
                    os.unlink(path)
                    return self._send_json(200, {"ok": True, "deleted": name})
                patch = payload.get("patch")
                if not isinstance(patch, dict):
                    return self._send_json(400, {"ok": False, "error": "missing 'patch' object"})
                if patch.get("v") != 1:
                    return self._send_json(400, {
                        "ok": False,
                        "error": f"unsupported patch schema v={patch.get('v')!r} (want 1)",
                    })
                extra = sorted(set(patch) - PATCH_KEYS)
                if extra:
                    return self._send_json(400, {"ok": False, "error": f"unknown patch keys: {extra}"})
                for sect, kind in (("seq", dict), ("desk", dict), ("pads", list)):
                    if sect in patch and patch[sect] is not None \
                            and not isinstance(patch[sect], kind):
                        return self._send_json(400, {"ok": False, "error": f"patch.{sect} must be a {kind.__name__}"})
                save_patch(name, patch)
                return self._send_json(200, {"ok": True, "name": name, "patches": list_patches()})

            if parsed.path == "/api/library/rescan":
                m = scan_library()
                return self._send_json(200, {
                    "ok": True,
                    "entries": len(m["entries"]),
                    "default": resolve_default(m, stem_defs()),
                })

            if parsed.path == "/api/library/promote":
                try:
                    sid = promote_entry(payload.get("id"),
                                        payload.get("name"))
                except KeyError:
                    return self._send_json(404, {
                        "ok": False,
                        "error": f"no library entry '{payload.get('id')}'"})
                except ValueError as e:
                    return self._send_json(400, {"ok": False, "error": str(e)})
                stem = next((s for s in stem_defs() if s["id"] == sid), None)
                return self._send_json(200, {"ok": True, "stem": stem})

            if parsed.path == "/api/library/default":
                sid = payload.get("id")
                if sid is not None and not isinstance(sid, str):
                    return self._send_json(400, {
                        "ok": False, "error": "default 'id' must be a stem id or null"})
                try:
                    resolved = set_default_stem(sid)
                except ValueError as e:
                    return self._send_json(400, {"ok": False, "error": str(e)})
                return self._send_json(200, {"ok": True, "default": resolved})

            return self._send_json(404, {"ok": False, "error": "unknown endpoint"})
        except Exception as e:
            return self._send_json(500, {"ok": False, "error": str(e)})


class ReusableTCPServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True


# Shared whole-studio state: one localStorage object, brownnote.studio.v1,
# read by every page (they share an origin).  Sections: seq (the / page),
# desk (/mixer), pads (/pads), key (the #47 studio key: scale/root/q),
# seed (reserved for #46).  A missing section means "keep this page's
# defaults", so partial patches still load.
STUDIO_JS = r"""
const STUDIO_LS_KEY = 'brownnote.studio.v1';

const PADS_DEFAULT = [
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

function studioRead() {
  try {
    const o = JSON.parse(localStorage.getItem(STUDIO_LS_KEY) || 'null');
    if (o && typeof o === 'object' && !Array.isArray(o)) return o;
  } catch (e) {}
  return { v: 1 };
}

function studioWrite(obj) {
  obj.v = 1;
  try { localStorage.setItem(STUDIO_LS_KEY, JSON.stringify(obj)); } catch (e) {}
  return obj;
}

function studioMerge(slice) {
  const o = studioRead();
  Object.assign(o, slice);
  return studioWrite(o);
}

function studioPadsSerialize(pads) {
  return pads.map(p => ({ stem: p.stem, semi: p.semi, label: p.label }));
}

// Default desk slice (the /mixer page owns this live, but / needs it to
// write complete whole-studio patches before /mixer has ever run).
function studioDeskDefaults(stems) {
  const channels = {};
  for (const s of stems) {
    channels[s.id] = {
      gain: s.defaultGain, pan: s.defaultPan, pitch: s.defaultPitch,
      mute: false, solo: false, useDry: false,
    };
  }
  return { master: 0.9, channels };
}

// #53: the manifest's canonical fart owns pad 1 on a fresh kit; pads and
// loop events whose stem vanished fall back to it — never a dead pad.
function studioPadsBoot(stems, defId) {
  const pads = PADS_DEFAULT.map(p => Object.assign({}, p));
  const d = (stems || []).find(s => s.id === defId) || (stems || [])[0];
  if (d) {
    pads[0].stem = d.id;
    pads[0].semi = 0;
    pads[0].label = String(d.name || d.id).toUpperCase().slice(0, 14);
  }
  return pads;
}

function studioStemById(stems, defId, id) {
  return stems.find(s => s.id === id)
      || stems.find(s => s.id === defId)
      || stems[0] || null;
}

// ===== scale-quantize block start (#47) =====
// One global studio key in the 'key' slice of brownnote.studio.v1,
// shared by every page.  ROOT is a transpose offset, not a detected
// key — the stems are not tuned to a known pitch, so semitone 0 is each
// sample's native pitch and "in key" means relative to the samples.
const SCALES = {
  minor_penta: [0, 3, 5, 7, 10],
  major_penta: [0, 2, 4, 7, 9],
  blues:       [0, 3, 5, 6, 7, 10],
  major:       [0, 2, 4, 5, 7, 9, 11],
  minor:       [0, 2, 3, 5, 7, 8, 10],
  dorian:      [0, 2, 3, 5, 7, 9, 10],
  chromatic:   [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};
const SCALE_LABELS = {
  minor_penta: 'minor penta', major_penta: 'major penta', blues: 'blues',
  major: 'major', minor: 'minor', dorian: 'dorian', chromatic: 'chromatic',
};
// Percussion is never snapped: hat sits at +14 and kick is pitched down
// for timbre, not melody.
const PITCHED = ['wah', 'lead', 'bomb'];

let KEY = { scale: 'minor_penta', root: 0, q: false };

function studioKeyDefault() {
  return { scale: 'minor_penta', root: 0, q: false };
}

// Pull the 'key' slice out of brownnote.studio.v1 into KEY (normalized).
function studioKeyApply() {
  const k = studioRead().key;
  KEY = {
    scale: (k && SCALES[k.scale]) ? k.scale : 'minor_penta',
    root: (k && typeof k.root === 'number' && isFinite(k.root))
      ? Math.max(0, Math.min(11, Math.round(k.root))) : 0,
    q: !!(k && k.q),
  };
  return KEY;
}

// Write one or more key fields back to the shared object.
function studioSetKey(slice) {
  studioMerge({ key: Object.assign({}, KEY, slice) });
  return studioKeyApply();
}

// q(semi): subtract root, snap to the nearest tone of KEY.scale across
// octaves (ties snap down), add root back.
function q(semi) {
  const tones = SCALES[KEY.scale] || SCALES.chromatic;
  const x = semi - KEY.root;
  const oct = Math.floor(x / 12) * 12;
  let best = Infinity;
  for (const t of tones) {
    for (const o of [-12, 0, 12]) {
      const cand = t + oct + o;
      if (Math.abs(cand - x) < Math.abs(best - x) ||
          (Math.abs(cand - x) === Math.abs(best - x) && cand < best)) {
        best = cand;
      }
    }
  }
  return best + KEY.root;
}

// Play-time gate: Q off or a non-pitched stem passes the total through.
// Callers pass the TOTAL shift (stem pitch + step/pad offset) — bomb sits
// at -5 by default, so snapping the offset alone would drift it out of key.
function qStem(stemId, semi) {
  if (!KEY.q || !PITCHED.includes(stemId)) return semi;
  return q(semi);
}

function keyBadgeText() {
  const name = SCALE_LABELS[KEY.scale] || KEY.scale;
  const root = KEY.root >= 0 ? `+${KEY.root}` : `${KEY.root}`;
  return `KEY: ${name} ${root}${KEY.q ? ' · Q ON' : ' · Q off'}`;
}

function refreshKeyBadge() {
  const el = document.getElementById('keyBadge');
  if (el) el.textContent = keyBadgeText();
}

globalThis.q = q;
globalThis.SCALES = SCALES;
globalThis.PITCHED = PITCHED;
// ===== scale-quantize block end =====
"""


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
  .patch-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-bottom: 10px;
    padding: 7px 9px;
    background: #14110f;
    border: 1px solid var(--border);
    border-radius: 7px;
  }
  .patch-label {
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.08em;
    color: var(--accent-hi);
  }
  .patch-bar input[type="text"] { width: 170px; }
  .patch-bar select { min-width: 150px; }
  .patch-hint {
    font-size: 9.5px;
    color: var(--muted);
    margin-left: auto;
  }
  .gassy-opt {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.05em;
    color: var(--muted);
    cursor: pointer;
    user-select: none;
  }
  .gassy-opt input { accent-color: var(--accent); margin: 0; }

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
  .step-cell.q-snap {
    box-shadow: inset 0 -3px 0 var(--accent-hi), 0 0 10px rgba(255,179,71,0.35);
  }
  #qBtn.q-on {
    background: var(--accent-hi);
    color: #000;
    border-color: #fcd34d;
    font-weight: 900;
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
    <div class="ctrl-pill" title="Studio key — Q snaps wah/lead/bomb to the scale at play time; kick/snare/hat are never quantized">
      <label for="scaleSelect">KEY</label>
      <select id="scaleSelect" onchange="setKeyScale(this.value)">
        <option value="minor_penta">Minor Penta</option>
        <option value="major_penta">Major Penta</option>
        <option value="blues">Blues</option>
        <option value="major">Major</option>
        <option value="minor">Minor</option>
        <option value="dorian">Dorian</option>
        <option value="chromatic">Chromatic</option>
      </select>
      <label for="rootSelect">ROOT</label>
      <select id="rootSelect" onchange="setKeyRoot(this.value)">
        <option value="0">+0 st</option>
        <option value="1">+1 st</option>
        <option value="2">+2 st</option>
        <option value="3">+3 st</option>
        <option value="4">+4 st</option>
        <option value="5">+5 st</option>
        <option value="6">+6 st</option>
        <option value="7">+7 st</option>
        <option value="8">+8 st</option>
        <option value="9">+9 st</option>
        <option value="10">+10 st</option>
        <option value="11">+11 st</option>
      </select>
      <button id="qBtn" class="mini-btn" onclick="toggleQuantize()" title="Quantize pitched stems to the studio key">Q</button>
    </div>
    <div class="ctrl-pill">
      <label for="barsSelect">Export Bars</label>
      <select id="barsSelect" onchange="persistSeq()">
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
    <a href="/jam">JAM</a>
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
        <button class="preset-btn primary" onclick="randomizePatch()" title="Roll a whole patch — steps, pitches, mixer, BPM & swing — from a shown, replayable seed">💨 I'M FEELING GASSY</button>
        <label class="gassy-opt" title="SAFE keeps the kick & snare steps; CHAOS re-rolls every stem"><input type="checkbox" id="gassyChaos" />CHAOS</label>
        <label class="gassy-opt" title="Auto-save each roll as a 'Gassy N' patch"><input type="checkbox" id="gassySave" checked />SAVE</label>
        <button class="preset-btn" onclick="clearPattern()">Clear</button>
      </div>
    </div>

    <div class="patch-bar">
      <span class="patch-label">PATCHES</span>
      <input id="patchName" type="text" maxlength="64" placeholder="name this jam…" />
      <button class="preset-btn primary" onclick="savePatch()">💾 Save</button>
      <select id="patchSelect"></select>
      <button class="preset-btn" onclick="loadPatch()">⤵ Load</button>
      <button class="preset-btn" onclick="deletePatch()">🗑 Del</button>
      <span class="patch-hint">the studio autosaves to this browser, so a refresh won't wipe the jam</span>
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
""" + STUDIO_JS + r"""
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
  persistSeq();
}

function toggleMute(id) {
  trackState[id].mute = !trackState[id].mute;
  document.getElementById(`btn-mute-${id}`).classList.toggle('mute-on', trackState[id].mute);
  persistSeq();
}
function toggleSolo(id) {
  trackState[id].solo = !trackState[id].solo;
  document.getElementById(`btn-solo-${id}`).classList.toggle('solo-on', trackState[id].solo);
  persistSeq();
}
function toggleDry(id) {
  trackState[id].useDry = !trackState[id].useDry;
  document.getElementById(`btn-dry-${id}`).classList.toggle('dry-on', trackState[id].useDry);
  persistSeq();
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
  persistSeq();
  if (st.steps[s] > 0 && !isPlaying) {
    playStemSample(id, st.stepPitches[s], st.steps[s] === 2 ? 1.25 : 1.0);
  }
}

function refreshTrackCells(id) {
  const st = trackState[id];
  const pitched = KEY.q && PITCHED.includes(id);
  for (let s = 0; s < 16; s++) {
    const cell = document.getElementById(`cell-${id}-${s}`);
    if (!cell) continue;
    cell.classList.remove('on-1', 'on-2', 'q-snap');
    const v = st.steps[s];
    const sp = st.stepPitches[s];
    let semiTxt = sp !== 0 ? (sp > 0 ? `+${sp}` : `${sp}`) : '';
    if (v > 0 && pitched) {
      const raw = (st.pitch || 0) + sp;
      const snapped = q(raw);
      if (snapped !== raw) {
        cell.classList.add('q-snap');
        semiTxt = `${snapped >= 0 ? '+' : ''}${snapped}◆`;
      }
    }
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
  const totalSemi = qStem(id, (st.pitch || 0) + (stepSemi || 0));
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
  persistSeq();
}

function updateSwing(val) {
  swing = parseInt(val, 10) / 100;
  document.getElementById('swingVal').textContent = `${val}%`;
  persistSeq();
}

// ---- Studio key UI (the 'key' slice is owned here, shared everywhere) --

function syncKeyUI() {
  const sel = document.getElementById('scaleSelect');
  if (sel) sel.value = KEY.scale;
  const rs = document.getElementById('rootSelect');
  if (rs) rs.value = String(KEY.root);
  const qb = document.getElementById('qBtn');
  if (qb) qb.classList.toggle('q-on', KEY.q);
}

function applyKeyChange() {
  syncKeyUI();
  for (const s of STEMS) refreshTrackCells(s.id);
}

function setKeyScale(v) { studioSetKey({ scale: v }); applyKeyChange(); }
function setKeyRoot(v) { studioSetKey({ root: parseInt(v, 10) || 0 }); applyKeyChange(); }
function toggleQuantize() { studioSetKey({ q: !KEY.q }); applyKeyChange(); }

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
  persistSeq();
}

function clearPattern() {
  for (const stem of STEMS) {
    trackState[stem.id].steps = new Array(16).fill(0);
    trackState[stem.id].stepPitches = new Array(16).fill(0);
    refreshTrackCells(stem.id);
  }
  persistSeq();
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
  persistSeq();
}

// ---- "I'M FEELING GASSY" full-patch randomizer (#46) --------------------
// mulberry32: tiny seeded PRNG.  Every draw in a roll goes through it —
// Math.random() can't be seeded, so ?seed= replay needs this.  Same seed
// + same build = identical patch.
function mulberry32(seed) {
  let a = seed >>> 0;
  return function () {
    a |= 0; a = (a + 0x6D2B79F5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const rint  = (rng, lo, hi) => lo + Math.floor(rng() * (hi - lo + 1));
const rspan = (rng, lo, hi) => lo + rng() * (hi - lo);
const rpick = (rng, arr) => arr[Math.floor(rng() * arr.length)];

// Per-stem taste table (#46 clarifications): sane ranges, not uniform
// noise.  pitch is an inclusive semitone range, or 'cycle' = pick from
// PITCH_CYCLE; pan is a ±span (kick stays center); density is per-step.
const GASSY_TASTE = {
  kick:  { pitch: [-7, 2],   gain: [0.80, 1.00], pan: 0,   density: 0.25 },
  snare: { pitch: [-3, 3],   gain: [0.70, 0.95], pan: 0.2, density: 0.15 },
  hat:   { pitch: [10, 18],  gain: [0.30, 0.55], pan: 0.5, density: 0.50 },
  wah:   { pitch: 'cycle',   gain: [0.60, 0.90], pan: 0.6, density: 0.20 },
  lead:  { pitch: 'cycle',   gain: [0.60, 0.90], pan: 0.6, density: 0.20 },
  bomb:  { pitch: [-12, 0],  gain: [0.75, 0.95], pan: 0.2, density: 0.08 },
};
const GASSY_BPM = [80, 150];
const GASSY_SWING = [0, 0.5];
const GASSY_DRY_P = 0.2;      // per-stem dry/pedaled flip chance
const GASSY_ACCENT_P = 0.25;  // on-step -> accent chance (as randomizePattern)
const GASSY_SAFE_STEMS = new Set(['kick', 'snare']);  // SAFE keeps their steps

// #47 hook: scale quantize is specced to expose window.q(semi) and store
// {scale, root, q:true} in the shared `key` slice, snapping only pitched
// stems.  Roll totals (base + step offset) through it while Q is on;
// identity until that work lands.
const GASSY_Q_PITCHED = new Set(['wah', 'lead', 'bomb']);
function qRollPitch(stemId, semi) {
  const k = studioRead().key;
  if (k && k.q && GASSY_Q_PITCHED.has(stemId) && typeof window.q === 'function') {
    return window.q(semi);
  }
  return semi;
}

// Deterministic whole-roll draw — no DOM, no Math.random().  Draw order is
// fixed, so seed alone decides the patch.
function rollPatch(seed) {
  const rng = mulberry32(seed);
  const tracks = {};
  for (const stem of STEMS) {
    const t = GASSY_TASTE[stem.id];
    if (!t) continue;
    const steps = new Array(16).fill(0);
    for (let s = 0; s < 16; s++) {
      if (rng() < t.density) steps[s] = rng() < GASSY_ACCENT_P ? 2 : 1;
    }
    let pitch = t.pitch === 'cycle' ? rpick(rng, PITCH_CYCLE)
                                    : rint(rng, t.pitch[0], t.pitch[1]);
    pitch = qRollPitch(stem.id, pitch);
    const stepPitches = new Array(16).fill(0);
    for (let s = 0; s < 16; s++) {
      if (!steps[s]) continue;
      const raw = rpick(rng, PITCH_CYCLE.slice(0, 5));
      stepPitches[s] = qRollPitch(stem.id, pitch + raw) - pitch;
    }
    tracks[stem.id] = {
      steps, stepPitches, pitch,
      gain: Math.round(rspan(rng, t.gain[0], t.gain[1]) * 100) / 100,
      pan: t.pan === 0 ? 0 : Math.round(rspan(rng, -t.pan, t.pan) * 100) / 100,
      useDry: rng() < GASSY_DRY_P,
    };
  }
  return {
    bpm: rint(rng, GASSY_BPM[0], GASSY_BPM[1]),
    swing: Math.round(rspan(rng, GASSY_SWING[0], GASSY_SWING[1]) * 100) / 100,
    tracks,
  };
}

// Apply a roll to the live studio.  keepGroove (SAFE) restores the current
// kick & snare steps afterwards; ?seed= replay passes false so the URL
// alone decides the whole patch.
function applyGassyRoll(seed, keepGroove) {
  const roll = rollPatch(seed);
  const kept = {};
  if (keepGroove) {
    for (const id of GASSY_SAFE_STEMS) {
      if (trackState[id]) kept[id] = [...trackState[id].steps];
    }
  }
  bpm = roll.bpm;
  swing = roll.swing;
  document.getElementById('bpmInput').value = bpm;
  document.getElementById('bpmVal').textContent = bpm;
  const pct = Math.round(swing * 100);
  document.getElementById('swingInput').value = pct;
  document.getElementById('swingVal').textContent = `${pct}%`;
  for (const stem of STEMS) {
    const t = roll.tracks[stem.id];
    if (!t) continue;
    const st = trackState[stem.id];
    st.gain = t.gain;
    st.pan = t.pan;
    st.pitch = t.pitch;
    st.useDry = t.useDry;
    st.stepPitches = t.stepPitches;
    st.steps = kept[stem.id] || t.steps;
  }
  buildTracksUI();
  studioMerge({ seed });
  persistSeq();
}

async function randomizePatch() {
  if (!STEMS.length) return;
  const seed = Math.floor(Math.random() * 4294967296);
  const chaos = document.getElementById('gassyChaos').checked;
  applyGassyRoll(seed, !chaos);
  let msg = `💨 Gassy roll (${chaos ? 'CHAOS' : 'SAFE'}) — seed ${seed}. ` +
            `Replay the full draw at /?seed=${seed}.`;
  if (document.getElementById('gassySave').checked) {
    msg += ' ' + await autoSaveGassy();
  }
  setStatus(msg);
}

// Auto-save each roll as "Gassy N" (#46; '#' is outside the patch-name
// charset).  The rolled seed rides along in the patch's seed field.
async function autoSaveGassy() {
  try {
    const res = await fetch('/api/patches');
    const list = await res.json();
    let n = 1;
    for (const p of (Array.isArray(list) ? list : [])) {
      const m = /^Gassy (\d+)$/.exec((p && p.name) || '');
      if (m) n = Math.max(n, parseInt(m[1], 10) + 1);
    }
    const name = `Gassy ${n}`;
    const save = await fetch('/api/patch', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, patch: buildPatch(name) }),
    });
    const data = await save.json();
    if (!save.ok || !data.ok) return `⚠ auto-save failed: ${data.error || save.status}`;
    refreshPatchList(name);
    return `Auto-saved as "${name}".`;
  } catch (e) {
    return `⚠ auto-save failed: ${e.message}`;
  }
}

// ---- PATCHES: whole-studio snapshots (schema v1) -----------------------
// seq slice = this page's share of brownnote.studio.v1; a saved patch also
// carries the desk/pads/key/seed sections so /mixer and /pads hydrate too.

function seqSlice() {
  const tracks = {};
  for (const s of STEMS) {
    const st = trackState[s.id];
    tracks[s.id] = {
      gain: st.gain, pan: st.pan, pitch: st.pitch,
      mute: !!st.mute, solo: !!st.solo, useDry: !!st.useDry,
      steps: [...st.steps], stepPitches: [...st.stepPitches],
    };
  }
  return {
    bpm, swing,
    bars: parseInt(document.getElementById('barsSelect').value, 10) || 4,
    tracks,
  };
}

function persistSeq() {
  if (!STEMS.length) return;
  studioMerge({ seq: seqSlice() });
}

function applySeqSlice(seq) {
  if (!seq || typeof seq !== 'object') return;
  if (typeof seq.bpm === 'number') {
    bpm = Math.round(seq.bpm);
    document.getElementById('bpmInput').value = bpm;
    document.getElementById('bpmVal').textContent = bpm;
  }
  if (typeof seq.swing === 'number') {
    swing = seq.swing;
    const pct = Math.round(swing * 100);
    document.getElementById('swingInput').value = pct;
    document.getElementById('swingVal').textContent = `${pct}%`;
  }
  if (seq.bars) {
    document.getElementById('barsSelect').value = String(seq.bars);
  }
  const tr = seq.tracks || {};
  for (const stem of STEMS) {
    const pt = tr[stem.id];
    if (!pt || typeof pt !== 'object') continue;   // unknown ids ignored
    const st = trackState[stem.id];
    for (const k of ['gain', 'pan', 'pitch']) {
      if (typeof pt[k] === 'number') st[k] = pt[k];
    }
    for (const k of ['mute', 'solo', 'useDry']) {
      if (k in pt) st[k] = !!pt[k];
    }
    if (Array.isArray(pt.steps)) {
      st.steps = Array.from({ length: 16 }, (_, i) => pt.steps[i] | 0);
    }
    if (Array.isArray(pt.stepPitches)) {
      st.stepPitches = Array.from({ length: 16 }, (_, i) => pt.stepPitches[i] | 0);
    }
  }
  buildTracksUI();
}

function buildPatch(name) {
  const base = studioRead();
  return {
    v: 1,
    name,
    seq: seqSlice(),
    desk: base.desk || studioDeskDefaults(STEMS),
    pads: base.pads || studioPadsSerialize(PADS_DEFAULT),
    key: base.key ?? null,
    seed: base.seed ?? null,
  };
}

function setStatus(msg) {
  document.getElementById('statusText').textContent = msg;
}

async function refreshPatchList(selectName) {
  try {
    const res = await fetch('/api/patches');
    const list = await res.json();
    const sel = document.getElementById('patchSelect');
    sel.innerHTML = '';
    if (!Array.isArray(list) || !list.length) {
      sel.innerHTML = '<option value="">(no saved patches)</option>';
      return;
    }
    for (const p of list) {
      const opt = document.createElement('option');
      opt.value = p.name;
      opt.textContent = p.name;
      sel.appendChild(opt);
    }
    if (selectName) sel.value = selectName;
  } catch (e) {}
}

async function savePatch() {
  const name = document.getElementById('patchName').value.trim();
  if (!name) { setStatus('⚠ Name the patch first — "Sewer Bomb + Flanger" deserves a name.'); return; }
  const res = await fetch('/api/patch', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, patch: buildPatch(name) }),
  });
  const data = await res.json();
  if (!res.ok || !data.ok) {
    setStatus(`❌ Patch save failed: ${data.error || res.status}`);
    return;
  }
  await refreshPatchList(name);
  setStatus(`✓ Saved patch "${name}" — seq, desk, pads, key & seed all in the bag.`);
}

async function loadPatch() {
  const name = document.getElementById('patchSelect').value;
  if (!name) { setStatus('⚠ Pick a patch to load.'); return; }
  const res = await fetch('/api/patch?name=' + encodeURIComponent(name));
  const data = await res.json();
  if (!res.ok) {
    setStatus(`❌ Patch load failed: ${data.error || res.status}`);
    return;
  }
  if (data.v !== 1) {
    setStatus(`❌ Patch "${name}" is schema v${data.v} — this studio only reads v1.`);
    return;
  }
  studioWrite(data);          // whole object in; other pages hydrate on load
  studioKeyApply();
  syncKeyUI();
  applySeqSlice(data.seq);
  document.getElementById('patchName').value = name;
  setStatus(`✓ Loaded "${name}". /mixer & /pads pick it up on their next load.`);
}

async function deletePatch() {
  const name = document.getElementById('patchSelect').value;
  if (!name) { setStatus('⚠ Pick a patch to delete.'); return; }
  const res = await fetch('/api/patch', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, delete: true }),
  });
  const data = await res.json();
  if (!res.ok || !data.ok) {
    setStatus(`❌ Patch delete failed: ${data.error || res.status}`);
    return;
  }
  await refreshPatchList();
  setStatus(`🗑 Deleted patch "${name}".`);
}

// Server-side render hears what the browser hears: with Q on, pitched
// tracks post their SNAPPED total (pitch folded into stepPitches so the
// server's semi = pitch + stepPitches[s] is already in key).  With Q off
// the payload is byte-identical to the raw trackState.
function renderTracks() {
  if (!KEY.q) return trackState;
  const out = {};
  for (const s of STEMS) {
    const st = trackState[s.id];
    const t = {
      gain: st.gain, pan: st.pan, pitch: st.pitch,
      mute: !!st.mute, solo: !!st.solo, useDry: !!st.useDry,
      steps: [...st.steps], stepPitches: [...st.stepPitches],
    };
    if (PITCHED.includes(s.id)) {
      const base = st.pitch || 0;
      t.pitch = 0;
      t.stepPitches = st.stepPitches.map(sp => q(base + (sp || 0)));
    }
    out[s.id] = t;
  }
  return out;
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
    body: JSON.stringify({ bpm, bars, swing, tracks: renderTracks() })
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
    // user-recorded stems play on DESK/PADS/CRATES but are not seq lanes
    STEMS = data.stems.filter(s => !s.user);
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
    const stored = studioRead();
    studioKeyApply();
    syncKeyUI();
    if (stored.seq && typeof stored.seq === 'object') {
      applySeqSlice(stored.seq);            // autosaved jam survives refresh
    } else {
      buildTracksUI();
      loadPatternPreset('funk');
    }
    // Seed sections this page doesn't own so the first Save already writes
    // a complete whole-studio snapshot.
    const seed = {};
    if (!stored.desk) seed.desk = studioDeskDefaults(STEMS);
    if (!stored.pads) seed.pads = studioPadsSerialize(studioPadsBoot(data.stems, data.default));
    if (!('key' in stored)) seed.key = studioKeyDefault();
    if (!('seed' in stored)) seed.seed = null;
    if (Object.keys(seed).length) studioMerge(seed);
    refreshPatchList();
    // #46: /?seed=N replays a gassy roll.  Full draw (not SAFE) so the URL
    // alone decides the whole patch — same seed, same build, same patch.
    const seedParam = new URLSearchParams(location.search).get('seed');
    if (seedParam !== null && /^-?\d{1,10}$/.test(seedParam)) {
      applyGassyRoll(parseInt(seedParam, 10), false);
      setStatus(`💨 Replayed gassy seed ${seedParam} — same seed, same patch.`);
    }
  }
  renderJukebox(data.tracks || []);
}

window.addEventListener('storage', (ev) => {
  if (ev.key !== STUDIO_LS_KEY) return;
  studioKeyApply();
  syncKeyUI();
  const o = studioRead();
  if (o.seq && typeof o.seq === 'object') applySeqSlice(o.seq);
  else for (const s of STEMS) refreshTrackCells(s.id);
});

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
  .key-badge {
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.06em;
    color: var(--accent-hi);
    border: 1px solid var(--accent);
    background: var(--panel-2);
    border-radius: 6px;
    padding: 6px 10px;
    white-space: nowrap;
    align-self: center;
  }
  .midi-badge {
    align-self: center;
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.06em;
    padding: 5px 9px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--panel-2);
    color: var(--muted);
    white-space: nowrap;
  }
  .midi-badge.on { color: var(--green); border-color: var(--green); }
  .midi-badge.err { color: var(--yellow); }
"""


# Shared Web MIDI input helper for /pads and /mixer. Pure client-side and
# feature-detected: listens on every channel, hot-plugs via statechange,
# and degrades to a nav badge reason on unsupported browsers or insecure
# origins (e.g. STUDIO_HOST=0.0.0.0 opened over a LAN IP).
MIDI_JS = r"""
// ---- Web MIDI input (shared by /pads and /mixer) ----
function midiBadge(state, detail) {
  const el = document.getElementById('midiBadge');
  if (!el) return;
  el.className = 'midi-badge ' + state;
  if (state === 'on')       el.textContent = `MIDI: ${detail}`;
  else if (state === 'off') el.textContent = 'MIDI: none';
  else                      el.textContent = `MIDI: unavailable (${detail})`;
}

function midiDispatch(ev, hooks) {
  const d = ev.data;
  if (!d || d.length < 3) return;
  const type = d[0] & 0xf0;                  // all channels: low nibble ignored
  if (type === 0x90) {                       // noteon; velocity 0 is noteoff
    if (d[2] > 0 && hooks.noteOn) hooks.noteOn(d[1], d[2]);
  } else if (type === 0xb0 && hooks.cc) {    // control change
    hooks.cc(d[1], d[2]);
  }
  // noteoff (0x80) and everything else ignored: one-shots, no clock
}

function midiWireInputs(access, hooks) {
  const names = [];
  for (const input of access.inputs.values()) {
    names.push(input.name || 'MIDI device');
    input.onmidimessage = (ev) => midiDispatch(ev, hooks);
  }
  midiBadge(names.length ? 'on' : 'off',
            names.length === 1 ? names[0] : `${names.length} devices`);
}

async function initMidi(hooks) {
  if (!window.isSecureContext) {
    midiBadge('err', 'insecure origin — open via localhost or 127.0.0.1');
    return;
  }
  if (!navigator.requestMIDIAccess) {
    midiBadge('err', 'unsupported browser — needs Chrome/Edge or Firefox 108+');
    return;
  }
  try {
    const access = await navigator.requestMIDIAccess();
    midiWireInputs(access, hooks);
    access.onstatechange = () => midiWireInputs(access, hooks);
  } catch (err) {
    midiBadge('err', err && err.name === 'SecurityError'
      ? 'permission denied'
      : (err && err.message) || 'request failed');
  }
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
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
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
    .strips { grid-template-columns: repeat(auto-fill, minmax(110px, 1fr)); }
    .master { width: 100%; }
  }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE DESK // STEM CONSOLE</h1>
      <p>Strips over the six built-in stems + your recorded samples · VU per channel · loop or tap</p>
    </div>
  </div>
  <span class="key-badge" id="keyBadge" title="Studio key is set on the SEQ+MIX page">KEY: —</span>
  <nav class="page-nav">
    <a href="/">SEQ+MIX</a>
    <a href="/mixer" class="active">DESK</a>
    <a href="/pads">PADS</a>
    <a href="/library">CRATES</a>
    <a href="/jam">JAM</a>
    <span class="midi-badge" id="midiBadge">MIDI: …</span>
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
""" + STUDIO_JS + MIDI_JS + r"""
let STEMS = [];
let DEFAULT_STEM = null;
let ch = {};
let audioCtx = null;
let masterGain = null, masterAnalyser = null;
let masterLevel = 0.9;
const rawBufs = {}, trimBufs = {};

// desk slice of brownnote.studio.v1 — never serialize the WebAudio nodes
// (g, p, a, loopSrc) that live alongside these fields in ch[].
function deskSlice() {
  const channels = {};
  for (const s of STEMS) {
    const st = ch[s.id];
    channels[s.id] = {
      gain: st.gain, pan: st.pan, pitch: st.pitch,
      mute: !!st.mute, solo: !!st.solo, useDry: !!st.useDry,
    };
  }
  return { master: masterLevel, channels };
}

function persistDesk() {
  if (!STEMS.length) return;
  studioMerge({ desk: deskSlice() });
}

function applyDeskSlice(desk) {
  if (!desk || typeof desk !== 'object') return;
  if (typeof desk.master === 'number') {
    masterLevel = desk.master;
    document.getElementById('masterVol').value = Math.round(masterLevel * 100);
    document.getElementById('masterVolLbl').textContent = `${Math.round(masterLevel * 100)}%`;
    if (masterGain) masterGain.gain.value = masterLevel;
  }
  const chans = desk.channels || {};
  for (const s of STEMS) {
    const c = chans[s.id];
    if (!c || typeof c !== 'object') continue;   // unknown ids ignored
    const st = ch[s.id];
    for (const k of ['gain', 'pan', 'pitch']) {
      if (typeof c[k] === 'number') st[k] = c[k];
    }
    for (const k of ['mute', 'solo', 'useDry']) {
      if (k in c) st[k] = !!c[k];
    }
  }
  if (document.getElementById('strips').children.length) {
    buildStrips();
    for (const s of STEMS) {
      const st = ch[s.id];
      if (st.loopSrc) {
        const btn = document.getElementById(`loop-${s.id}`);
        if (btn) { btn.classList.add('on'); btn.textContent = 'LOOPING'; }
      }
      if (audioCtx) {
        applyGain(s.id);
        if (st.p) st.p.pan.value = st.pan;
      }
    }
  }
}

function ensureAudio() {
  if (!audioCtx) {
    const AC = window.AudioContext || window.webkitAudioContext;
    audioCtx = new AC();
    masterGain = audioCtx.createGain();
    masterGain.gain.value = masterLevel;
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
    src.playbackRate.value = Math.pow(2, qStem(id, st.pitch || 0) / 12);
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
    document.getElementById(`gain-${id}`).value = Math.round(val * 100);
    applyGain(id);
  }
  if (key === 'pan') {
    document.getElementById(`plbl-${id}`).textContent =
      Math.abs(val) < 0.05 ? 'C' : (val < 0 ? `${Math.round(-val*100)}L` : `${Math.round(val*100)}R`);
    document.getElementById(`pan-${id}`).value = Math.round(val * 100);
    if (ch[id].p) ch[id].p.pan.value = val;
  }
  if (key === 'pitch') {
    document.getElementById(`tlbl-${id}`).textContent = `${val >= 0 ? '+'+val : val}st`;
  }
  persistDesk();
}

function toggleFlag(id, flag, cls) {
  ch[id][flag] = !ch[id][flag];
  document.getElementById(`btn-${flag}-${id}`).classList.toggle(cls, ch[id][flag]);
  if (flag === 'mute' || flag === 'solo') STEMS.forEach(s => applyGain(s.id));
  if (flag === 'useDry') { delete trimBufs[id + '|d']; delete trimBufs[id + '|w']; tap(id); }
  persistDesk();
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
  masterLevel = v;
  if (masterGain) masterGain.gain.value = v;
  document.getElementById('masterVol').value = Math.round(v * 100);
  document.getElementById('masterVolLbl').textContent = `${Math.round(v*100)}%`;
  persistDesk();
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
      <div class="strip-name">${stem.id === DEFAULT_STEM ? '★ ' : ''}${stem.name}</div>
      <div class="strip-sub">"${stem.phrase}"<br>${stem.effect}</div>
      <div class="strip-mid">
        <input class="fader" id="gain-${stem.id}" type="range" orient="vertical" min="0" max="140" value="${Math.round(st.gain*100)}"
          oninput="setParam('${stem.id}','gain',this.value/100)" />
        <canvas class="vu" id="vu-${stem.id}" width="12" height="128"></canvas>
      </div>
      <div class="strip-slider"><span>VOL <b id="vlbl-${stem.id}">${Math.round(st.gain*100)}%</b></span></div>
      <div class="strip-slider">
        <span>PAN <b id="plbl-${stem.id}">${panLbl}</b></span>
        <input id="pan-${stem.id}" type="range" min="-100" max="100" value="${Math.round(st.pan*100)}"
          oninput="setParam('${stem.id}','pan',this.value/100)" />
      </div>
      <div class="strip-slider">
        <span>PITCH <b id="tlbl-${stem.id}">${st.pitch >= 0 ? '+'+st.pitch : st.pitch}st</b></span>
        <input type="range" min="-12" max="18" value="${st.pitch}"
          oninput="setParam('${stem.id}','pitch',parseInt(this.value,10))" />
      </div>
      <div class="strip-btns">
        <button class="mini-btn ${st.useDry ? 'dry-on' : ''}" id="btn-useDry-${stem.id}" onclick="toggleFlag('${stem.id}','useDry','dry-on')">DRY</button>
        <button class="mini-btn ${st.mute ? 'mute-on' : ''}" id="btn-mute-${stem.id}" onclick="toggleFlag('${stem.id}','mute','mute-on')">M</button>
        <button class="mini-btn ${st.solo ? 'solo-on' : ''}" id="btn-solo-${stem.id}" onclick="toggleFlag('${stem.id}','solo','solo-on')">S</button>
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
  DEFAULT_STEM = data.default || null;
  for (const s of STEMS) {
    ch[s.id] = {
      gain: s.defaultGain, pan: s.defaultPan, pitch: s.defaultPitch,
      mute: false, solo: false, useDry: false,
      g: null, p: null, a: null, loopSrc: null,
    };
  }
  const stored = studioRead();
  studioKeyApply();
  refreshKeyBadge();
  applyDeskSlice(stored.desk);
  buildStrips();
  if (!stored.desk) persistDesk();   // seed the shared snapshot once
  initMidi({
    cc: (num, val) => {
      const st = document.getElementById('statusText');
      const g = num - 20;                         // CC20-25: channel 1-6 gain
      const p = num - 30;                         // CC30-35: channel 1-6 pan
      if (g >= 0 && g < STEMS.length) {
        setParam(STEMS[g].id, 'gain', (val / 127) * 1.4);
        st.textContent = `🎛 CC${num} → ${STEMS[g].name} vol ${document.getElementById('vlbl-' + STEMS[g].id).textContent}`;
      } else if (p >= 0 && p < STEMS.length) {
        setParam(STEMS[p].id, 'pan', Math.max(-1, Math.min(1, (val - 64) / 64)));
        st.textContent = `🎛 CC${num} → ${STEMS[p].name} pan ${document.getElementById('plbl-' + STEMS[p].id).textContent}`;
      } else if (num === 7) {                     // CC7: master out
        const v = (val / 127) * 1.2;
        setMaster(v);
        st.textContent = `🎛 CC7 → master out ${Math.round(v * 100)}%`;
      }
    },
  });
}

window.addEventListener('storage', (ev) => {
  if (ev.key !== STUDIO_LS_KEY) return;
  studioKeyApply();
  refreshKeyBadge();
  applyDeskSlice(studioRead().desk);
});

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
  .pad.q-snap { box-shadow: inset 0 -4px 0 var(--accent-hi); }
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
  .side canvas#recWave {
    width: 100%; height: 64px;
    background: #0b0908; border: 1px solid var(--border); border-radius: 6px;
  }
  .rec-row { display: flex; gap: 6px; align-items: center; margin-top: 8px; }
  .rec-row input[type="text"] { flex: 1; min-width: 0; }
  .rec-time { font-size: 10px; color: var(--red); min-width: 40px; text-align: right; }
  #recBtn.rec-on { background: linear-gradient(180deg, #ef4444, #b91c1c); color: #fff; border-color: #fca5a5; }
  .rec-sub {
    font-size: 9px; color: var(--muted); letter-spacing: 0.08em;
    margin-top: 10px; padding-top: 8px; border-top: 1px solid var(--border);
  }
  .usamp { display: flex; gap: 6px; align-items: center; margin-top: 6px; }
  .usamp-name {
    flex: 1; min-width: 0; font-size: 10px; font-weight: 700;
    border-left: 3px solid var(--accent); padding-left: 6px;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .usamp select { max-width: 118px; font-size: 9.5px; padding: 3px 4px; }
  .usamp .mini-btn { flex-shrink: 0; }

  /* LOOP station */
  .loop-card { grid-column: 1 / -1; }
  .loop-layout {
    display: grid;
    grid-template-columns: 180px 1fr;
    gap: 14px;
    align-items: start;
  }
  @media (max-width: 700px) { .loop-layout { grid-template-columns: 1fr; } }
  #loopRing {
    width: 180px;
    height: 180px;
    background: #0b0908;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .loop-controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
  }
  .loop-pill {
    display: flex;
    align-items: center;
    gap: 5px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    padding: 3px 7px;
    border-radius: 6px;
  }
  .loop-pill label { color: var(--muted); font-size: 9.5px; text-transform: uppercase; }
  .loop-pill input[type="number"] { width: 54px; padding: 3px 5px; }
  .loop-pill select { padding: 3px 5px; }
  .loop-pill b { color: var(--accent-hi); font-size: 10px; }
  .mini-btn.on { background: var(--green); color: #06210f; border-color: #4ade80; }
  .mini-btn.rec-on { background: var(--red); color: #fff; border-color: #f87171; }
  .loop-status { font-size: 10px; color: var(--muted); }
  .layer-list { display: flex; flex-direction: column; gap: 4px; max-height: 160px; overflow-y: auto; }
  .layer-row {
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--panel-2);
    border: 1px solid #2b231f;
    border-radius: 6px;
    padding: 4px 8px;
  }
  .layer-row.muted { opacity: 0.45; }
  .layer-name { flex: 1; font-size: 10.5px; }
  .layer-dots { display: flex; gap: 2px; }
  .layer-dot { width: 7px; height: 7px; border-radius: 50%; }
  .layer-empty { color: var(--muted); font-size: 10px; padding: 6px 2px; }
  .pad.echo { box-shadow: 0 0 10px var(--stem-color, var(--accent)); }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE PADS // FART MPC</h1>
      <p>16 finger-drum pads over the stems + your recordings · choke per stem · hit high for max velocity</p>
    </div>
  </div>
  <span class="key-badge" id="keyBadge" title="Studio key is set on the SEQ+MIX page">KEY: —</span>
  <nav class="page-nav">
    <a href="/">SEQ+MIX</a>
    <a href="/mixer">DESK</a>
    <a href="/pads" class="active">PADS</a>
    <a href="/library">CRATES</a>
    <a href="/jam">JAM</a>
    <span class="midi-badge" id="midiBadge">MIDI: …</span>
  </nav>
</header>

<div class="pad-layout">
  <section class="card">
    <div class="card-header">
      <span class="card-title">Pad Grid — keys 1-4 / Q W E R / A S D F / Z X C V</span>
      <div style="display:flex; align-items:center; gap:8px;">
        <label for="midiBase" style="color:var(--muted); font-size:9.5px; text-transform:uppercase; letter-spacing:0.06em;">MIDI base note</label>
        <input id="midiBase" type="number" min="0" max="112" value="36" style="width:54px;" onchange="setMidiBase(this.value)" />
        <button class="mini-btn" id="dryBtn" onclick="toggleAllDry()">DRY KIT</button>
      </div>
    </div>
    <div class="pad-grid" id="padGrid"></div>
    <div class="status-banner"><span id="statusText">Whack a pad. Same stem chokes its own tail — flam away.</span><span id="statusLink"></span></div>
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
    <section class="card">
      <div class="card-header"><span class="card-title">Recorder — catch a real one</span></div>
      <canvas id="recWave" width="230" height="64"></canvas>
      <div class="rec-row">
        <button id="recBtn" class="primary" onclick="toggleRecord()">● REC</button>
        <button id="recPlay" class="mini-btn" onclick="playTake()" disabled title="audition the take">▶</button>
        <span class="rec-time" id="recTime"></span>
      </div>
      <div class="rec-row">
        <input type="text" id="recName" maxlength="48" placeholder="name it — letters, digits, space _ -" />
        <button id="saveBtn" onclick="saveTake()" disabled>SAVE</button>
      </div>
      <div id="userSamples"></div>
      <div class="status-banner" id="recStatus" style="min-height:0;">Record a fart, trim + normalize happens on save…</div>
    </section>
    <section class="card legend">
      <b>PADS:</b> bombs on top row, lead second, wah/snare/hat third, kicks &amp; thuds on the bottom.<br>
      <b>VELOCITY:</b> click near the top edge of a pad to smack it, bottom edge for a soft poot.<br>
      <b>CHOKE:</b> retriggering a pad from the same stem cuts the previous tail.<br>
      <b>LOOP:</b> hits are quantized to 16ths on the loop grid. Each armed pass
      becomes a take layer — OVERDUB stacks them, off replaces them.
    </section>
  </div>

  <section class="card loop-card">
    <div class="card-header">
      <span class="card-title">Loop Station — event looper (16th-quantized)</span>
      <span class="loop-status" id="loopStatus">idle</span>
    </div>
    <div class="loop-layout">
      <canvas id="loopRing" width="180" height="180"></canvas>
      <div>
        <div class="loop-controls">
          <button class="mini-btn" id="loopRecBtn" onclick="toggleRec()">● REC</button>
          <button class="mini-btn" id="loopPlayBtn" onclick="toggleLoopPlay()">▶ PLAY</button>
          <button class="mini-btn" onclick="undoLayer()">↩ UNDO TAKE</button>
          <button class="mini-btn" onclick="clearLayers()">✕ CLEAR</button>
          <button class="mini-btn" onclick="exportLoop()">⬇ MP3</button>
          <span class="loop-pill"><label>BPM</label><input type="number" id="loopBpm" min="40" max="240" step="1" onchange="setLoopBpm(this.value)"><b id="loopBpmVal"></b></span>
          <span class="loop-pill"><label>BARS</label><select id="loopBars" onchange="setLoopBars(this.value)"><option value="1">1</option><option value="2" selected>2</option><option value="4">4</option></select></span>
          <button class="mini-btn on" id="clickBtn" onclick="toggleClick()">CLICK</button>
          <button class="mini-btn on" id="odubBtn" onclick="toggleOverdub()">OVERDUB</button>
        </div>
        <div class="layer-list" id="layerList"></div>
      </div>
    </div>
  </section>
</div>

<script>
""" + STUDIO_JS + MIDI_JS + r"""
// The pad map is the 'pads' slice of brownnote.studio.v1 — serialized as
// {stem, semi, label} per slot; 'key' stays a page-local default.
let PADS = PADS_DEFAULT.map(p => Object.assign({}, p));
let DEFAULT_STEM = null;

function applyPadsSlice(pads) {
  if (!Array.isArray(pads)) return;
  for (let i = 0; i < PADS.length && i < pads.length; i++) {
    const p = pads[i];
    if (!p || typeof p !== 'object') continue;
    if (p.stem !== undefined && !STEMS.find(s => s.id === p.stem)) continue; // unknown stems ignored
    if (p.stem !== undefined) PADS[i].stem = p.stem;
    if (typeof p.semi === 'number') PADS[i].semi = p.semi;
    if (p.label !== undefined) PADS[i].label = String(p.label);
  }
}

let STEMS = [];
let audioCtx = null;
let masterGain = null, masterAnalyser = null;
let allDry = false;
const rawBufs = {}, trimBufs = {};
const lastSrc = {};

let midiBase = 36;
try {
  const saved = parseInt(localStorage.getItem('brownnote.midiBaseNote'), 10);
  if (!isNaN(saved)) midiBase = Math.min(112, Math.max(0, saved));
} catch (e) {}

function setMidiBase(v) {
  midiBase = Math.min(112, Math.max(0, parseInt(v, 10) || 0));
  document.getElementById('midiBase').value = midiBase;
  try { localStorage.setItem('brownnote.midiBaseNote', String(midiBase)); } catch (e) {}
  document.getElementById('statusText').textContent =
    `🎹 MIDI base note ${midiBase} — listening on notes ${midiBase}–${midiBase + 15}, all channels.`;
}

function stemById(id) {
  return studioStemById(STEMS, DEFAULT_STEM, id);
}

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
  if (STEMS.length) preloadKit();
}

async function padBuffer(stem) {
  const url = allDry ? stem.dryUrl : stem.url;
  if (!rawBufs[url]) {
    const res = await fetch(url);
    const arr = await res.arrayBuffer();
    rawBufs[url] = await audioCtx.decodeAudioData(arr);
  }
  const src = rawBufs[url];
  const key = url + '|' + stem.trimSec;
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
  const tHit = audioCtx.currentTime;
  const pad = PADS[i];
  const stem = stemById(pad.stem);
  if (!stem) return;
  if (loopRecording && take) {
    take.events.push({
      stemId: pad.stem, semi: pad.semi,
      vel: Math.round(vel * 100) / 100,
      step: quantizeStep(tHit),
    });
  }

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
  const semi = qStem(pad.stem, pad.semi);
  src.playbackRate.value = Math.pow(2, semi / 12);
  const g = audioCtx.createGain();
  g.gain.value = vel;
  src.connect(g); g.connect(masterGain);
  src.start();
  lastSrc[pad.stem] = src;
  src.onended = () => { if (lastSrc[pad.stem] === src) lastSrc[pad.stem] = null; };
  const snapTxt = semi !== pad.semi ? ` · Q→${semi >= 0 ? '+' : ''}${semi}st` : '';
  document.getElementById('statusText').textContent = `💥 ${pad.label} @ vel ${Math.round(vel*100)}%${snapTxt}`;
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
    const stem = stemById(pad.stem);
    const el = document.createElement('div');
    el.className = 'pad';
    el.id = `pad-${i}`;
    el.style.setProperty('--stem-color', stem ? stem.color : '#e07a3c');
    const snapped = (KEY.q && stem && PITCHED.includes(stem.id)) ? q(pad.semi) : pad.semi;
    if (snapped !== pad.semi) el.classList.add('q-snap');
    const subTxt = pad.semi
      ? `${pad.semi > 0 ? '+' : ''}${pad.semi}st` + (snapped !== pad.semi ? `→${snapped >= 0 ? '+' : ''}${snapped}` : '')
      : (snapped !== pad.semi ? `Q→${snapped >= 0 ? '+' : ''}${snapped}st` : '');
    el.innerHTML = `
      <span class="pad-label">${pad.label}</span>
      <span class="pad-sub">${stem ? stem.name : pad.stem} ${subTxt}</span>
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

// ---- RECORDER (#43): getUserMedia -> MediaRecorder -> decodeAudioData ->
// OfflineAudioContext resample to 44.1 kHz mono -> strip leading silence
// (-45 dBFS), peak-normalize to -1 dBFS, cap 4 s -> s16 WAV.  The server
// takes only that exact shape; no ffmpeg on the server side at all.

const REC_SR = 44100;
const REC_MAX_SEC = 4;
const REC_SILENCE = Math.pow(10, -45 / 20);
const REC_PEAK = Math.pow(10, -1 / 20);
const REC_NAME_RE = /^[A-Za-z0-9 _-]{1,48}$/;

let mediaStream = null, mediaRec = null, recChunks = [];
let recTimer = null, recT0 = 0;
let takePcm = null, takeWav = null;

function recStatus(msg) {
  document.getElementById('recStatus').textContent = msg;
}

async function toggleRecord() {
  if (mediaRec && mediaRec.state === 'recording') {
    mediaRec.stop();
    return;
  }
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
    recStatus('⚠ Mic capture needs a secure context — open the studio on ' +
              'localhost / 127.0.0.1 (a LAN IP will not work).');
    return;
  }
  ensureAudio();
  try {
    mediaStream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch (e) {
    recStatus('⚠ Mic unavailable: ' + (e && e.message ? e.message : e) +
              ' — getUserMedia wants localhost/127.0.0.1 or https.');
    return;
  }
  recChunks = [];
  mediaRec = new MediaRecorder(mediaStream);
  mediaRec.ondataavailable = (ev) => { if (ev.data && ev.data.size) recChunks.push(ev.data); };
  mediaRec.onstop = onTakeDone;
  mediaRec.start();
  recT0 = Date.now();
  const btn = document.getElementById('recBtn');
  btn.textContent = '■ STOP';
  btn.classList.add('rec-on');
  takePcm = takeWav = null;
  document.getElementById('recPlay').disabled = true;
  document.getElementById('saveBtn').disabled = true;
  drawRecWave(null);
  recTimer = setInterval(() => {
    document.getElementById('recTime').textContent = ((Date.now() - recT0) / 1000).toFixed(1) + 's';
  }, 100);
  recStatus('🔴 recording… STOP when the deed is done.');
}

async function onTakeDone() {
  clearInterval(recTimer); recTimer = null;
  if (mediaStream) { for (const t of mediaStream.getTracks()) t.stop(); mediaStream = null; }
  const btn = document.getElementById('recBtn');
  btn.textContent = '● REC';
  btn.classList.remove('rec-on');
  mediaRec = null;
  try {
    const blob = new Blob(recChunks);
    const decoded = await audioCtx.decodeAudioData(await blob.arrayBuffer());
    takePcm = processTake(await resampleMono(decoded));
    if (!takePcm.length) {
      drawRecWave(null);
      recStatus('⚠ take was all silence — fart louder.');
      return;
    }
    takeWav = encodeWav16(takePcm);
    drawRecWave(takePcm);
    document.getElementById('recPlay').disabled = false;
    document.getElementById('saveBtn').disabled = false;
    document.getElementById('recTime').textContent = (takePcm.length / REC_SR).toFixed(2) + 's';
    recStatus(`✓ ${(takePcm.length / REC_SR).toFixed(2)}s take — silence-stripped, normalized to −1 dBFS. ▶ it, name it, SAVE.`);
  } catch (e) {
    recStatus('⚠ could not decode the take: ' + (e && e.message ? e.message : e));
  }
}

async function resampleMono(buf) {
  const OAC = window.OfflineAudioContext || window.webkitOfflineAudioContext;
  const frames = Math.max(1, Math.ceil(buf.duration * REC_SR));
  const off = new OAC(1, frames, REC_SR);
  const src = off.createBufferSource();
  src.buffer = buf;
  src.connect(off.destination);
  src.start(0);
  const rendered = await off.startRendering();
  return rendered.getChannelData(0).slice();
}

function processTake(pcm) {
  let start = 0;
  while (start < pcm.length && Math.abs(pcm[start]) < REC_SILENCE) start++;
  const end = Math.min(pcm.length, start + REC_MAX_SEC * REC_SR);
  const out = pcm.slice(start, end);
  let peak = 0;
  for (let i = 0; i < out.length; i++) {
    const a = Math.abs(out[i]);
    if (a > peak) peak = a;
  }
  if (peak > 1e-6) {
    const g = REC_PEAK / peak;
    for (let i = 0; i < out.length; i++) out[i] *= g;
  }
  return out;
}

function encodeWav16(pcm) {
  const n = pcm.length;
  const buf = new ArrayBuffer(44 + n * 2);
  const v = new DataView(buf);
  const wstr = (off, s) => { for (let i = 0; i < s.length; i++) v.setUint8(off + i, s.charCodeAt(i)); };
  wstr(0, 'RIFF'); v.setUint32(4, 36 + n * 2, true); wstr(8, 'WAVE');
  wstr(12, 'fmt '); v.setUint32(16, 16, true); v.setUint16(20, 1, true);
  v.setUint16(22, 1, true); v.setUint32(24, REC_SR, true);
  v.setUint32(28, REC_SR * 2, true); v.setUint16(32, 2, true); v.setUint16(34, 16, true);
  wstr(36, 'data'); v.setUint32(40, n * 2, true);
  for (let i = 0; i < n; i++) {
    const s = Math.max(-1, Math.min(1, pcm[i]));
    v.setInt16(44 + i * 2, Math.round(s * 32767), true);
  }
  return buf;
}

function drawRecWave(pcm) {
  const cv = document.getElementById('recWave');
  const ctx = cv.getContext('2d');
  ctx.fillStyle = '#0b0908';
  ctx.fillRect(0, 0, cv.width, cv.height);
  if (!pcm || !pcm.length) {
    ctx.fillStyle = '#463a33';
    ctx.font = '9px monospace';
    ctx.fillText('no take yet', 8, cv.height / 2 + 3);
    return;
  }
  const buckets = 160;
  const step = Math.max(1, Math.floor(pcm.length / buckets));
  const w = cv.width / buckets;
  ctx.fillStyle = '#38bdf8';
  for (let b = 0; b < buckets; b++) {
    let m = 0;
    const off = b * step;
    for (let i = 0; i < step && off + i < pcm.length; i += 5) {
      const a = Math.abs(pcm[off + i]);
      if (a > m) m = a;
    }
    const h = Math.max(1, m * cv.height * 0.92);
    ctx.fillRect(b * w, (cv.height - h) / 2, Math.max(1, w - 0.5), h);
  }
}

function playTake() {
  if (!takePcm || !takePcm.length) return;
  ensureAudio();
  const buf = audioCtx.createBuffer(1, takePcm.length, REC_SR);
  buf.copyToChannel(takePcm, 0);
  const src = audioCtx.createBufferSource();
  src.buffer = buf;
  src.connect(masterGain);
  src.start();
}

async function saveTake() {
  if (!takeWav) return;
  const name = document.getElementById('recName').value.trim();
  if (!REC_NAME_RE.test(name)) {
    recStatus('⚠ name it first — letters, digits, space, _ and - only, up to 48 chars.');
    return;
  }
  document.getElementById('saveBtn').disabled = true;
  let data = {};
  try {
    const res = await fetch('/api/upload-sample?name=' + encodeURIComponent(name), {
      method: 'POST',
      headers: { 'Content-Type': 'audio/wav' },
      body: takeWav,
    });
    data = await res.json();
    if (!res.ok || !data.id) {
      recStatus('❌ save failed: ' + (data.error || `HTTP ${res.status}`));
      document.getElementById('saveBtn').disabled = false;
      return;
    }
  } catch (e) {
    recStatus('❌ save failed: ' + (e && e.message ? e.message : e));
    document.getElementById('saveBtn').disabled = false;
    return;
  }
  const ix = STEMS.findIndex(s => s.id === data.id);
  if (ix >= 0) STEMS[ix] = data; else STEMS.push(data);
  delete rawBufs[data.url];
  delete trimBufs[data.url + '|t'];
  renderUserSamples();
  recStatus(`✓ saved "${data.name}" (${(takePcm.length / REC_SR).toFixed(2)}s) — assign it to a pad below.`);
}

function renderUserSamples() {
  const wrap = document.getElementById('userSamples');
  wrap.innerHTML = '';
  const users = STEMS.filter(s => s.user);
  if (!users.length) return;
  const sub = document.createElement('div');
  sub.className = 'rec-sub';
  sub.textContent = 'YOUR SAMPLES — assign to a pad, or delete';
  wrap.appendChild(sub);
  for (const s of users) {
    const row = document.createElement('div');
    row.className = 'usamp';
    const nameEl = document.createElement('span');
    nameEl.className = 'usamp-name';
    nameEl.style.borderColor = s.color;
    nameEl.textContent = s.name;
    nameEl.title = s.name;
    const sel = document.createElement('select');
    sel.innerHTML = '<option value="">→ pad…</option>' +
      PADS.map((p, i) => `<option value="${i}" ${p.stem === s.id ? 'selected' : ''}>${i + 1} · ${p.key.toUpperCase()} · ${p.label}</option>`).join('');
    sel.onchange = () => assignSample(s.id, sel.value);
    const del = document.createElement('button');
    del.className = 'mini-btn';
    del.textContent = 'DEL';
    del.onclick = () => deleteSample(s.id, s.name);
    row.appendChild(nameEl);
    row.appendChild(sel);
    row.appendChild(del);
    wrap.appendChild(row);
  }
}

function assignSample(stemId, padIdx) {
  if (padIdx === '') return;
  const i = parseInt(padIdx, 10);
  const stem = STEMS.find(s => s.id === stemId);
  if (!stem || !(i >= 0 && i < PADS.length)) return;
  PADS[i].stem = stemId;
  PADS[i].semi = 0;
  PADS[i].label = stem.name.toUpperCase().slice(0, 14);
  studioMerge({ pads: studioPadsSerialize(PADS) });
  buildPads();
  renderUserSamples();
  recStatus(`✓ "${stem.name}" lives on pad ${i + 1} (${PADS[i].key.toUpperCase()}) — it's in the patch.`);
}

async function deleteSample(stemId, name) {
  const stem = STEMS.find(s => s.id === stemId);
  let data = {};
  try {
    const res = await fetch('/api/upload-sample', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: stemId, name, delete: true }),
    });
    data = await res.json();
    if (!res.ok || !data.ok) {
      recStatus('❌ delete failed: ' + (data.error || `HTTP ${res.status}`));
      return;
    }
  } catch (e) {
    recStatus('❌ delete failed: ' + (e && e.message ? e.message : e));
    return;
  }
  const ix = STEMS.findIndex(s => s.id === stemId);
  if (ix >= 0) STEMS.splice(ix, 1);
  if (stem) {
    delete rawBufs[stem.url];
    delete trimBufs[stem.url + '|t'];
  }
  // pads pointed at a deleted stem fall back to that slot's default
  let changed = false;
  PADS.forEach((p, i) => {
    if (p.stem === stemId) { Object.assign(p, PADS_DEFAULT[i]); changed = true; }
  });
  if (changed) studioMerge({ pads: studioPadsSerialize(PADS) });
  buildPads();
  renderUserSamples();
  recStatus(`✓ "${name}" deleted.`);
}

// ---------- LOOP STATION (event looper, issue #48) ----------
// Lookahead scheduler: a 25ms timer schedules every grid event falling
// inside the next 100ms at audioCtx.currentTime-based times. Recorded hits
// are stamped with audioCtx.currentTime and quantized to the nearest 16th.
const LOOKAHEAD_MS = 25, HORIZON_S = 0.1;
let bpm = initBpm();
let loopBars = 2;
let loopPlaying = false, loopRecording = false;
let overdubOn = true, clickOn = true;
let loopStart = 0;                 // audioCtx time of loop step 0
let schedTimer = null;
let schedStep = 0, schedTime = 0;  // next absolute step index and its audio time
let layers = [], take = null, layerSeq = 0;
let schedLate = 0;                 // worst event-scheduling lateness (acceptance log)

function initBpm() {
  try {
    const st = JSON.parse(localStorage.getItem('brownnote.studio.v1') || 'null');
    const v = st && st.seq && +st.seq.bpm;
    if (v >= 40 && v <= 240) return Math.round(v);
  } catch (e) {}
  return 116;
}

const nSteps = () => 16 * loopBars;
const stepSec = () => 60 / bpm / 4;
const loopSec = () => nSteps() * stepSec();

function preloadKit() {
  return Promise.all(STEMS.map(s => padBuffer(s).catch(() => null)));
}

function setStatus(msg) {
  document.getElementById('statusText').textContent = msg;
}

function quantizeStep(t) {
  const s = Math.round((t - loopStart) / stepSec());
  return ((s % nSteps()) + nSteps()) % nSteps();
}

function firePadEvent(ev, when) {
  const stem = stemById(ev.stemId);
  if (!stem) return;
  const buf = trimBufs[(allDry ? stem.dryUrl : stem.url) + '|' + stem.trimSec];
  if (!buf) { padBuffer(stem); return; }  // warm for the next pass
  const prev = lastSrc[ev.stemId];
  if (prev) { try { prev.stop(when); } catch (e) {} }
  const src = audioCtx.createBufferSource();
  src.buffer = buf;
  src.playbackRate.value = Math.pow(2, qStem(ev.stemId, ev.semi) / 12);
  const g = audioCtx.createGain();
  g.gain.value = ev.vel;
  src.connect(g); g.connect(masterGain);
  src.start(when);
  src.stop(when + buf.duration + 0.05);
  lastSrc[ev.stemId] = src;
  src.onended = () => { if (lastSrc[ev.stemId] === src) lastSrc[ev.stemId] = null; };
  setTimeout(() => echoStem(ev.stemId), Math.max(0, (when - audioCtx.currentTime) * 1000));
}

function echoStem(stemId) {
  PADS.forEach((p, i) => {
    if (p.stem !== stemId) return;
    const el = document.getElementById(`pad-${i}`);
    if (!el) return;
    el.classList.add('echo');
    setTimeout(() => el.classList.remove('echo'), 90);
  });
}

function click(accent, when) {
  const o = audioCtx.createOscillator();
  const g = audioCtx.createGain();
  o.type = 'square';
  o.frequency.value = accent ? 2200 : 1400;
  g.gain.setValueAtTime(accent ? 0.20 : 0.11, when);
  g.gain.exponentialRampToValueAtTime(0.0001, when + 0.035);
  o.connect(g); g.connect(masterGain);
  o.start(when);
  o.stop(when + 0.05);
}

function schedTick() {
  if (!loopPlaying || !audioCtx) return;
  const horizon = audioCtx.currentTime + HORIZON_S;
  const sSec = stepSec(), n = nSteps();
  let guard = 0;
  while (schedTime < horizon && guard++ < 8192) {
    const s = schedStep % n;
    const stale = schedTime < audioCtx.currentTime - 0.06;
    if (!stale) {
      for (const layer of layers) {
        if (layer.muted) continue;
        for (const ev of layer.byStep[s] || []) {
          const late = audioCtx.currentTime - schedTime;
          if (late > schedLate) {
            schedLate = late;
            console.debug(`[looper] event scheduled ${Math.round(late * 1000)}ms late (worst so far)`);
          }
          firePadEvent(ev, schedTime);
        }
      }
      if (clickOn && s % 4 === 0) click(s % 16 === 0, schedTime);
    }
    schedStep++;
    schedTime = loopStart + schedStep * sSec;
    if (schedStep % n === 0) onCycleWrap();
  }
}

function onCycleWrap() {
  // An armed pass became a layer the moment the loop wrapped.
  if (loopRecording && take && take.events.length) commitTake();
}

async function startLoop() {
  await preloadKit();
  loopStart = audioCtx.currentTime + 0.08;
  schedStep = 0;
  schedTime = loopStart;
  schedLate = 0;
  loopPlaying = true;
  schedTimer = setInterval(schedTick, LOOKAHEAD_MS);
  schedTick();
  const btn = document.getElementById('loopPlayBtn');
  btn.textContent = '⏹ STOP';
  btn.classList.add('rec-on');
  updateLoopStatus();
}

function stopLoop() {
  if (loopRecording) disarmRec();
  loopPlaying = false;
  clearInterval(schedTimer);
  for (const k in lastSrc) {
    if (lastSrc[k]) { try { lastSrc[k].stop(); } catch (e) {} lastSrc[k] = null; }
  }
  const btn = document.getElementById('loopPlayBtn');
  btn.textContent = '▶ PLAY';
  btn.classList.remove('rec-on');
  updateLoopStatus();
}

async function toggleLoopPlay() {
  ensureAudio();
  if (loopPlaying) stopLoop(); else await startLoop();
}

async function toggleRec() {
  ensureAudio();
  if (loopRecording) { disarmRec(); return; }
  if (!loopPlaying) await startLoop();
  take = { events: [] };
  loopRecording = true;
  document.getElementById('loopRecBtn').classList.add('rec-on');
  setStatus('⏺ recording — play the pads; each loop pass becomes a take');
  updateLoopStatus();
}

function disarmRec() {
  loopRecording = false;
  document.getElementById('loopRecBtn').classList.remove('rec-on');
  commitTake();
  updateLoopStatus();
}

function commitTake() {
  if (take && take.events.length) {
    if (!overdubOn) layers = [];
    const byStep = {};
    for (const ev of take.events) (byStep[ev.step] = byStep[ev.step] || []).push(ev);
    layers.push({ id: ++layerSeq, events: take.events, byStep, muted: false });
    setStatus(`✓ take ${layerSeq} locked in — ${take.events.length} hit(s)`);
  }
  // Still armed? Start the next pass's take; disarmed takes go away.
  take = loopRecording ? { events: [] } : null;
  renderLayers();
}

function loopPhase() {
  // Position within the current cycle in [0, 1) — read before changing
  // bpm/bars so the transport can re-anchor without a phase jump.
  return (((audioCtx.currentTime - loopStart) / loopSec()) % 1 + 1) % 1;
}

function setLoopBpm(v) {
  const nv = Math.max(40, Math.min(240, parseInt(v, 10) || bpm));
  const phase = (loopPlaying && audioCtx) ? loopPhase() : 0;
  bpm = nv;
  if (loopPlaying) {
    loopStart = audioCtx.currentTime - phase * loopSec();
    schedStep = Math.ceil((audioCtx.currentTime - loopStart) / stepSec() - 1e-9);
    schedTime = loopStart + schedStep * stepSec();
  }
  document.getElementById('loopBpm').value = bpm;
  document.getElementById('loopBpmVal').textContent = bpm;
  updateLoopStatus();
}

function setLoopBars(v) {
  const nv = parseInt(v, 10);
  if (![1, 2, 4].includes(nv)) return;
  const phase = (loopPlaying && audioCtx) ? loopPhase() : 0;
  loopBars = nv;
  if (loopPlaying) {
    loopStart = audioCtx.currentTime - phase * loopSec();
    schedStep = Math.ceil((audioCtx.currentTime - loopStart) / stepSec() - 1e-9);
    schedTime = loopStart + schedStep * stepSec();
  }
  for (const l of layers) {
    l.events = l.events.filter(e => e.step < nSteps());
    l.byStep = {};
    for (const ev of l.events) (l.byStep[ev.step] = l.byStep[ev.step] || []).push(ev);
  }
  if (take) take.events = take.events.filter(e => e.step < nSteps());
  renderLayers();
}

function toggleClick() {
  clickOn = !clickOn;
  document.getElementById('clickBtn').classList.toggle('on', clickOn);
}

function toggleOverdub() {
  overdubOn = !overdubOn;
  document.getElementById('odubBtn').classList.toggle('on', overdubOn);
  setStatus(overdubOn ? 'OVERDUB on — new takes stack as layers' : 'OVERDUB off — next take replaces all layers');
}

function toggleLayerMute(id) {
  const l = layers.find(x => x.id === id);
  if (l) { l.muted = !l.muted; renderLayers(); }
}

function deleteLayer(id) {
  layers = layers.filter(x => x.id !== id);
  renderLayers();
}

function undoLayer() {
  const gone = layers.pop();
  renderLayers();
  if (gone) setStatus(`↩ undid take ${gone.id} — ${layers.length} layer(s) left`);
}

function clearLayers() {
  layers = [];
  renderLayers();
  setStatus('✕ loop cleared');
}

function updateLoopStatus() {
  const mode = loopRecording ? '⏺ REC' : (loopPlaying ? '▶ looping' : 'idle');
  const late = loopPlaying && schedLate > 0.001 ? ` · sched late ≤${Math.round(schedLate * 1000)}ms` : '';
  document.getElementById('loopStatus').textContent =
    `${mode} · ${loopBars} bar${loopBars > 1 ? 's' : ''} @ ${bpm} BPM · ` +
    `${layers.length} take${layers.length === 1 ? '' : 's'} · ${overdubOn ? 'overdub' : 'replace'}${late}`;
}

function renderLayers() {
  const el = document.getElementById('layerList');
  if (!layers.length) {
    el.innerHTML = '<div class="layer-empty">no takes yet — hit ● REC, then play the pads. Each loop pass becomes a layer.</div>';
  } else {
    el.innerHTML = layers.map(l => {
      const dots = [...new Set(l.events.map(e => e.stemId))].slice(0, 8).map(id => {
        const st = STEMS.find(s => s.id === id);
        return `<span class="layer-dot" style="background:${st ? st.color : '#888'}"></span>`;
      }).join('');
      return `<div class="layer-row ${l.muted ? 'muted' : ''}">
        <span class="layer-dots">${dots}</span>
        <span class="layer-name">TAKE ${l.id} — ${l.events.length} hit${l.events.length === 1 ? '' : 's'}</span>
        <button class="mini-btn ${l.muted ? 'mute-on' : ''}" onclick="toggleLayerMute(${l.id})">M</button>
        <button class="mini-btn" onclick="deleteLayer(${l.id})">✕</button>
      </div>`;
    }).join('');
  }
  updateLoopStatus();
}

// Export is a lossy summary of the loop: one (velocity, pitch) pair per
// stem per 16th — later takes win collisions; velocity quantizes to the
// renderer's 3 levels (soft/medium/accent).
function quantVel(v) { return v >= 0.95 ? 2 : (v >= 0.6 ? 1 : 3); }

async function exportLoop() {
  const stLink = document.getElementById('statusLink');
  if (!layers.some(l => !l.muted && l.events.length)) {
    setStatus('nothing to export — record a take first');
    return;
  }
  const n = nSteps();
  const tracks = {};
  for (const stem of STEMS) {
    tracks[stem.id] = {
      steps: new Array(n).fill(0),
      stepPitches: new Array(n).fill(0),
      pitch: 0, gain: stem.defaultGain ?? 0.9, pan: 0,
      trimSec: stem.trimSec, useDry: allDry, mute: false, solo: false,
    };
  }
  for (const l of layers) {
    if (l.muted) continue;
    for (const ev of l.events) {
      const t = tracks[ev.stemId];
      if (!t) continue;
      t.steps[ev.step] = quantVel(ev.vel);
      t.stepPitches[ev.step] = qStem(ev.stemId, ev.semi);
    }
  }
  setStatus(`⏳ rendering ${loopBars}-bar loop @ ${bpm} BPM to MP3...`);
  stLink.innerHTML = '';
  try {
    const res = await fetch('/api/render-sequence', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ bpm, bars: loopBars, swing: 0, tracks }),
    });
    const data = await res.json();
    if (!data.ok) { setStatus(`❌ export failed: ${data.error}`); return; }
    setStatus(`✓ loop bounced (${data.durationSec}s) -> ${data.rootFile || data.url} — lossy summary of the loop`);
    stLink.innerHTML = `<a href="${data.url}" download style="color:var(--accent-hi);font-weight:700;">⬇ Download MP3</a>`;
  } catch (e) {
    setStatus(`❌ export failed: ${e}`);
  }
}

function drawRing() {
  const cv = document.getElementById('loopRing');
  const ctx = cv.getContext('2d');
  let frameNo = 0;
  function frame() {
    requestAnimationFrame(frame);
    const w = cv.width, h = cv.height, cx = w / 2, cy = h / 2, r = 76;
    const n = nSteps();
    ctx.fillStyle = '#0b0908';
    ctx.fillRect(0, 0, w, h);

    for (let s = 0; s < n; s++) {
      const a = (s / n) * Math.PI * 2 - Math.PI / 2;
      const inner = s % 16 === 0 ? r - 12 : (s % 4 === 0 ? r - 8 : r - 4);
      ctx.strokeStyle = s % 16 === 0 ? '#5b463a' : (s % 4 === 0 ? '#41322a' : '#2b211c');
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(cx + Math.cos(a) * inner, cy + Math.sin(a) * inner);
      ctx.lineTo(cx + Math.cos(a) * r, cy + Math.sin(a) * r);
      ctx.stroke();
    }

    const drawEvents = (evts, dim, override) => {
      for (const ev of evts) {
        const st = STEMS.find(s => s.id === ev.stemId);
        const a = (ev.step / n) * Math.PI * 2 - Math.PI / 2;
        ctx.fillStyle = override || (st ? st.color : '#888');
        ctx.globalAlpha = dim ? 0.22 : 0.95;
        ctx.beginPath();
        ctx.arc(cx + Math.cos(a) * (r - 18), cy + Math.sin(a) * (r - 18),
                1.6 + ev.vel * 1.6, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.globalAlpha = 1;
    };
    for (const l of layers) drawEvents(l.events, l.muted);
    if (take) drawEvents(take.events, false, '#ef4444');

    if (loopPlaying && audioCtx) {
      const phase = (((audioCtx.currentTime - loopStart) / loopSec()) % 1 + 1) % 1;
      const a = phase * Math.PI * 2 - Math.PI / 2;
      const col = loopRecording ? '#ef4444' : '#ffb347';
      ctx.strokeStyle = col;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(cx, cy);
      ctx.lineTo(cx + Math.cos(a) * r, cy + Math.sin(a) * r);
      ctx.stroke();
      ctx.beginPath();
      ctx.arc(cx, cy, r + 3, -Math.PI / 2, a);
      ctx.strokeStyle = loopRecording ? 'rgba(239,68,68,0.5)' : 'rgba(255,179,71,0.35)';
      ctx.lineWidth = 3;
      ctx.stroke();
    }

    ctx.fillStyle = '#9e8e82';
    ctx.font = '10px ui-monospace, monospace';
    ctx.textAlign = 'center';
    ctx.fillText(`${bpm} BPM`, cx, cy - 4);
    ctx.fillText(`${loopBars} BAR${loopBars > 1 ? 'S' : ''}`, cx, cy + 10);

    if (++frameNo % 30 === 0) updateLoopStatus();
  }
  frame();
}

async function init() {
  const res = await fetch('/api/state');
  const data = await res.json();
  STEMS = data.stems;
  DEFAULT_STEM = data.default || (STEMS[0] && STEMS[0].id) || null;
  PADS = studioPadsBoot(STEMS, DEFAULT_STEM);
  const stored = studioRead();
  studioKeyApply();
  refreshKeyBadge();
  applyPadsSlice(stored.pads);
  buildPads();
  renderUserSamples();
  drawRecWave(null);
  if (!stored.pads) {
    studioMerge({ pads: studioPadsSerialize(PADS) });   // seed once
  }
  document.getElementById('loopBpm').value = bpm;
  document.getElementById('loopBpmVal').textContent = bpm;
  renderLayers();
  drawRing();
  document.getElementById('midiBase').value = midiBase;
  initMidi({
    noteOn: (note, vel) => {
      const off = note - midiBase;                 // bottom-left pad sends `base`
      if (off < 0 || off > 15) return;
      const row = 3 - (off >> 2), col = off & 3;   // physical 4x4 layout = screen
      hitPad(row * 4 + col, vel / 127);
    },
  });
}

window.addEventListener('storage', (ev) => {
  if (ev.key !== STUDIO_LS_KEY) return;
  studioKeyApply();
  refreshKeyBadge();
  applyPadsSlice(studioRead().pads);
  buildPads();
  renderUserSamples();
});

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
  .crate.is-default { border-color: var(--yellow); box-shadow: 0 0 12px rgba(245,158,11,0.25); }
  .def-star { color: var(--yellow); font-weight: 700; }
  .kind-synth { --kind-color: #e07a3c; }
  .kind-recorded { --kind-color: #22c55e; }
  .kind-render { --kind-color: #4ecdc4; }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE CRATES // FLATULENCE LIBRARY</h1>
      <p>build/library.json — every usable WAV/MP3 cataloged. Drop a 44.1k mono s16 WAV in build/library/ and hit ↻ to make it a stem; other WAVs promote in one click.</p>
    </div>
  </div>
  <nav class="page-nav">
    <a href="/">SEQ+MIX</a>
    <a href="/mixer">DESK</a>
    <a href="/pads">PADS</a>
    <a href="/library" class="active">CRATES</a>
    <a href="/jam">JAM</a>
  </nav>
</header>

<section class="card">
  <div class="card-header">
    <div class="filter-bar" id="filterBar">
      <button class="filter-btn active" data-kind="all" onclick="setFilter('all', this)">All</button>
      <button class="filter-btn" data-kind="stem" onclick="setFilter('stem', this)">Stems</button>
      <button class="filter-btn" data-kind="synth" onclick="setFilter('synth', this)">Synth</button>
      <button class="filter-btn" data-kind="recorded" onclick="setFilter('recorded', this)">Recorded</button>
      <button class="filter-btn" data-kind="render" onclick="setFilter('render', this)">Renders</button>
      <button class="filter-btn" data-kind="raw" onclick="setFilter('raw', this)">Raw</button>
    </div>
    <button class="mini-btn" onclick="rescanLibrary()">↻ Rescan</button>
  </div>
  <div class="crate-grid" id="crateGrid"></div>
  <div class="status-banner" id="statusText">Digging the crates…</div>
</section>

<audio id="player" style="display:none"></audio>

<script>
const player = document.getElementById('player');
let LIB = { entries: [], default: null, defaultSet: null };
let filter = 'all';
let audioCtx = null;
const peaks = {};   // url -> Float32Array of peaks 0..1
let playingUrl = null;

const KIND_CLASS = { synth: 'kind-synth', recorded: 'kind-recorded', render: 'kind-render' };

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

function crateShown(e) {
  if (filter === 'all') return true;
  if (filter === 'stem') return !!e.stem;
  if (filter === 'raw') return !e.stem;
  return e.kind === filter;
}

function metaLine(e) {
  const bits = [];
  if (typeof e.durationSec === 'number') bits.push(`${e.durationSec}s`);
  if (typeof e.sizeKb === 'number') bits.push(`${e.sizeKb} KB`);
  if (typeof e.loudness === 'number') bits.push(`peak ${e.loudness}`);
  if (!e.stem && !e.conform) bits.push('raw — promote normalizes');
  return bits.join(' · ');
}

function renderCrates() {
  const grid = document.getElementById('crateGrid');
  grid.innerHTML = '';
  const shown = LIB.entries.filter(crateShown);
  const def = LIB.entries.find(e => e.stem === LIB.default);
  document.getElementById('statusText').textContent =
    (shown.length ? `${shown.length} record${shown.length === 1 ? '' : 's'} in the crate.` : 'Crate is empty — go render something on the SEQ page.')
    + (def ? ` · THE fart: ★ ${def.name}` : ' · no default fart resolved');
  for (const e of shown) {
    const el = document.createElement('div');
    el.className = `crate ${KIND_CLASS[e.kind] || ''}` +
      (e.stem === LIB.default ? ' is-default' : '');
    el.dataset.url = e.url;
    const isDef = e.stem && e.stem === LIB.default;
    const promoteBtn = (!e.stem && e.file.endsWith('.wav'))
      ? `<button class="mini-btn" onclick="promoteEntry('${e.id}')">＋ STEM</button>` : '';
    const starBtn = (e.stem && !isDef)
      ? `<button class="mini-btn" title="make THE default fart" onclick="setDefault('${e.stem}')">★</button>` : '';
    const stemTag = e.stem
      ? `<span class="tag">${isDef ? '<span class="def-star">★ DEFAULT</span>' : 'STEM'}</span>` : '';
    el.innerHTML = `
      <div class="crate-top">
        <span class="crate-name" title="${e.name}">${e.name}</span>
        <span class="tag">${e.kind}</span>${stemTag}
      </div>
      <canvas class="wave" data-url="${e.url}" width="300" height="44"></canvas>
      <div class="crate-top">
        <div class="crate-meta"><span>${metaLine(e)}</span></div>
        <div class="crate-btns">
          ${promoteBtn}${starBtn}
          <button class="mini-btn play-btn" onclick="toggleTrack('${e.url}')">▶</button>
          <a href="${e.url}" download>⬇</a>
        </div>
      </div>
    `;
    grid.appendChild(el);
    drawWave(e.url, 0);
  }
  markPlaying();
}

async function promoteEntry(id) {
  const res = await fetch('/api/library/promote', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id }),
  });
  const data = await res.json();
  if (!res.ok || !data.ok) {
    document.getElementById('statusText').textContent =
      `❌ promote failed: ${data.error || `HTTP ${res.status}`}`;
    return;
  }
  await loadCrates();
  document.getElementById('statusText').textContent =
    `✓ "${data.stem.name}" is a stem (${data.stem.id}) — playable on PADS/DESK.`;
}

async function setDefault(stemId) {
  const res = await fetch('/api/library/default', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: stemId }),
  });
  const data = await res.json();
  if (!res.ok || !data.ok) {
    document.getElementById('statusText').textContent =
      `❌ default failed: ${data.error || `HTTP ${res.status}`}`;
    return;
  }
  await loadCrates();
}

async function rescanLibrary() {
  document.getElementById('statusText').textContent = 'Rescanning…';
  const res = await fetch('/api/library/rescan', { method: 'POST' });
  const data = await res.json();
  if (!res.ok || !data.ok) {
    document.getElementById('statusText').textContent =
      `❌ rescan failed: ${data.error || `HTTP ${res.status}`}`;
    return;
  }
  await loadCrates();
}

async function loadCrates() {
  const res = await fetch('/api/library');
  const data = await res.json();
  LIB = data.ok ? data : { entries: [], default: null };
  renderCrates();
}

window.addEventListener('DOMContentLoaded', loadCrates);
</script>
</body>
</html>
"""


JAM_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>BROWNNOTE // NINJAM Jam Bridge</title>
<style>""" + PAGE_CSS + r"""
  .jam-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  @media (max-width: 900px) { .jam-grid { grid-template-columns: 1fr; } }
  .field { display: flex; flex-direction: column; gap: 4px; margin-bottom: 9px; }
  .field label { font-size: 9.5px; color: var(--muted); letter-spacing: 0.08em; }
  .field input {
    font-family: inherit;
    font-size: 11px;
    background: var(--panel-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 7px 10px;
  }
  .stat-row { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0; }
  .stat {
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 10px;
    font-size: 10.5px;
    color: var(--muted);
  }
  .stat b { color: var(--accent-hi); font-weight: 700; }
  .jam-badge {
    font-size: 11px;
    font-weight: 800;
    letter-spacing: 0.08em;
    padding: 5px 12px;
    border-radius: 6px;
    border: 1px solid var(--border);
    color: var(--muted);
  }
  .jam-badge.live { color: var(--green); border-color: var(--green); }
  .jam-badge.starting { color: var(--yellow); border-color: var(--yellow); }
  .jam-badge.error { color: var(--red); border-color: var(--red); }
  .result-line {
    font-size: 10px;
    color: var(--muted);
    word-break: break-all;
    min-height: 14px;
  }
  audio { width: 100%; margin-top: 8px; }
</style>
</head>
<body>

<header>
  <div class="brand">
    <div class="logo-badge">💨</div>
    <div>
      <h1>BROWNNOTE JAM // NINJAM BRIDGE</h1>
      <p>Jam farts with friends on the internet — your SEQ+MIX loop streams through kujamba</p>
    </div>
  </div>
  <nav class="page-nav">
    <a href="/">SEQ+MIX</a>
    <a href="/mixer">DESK</a>
    <a href="/pads">PADS</a>
    <a href="/library">CRATES</a>
    <a href="/jam" class="active">JAM</a>
  </nav>
</header>

<div class="jam-grid">
  <section class="card">
    <div class="card-header">
      <span class="card-title">Join a NINJAM server</span>
    </div>
    <div class="field">
      <label for="jamHost">SERVER HOST[:PORT]</label>
      <input id="jamHost" type="text" maxlength="255" placeholder="ninjam.example.org:2049" />
    </div>
    <div class="field">
      <label for="jamUser">USER</label>
      <input id="jamUser" type="text" maxlength="64" placeholder="stinker" />
    </div>
    <div class="field">
      <label for="jamPass">PASSWORD — POST body only, handed to kujamba via a 0600 file, never argv or logs</label>
      <input id="jamPass" type="password" maxlength="128" autocomplete="off" />
    </div>
    <button class="primary" onclick="joinJam()">💨 JOIN THE JAM</button>
    <div class="status-banner" id="joinStatus">Your current SEQ+MIX pattern rides along as the loop. Friends monitor you in their own NINJAM client.</div>
  </section>

  <section class="card">
    <div class="card-header">
      <span class="card-title">Session</span>
      <span class="jam-badge" id="jamBadge">OFF</span>
    </div>
    <div class="stat-row" id="jamStats"></div>
    <div class="result-line" id="jamResult"></div>
    <audio id="jamAudio" controls style="display:none"></audio>
    <div style="display:flex; gap:6px; margin-top:10px;">
      <button onclick="pushLoop()" title="Re-render the current SEQ+MIX pattern into loop.wav">↻ PUSH CURRENT LOOP</button>
      <button onclick="stopJam()">⏹ STOP</button>
    </div>
    <div class="status-banner" id="statusText">Idle.</div>
  </section>
</div>

<script>
""" + STUDIO_JS + r"""
function seqPayload() {
  const s = studioRead().seq;
  return (s && typeof s === 'object') ? s : {};
}

function setStatus(msg) {
  document.getElementById('statusText').textContent = msg;
}

function paintJam(st) {
  const badge = document.getElementById('jamBadge');
  badge.textContent = (st.state || 'off').toUpperCase();
  badge.className = 'jam-badge ' + (st.state || 'off');
  const stats = [];
  if (st.host) stats.push(`<span class="stat">server <b>${st.host}</b></span>`);
  if (st.user) stats.push(`<span class="stat">user <b>${st.user}</b></span>`);
  if (st.bpm) stats.push(`<span class="stat">bpm <b>${st.bpm}</b></span>`);
  if (st.bpi) stats.push(`<span class="stat">bpi <b>${st.bpi}</b> (${st.bars} bars)</span>`);
  if (st.intervalSec) stats.push(`<span class="stat">interval <b>${st.intervalSec}s${st.intervalClamped ? ' (clamped)' : ''}</b></span>`);
  document.getElementById('jamStats').innerHTML = stats.join('');
  document.getElementById('jamResult').textContent = st.resultLine || '';
  const audio = document.getElementById('jamAudio');
  if (st.loopUrl) {
    audio.style.display = '';
    if (!audio.src.endsWith(st.loopUrl)) audio.src = st.loopUrl;
  } else {
    audio.style.display = 'none';
    audio.removeAttribute('src');
  }
  if (st.error) setStatus('⚠ ' + st.error);
}

async function pollJam() {
  try {
    const res = await fetch('/api/jam');
    paintJam(await res.json());
  } catch (e) {}
}

async function joinJam() {
  const host = document.getElementById('jamHost').value.trim();
  const user = document.getElementById('jamUser').value.trim();
  const pass = document.getElementById('jamPass').value;
  document.getElementById('jamPass').value = '';
  const res = await fetch('/api/jam', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ host, user, pass, seq: seqPayload() }),
  });
  const data = await res.json();
  paintJam(data);
  document.getElementById('joinStatus').textContent = data.ok
    ? `✓ joining ${host} as ${user}…`
    : `❌ ${data.error}`;
}

async function pushLoop() {
  const res = await fetch('/api/jam', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ seq: seqPayload() }),
  });
  const data = await res.json();
  paintJam(data);
  if (!data.ok) setStatus(`❌ ${data.error}`);
}

async function stopJam() {
  const res = await fetch('/api/jam', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ stop: true }),
  });
  const data = await res.json();
  paintJam(data);
  setStatus(data.ok ? 'Left the jam.' : `❌ ${data.error}`);
}

setInterval(pollJam, 1000);
pollJam();
</script>
</body>
</html>
"""


EXTRA_PAGES = {
    "/mixer": MIXER_HTML,
    "/pads": PADS_HTML,
    "/library": LIBRARY_HTML,
    "/jam": JAM_HTML,
}


if __name__ == "__main__":
    try:
        ensure_stems()
    except Exception as e:
        print(f"warning: stem build failed ({e}); /api/state will retry and 503", file=sys.stderr)
    # A SIGKILLed previous run leaves its creds file + transcript behind.
    _jam_clean_dir()

    # Signals must not take JAM_LOCK here (it can be held mid-join, and
    # the lock isn't reentrant) — bail out and let finally: jam_stop()
    # below do the teardown on the main thread.
    def _shutdown_jam(*_):
        raise SystemExit(0)
    signal.signal(signal.SIGTERM, _shutdown_jam)
    if hasattr(signal, "SIGHUP"):   # terminal close must not orphan the child
        signal.signal(signal.SIGHUP, _shutdown_jam)

    try:
        scan_library()
    except Exception as e:
        print(f"warning: library scan failed ({e}); /api/library will retry", file=sys.stderr)
    with ReusableTCPServer((HOST, PORT), StudioHandler) as httpd:
        print(f"Brownnote Studio listening on http://{HOST}:{PORT}", flush=True)
        print("  pages: / (seq+mixer)  /mixer (desk)  /pads (MPC)  /library (crates)  /jam (nin)", flush=True)
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            jam_stop()
