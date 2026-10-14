#!/usr/bin/env python3
"""
studio_smoke.py — smoke test for scripts/studio.py (stdlib only).

Boots the studio server on a free loopback port, then checks:
  - GET /, /mixer, /pads, /library  -> 200 + text/html
  - GET /api/state                  -> 200 with stems, or 503 JSON if the
                                     stem build cannot run on this machine
  - POST /api/no-such-endpoint      -> 404 JSON {"ok": false, ...}
  - PATCHES (#45): POST /api/patch saves a v1 snapshot under
    build/patches/, GET /api/patches lists it, GET /api/patch?name=
    returns it, delete removes it, and pathy/oversized/empty names
    plus non-v1 schemas 400 without writing outside build/patches/
  - SAMPLES (#43): POST /api/upload-sample stores a 44.1 kHz mono s16
    WAV + stem def under build/user/, /api/state lists it user:true,
    the wav is served, stereo/48 kHz/float/pathy/non-WAV uploads 400,
    over-cap bodies 413, and {name, delete:true} removes both files
  - POST /api/render-sequence       -> when stems are available: a length-16
                                     steps grid repeats identically every
                                     bar, while a length-16*bars grid places
                                     hits on their own steps (issue #48
                                     looper export)

Prints "ok: ..." / "bad: ..." so tests/run_tests.sh can case-match the
verdict like the inline python3 heredocs do.  Exit 0 on pass, 1 on fail.
"""
import array
import http.client
import io
import json
import math
import os
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import wave

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
BASE = None  # set in main()


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def get(path):
    """-> (status, content-type, body bytes); HTTPError still returns data."""
    try:
        r = urllib.request.urlopen(BASE + path, timeout=10)
        return r.status, r.headers.get("Content-Type", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", ""), e.read()


def post(path, body=b"{}", timeout=10):
    req = urllib.request.Request(
        BASE + path, data=body, method="POST",
        headers={"Content-Type": "application/json"})
    try:
        r = urllib.request.urlopen(req, timeout=timeout)
        return r.status, r.headers.get("Content-Type", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", ""), e.read()


def post_typed(path, body, ctype):
    """POST with an explicit Content-Type (sample uploads are not JSON)."""
    req = urllib.request.Request(
        BASE + path, data=body, method="POST",
        headers={"Content-Type": ctype})
    try:
        r = urllib.request.urlopen(req, timeout=10)
        return r.status, r.headers.get("Content-Type", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", ""), e.read()


def wav_bytes(rate=44100, ch=1, width=2, secs=0.5):
    """A `secs`-second sine WAV via the wave module."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(ch)
        w.setsampwidth(width)
        w.setframerate(rate)
        n = int(rate * secs)
        pcm = bytearray()
        for i in range(n):
            v = 9000 * math.sin(2 * math.pi * 220 * i / rate)
            if width == 2:
                frame = struct.pack("<h", int(v)) * ch
            elif width == 1:
                frame = bytes([128 + int(v / 256)]) * ch
            else:
                frame = struct.pack("<i", int(v * 65536)) * ch
            pcm += frame
        w.writeframes(bytes(pcm))
    return buf.getvalue()


def float_wav(rate=44100, ch=1):
    """A hand-rolled IEEE-float WAV (format tag 3) the wave module won't write."""
    n = int(rate * 0.5)
    data = b"".join(
        struct.pack("<f", 0.5 * math.sin(2 * math.pi * 220 * i / rate))
        for i in range(n))
    fmt = (b"fmt " + struct.pack("<I", 16)
           + struct.pack("<HHIIHH", 3, ch, rate,
                         rate * ch * 4, ch * 4, 32))
    return (b"RIFF" + struct.pack("<I", 4 + 8 + 16 + 8 + len(data)) + b"WAVE"
            + fmt + b"data" + struct.pack("<I", len(data)) + data)


def wait_ready(proc, port, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            return False
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return True
        except OSError:
            time.sleep(0.2)
    return False


def main():
    global BASE
    port = free_port()
    BASE = f"http://127.0.0.1:{port}"
    env = dict(os.environ, STUDIO_PORT=str(port), STUDIO_HOST="127.0.0.1")
    proc = subprocess.Popen(
        [sys.executable, os.path.join(ROOT, "scripts", "studio.py")],
        cwd=ROOT, env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not wait_ready(proc, port):
            print(f"bad: studio.py did not listen on :{port} "
                  f"(exit {proc.poll()})")
            return 1

        fails = []

        for path in ("/", "/mixer", "/pads", "/library"):
            status, ctype, body = get(path)
            if status != 200:
                fails.append(f"GET {path}: status {status}")
            elif "text/html" not in ctype:
                fails.append(f"GET {path}: content-type {ctype!r}")
            elif b"<html" not in body[:512].lower():
                fails.append(f"GET {path}: 200 but no <html>")

        status, ctype, body = get("/api/state")
        stem_ids = []
        try:
            payload = json.loads(body)
        except Exception:
            payload = None
        if status == 200:
            if not isinstance(payload, dict) or "stems" not in payload:
                fails.append("GET /api/state: 200 but no 'stems' key")
            else:
                stem_ids = [s["id"] for s in payload["stems"] if "id" in s]
        elif status == 503:
            if not isinstance(payload, dict) or payload.get("ok") is not False:
                fails.append("GET /api/state: 503 but body is not "
                             '{"ok": false, ...}')
        else:
            fails.append(f"GET /api/state: status {status} (want 200 or 503)")

        # ---- #46: the gassy randomizer ships in the / page --------------
        status, _, body = get("/")
        if status == 200:
            for marker in (b"randomizePatch", b"mulberry32",
                           b"I'M FEELING GASSY", b"gassyChaos",
                           b"gassySave"):
                if marker not in body:
                    fails.append(f"GET /: missing {marker.decode()!r}")

        # issue #48: the /pads looper exports a flat 16*bars grid. Length-16
        # steps arrays must keep the legacy repeat-per-bar behaviour, and an
        # extended grid must place hits on their own steps.
        render_note = "render-sequence skipped (stems unavailable)"
        if stem_ids:
            render_note = "render-sequence len16/len32 grids ok"
            sid = "snare" if "snare" in stem_ids else stem_ids[0]

            def seq_tracks(steps, pitches=None):
                tr = {i: {"mute": True} for i in stem_ids}
                tr[sid] = {
                    "mute": False, "steps": steps,
                    "stepPitches": pitches if pitches is not None else [0] * len(steps),
                    "pitch": 0, "gain": 0.9, "pan": 0, "trimSec": 0.2,
                }
                return tr

            def render_wav(tracks, bars=2, bpm=116):
                body = json.dumps({"bpm": bpm, "bars": bars, "swing": 0,
                                   "tracks": tracks}).encode()
                st, _, b = post("/api/render-sequence", body, timeout=90)
                if st != 200:
                    return None, f"status {st}: {b[:160]!r}"
                p = json.loads(b)
                if not p.get("ok"):
                    return None, f"not ok: {p.get('error')}"
                wst, _, wb = get(p["wavUrl"])
                if wst != 200:
                    return None, f"wav fetch status {wst}"
                return wb, None

            def rms_at(wav_bytes, t, dur=0.05):
                """Mean-square level of the left channel over [t, t+dur) sec."""
                with wave.open(io.BytesIO(wav_bytes), "rb") as w:
                    sr, ch, nf = w.getframerate(), w.getnchannels(), w.getnframes()
                    i0 = max(0, min(int(t * sr), nf))
                    w.setpos(i0)
                    raw = w.readframes(max(0, min(int((t + dur) * sr), nf) - i0))
                vals = array.array("h")
                vals.frombytes(raw)
                vals = vals[0::ch]
                if not vals:
                    return 0.0
                return (sum(v * v for v in vals) / len(vals)) ** 0.5

            bpm, bars = 116, 2
            bar_sec = 4 * 60.0 / bpm
            step_sec = bar_sec / 16
            pat = [1] + [0] * 15                      # one hit, step 0
            wav_a, e1 = render_wav(seq_tracks(pat), bars, bpm)
            wav_b, e2 = render_wav(seq_tracks(pat * 2), bars, bpm)
            pat32 = pat + [0] * 4 + [1] + [0] * 11    # hits at steps 0 and 20
            wav_c, e3 = render_wav(seq_tracks(pat32), bars, bpm)
            err = e1 or e2 or e3
            if err:
                fails.append(f"POST /api/render-sequence: {err}")
            else:
                if wav_a != wav_b:
                    fails.append("render-sequence: length-16 grid did not "
                                 "repeat identically per bar")
                if wav_a == wav_c:
                    fails.append("render-sequence: length-32 grid ignored "
                                 "(identical to length-16 render)")
                else:
                    if rms_at(wav_c, bar_sec) > 100:
                        fails.append("render-sequence: extended-grid hit "
                                     "landed on the bar-2 downbeat")
                    if rms_at(wav_c, bar_sec + 4 * step_sec) < 500:
                        fails.append("render-sequence: no hit audible at "
                                     "extended step 20")

        status, ctype, body = post("/api/no-such-endpoint")
        try:
            payload = json.loads(body)
        except Exception:
            payload = None
        if status != 404:
            fails.append(f"POST /api/no-such-endpoint: status {status}")
        elif not isinstance(payload, dict) or payload.get("ok") is not False:
            fails.append("POST /api/no-such-endpoint: 404 but body is not "
                         '{"ok": false, ...}')

        # ---- #45: whole-studio patches ----------------------------------
        pname = "smoke test"
        pfile = os.path.join(ROOT, "build", "patches", pname + ".json")
        patch = {
            "v": 1, "name": pname,
            "seq": {"bpm": 120, "swing": 0.1, "bars": 4, "tracks": {}},
            "desk": {"master": 0.9, "channels": {}},
            "pads": [{"stem": "bomb", "semi": 0, "label": "BROWN BOMB"}],
            "key": None, "seed": None,
        }
        try:
            # save
            status, _, body = post("/api/patch", json.dumps(
                {"name": pname, "patch": patch}).encode())
            payload = json.loads(body) if body else {}
            if status != 200 or payload.get("ok") is not True:
                fails.append(f"POST /api/patch save: status {status} {payload!r}")
            elif not os.path.isfile(pfile):
                fails.append("POST /api/patch save: 200 but no file in build/patches/")

            # list
            status, _, body = get("/api/patches")
            names = []
            try:
                names = [p.get("name") for p in json.loads(body)]
            except Exception:
                pass
            if status != 200 or pname not in names:
                fails.append(f"GET /api/patches: status {status}, names {names!r}")

            # load round-trips the snapshot
            status, _, body = get("/api/patch?name=" +
                                  urllib.parse.quote(pname))
            try:
                got = json.loads(body)
            except Exception:
                got = {}
            if status != 200 or got.get("v") != 1 or \
                    got.get("seq", {}).get("bpm") != 120:
                fails.append(f"GET /api/patch?name=: status {status} {got!r}")

            # bad names 400 and write nothing outside build/patches/
            for bad_name in ("../x", "a/b", "", "x" * 65):
                status, _, _ = post("/api/patch", json.dumps(
                    {"name": bad_name, "patch": patch}).encode())
                if status != 400:
                    fails.append(f"POST /api/patch name {bad_name!r}: "
                                 f"status {status} (want 400)")
                status, _, _ = get("/api/patch?name=" +
                                   urllib.parse.quote(bad_name))
                if status != 400:
                    fails.append(f"GET /api/patch name {bad_name!r}: "
                                 f"status {status} (want 400)")
            if os.path.exists(os.path.join(ROOT, "build", "x.json")) or \
                    os.path.exists(os.path.join(ROOT, "x.json")):
                fails.append("patchy name escaped build/patches/")

            # non-v1 schema rejected
            status, _, _ = post("/api/patch", json.dumps(
                {"name": pname, "patch": {"v": 2}}).encode())
            if status != 400:
                fails.append(f"POST /api/patch v=2: status {status} (want 400)")

            # delete, then it's gone from list + load
            status, _, body = post("/api/patch", json.dumps(
                {"name": pname, "delete": True}).encode())
            payload = json.loads(body) if body else {}
            if status != 200 or payload.get("ok") is not True:
                fails.append(f"POST /api/patch delete: status {status} {payload!r}")
            status, _, body = get("/api/patches")
            try:
                names = [p.get("name") for p in json.loads(body)]
            except Exception:
                names = [pname]
            if pname in names:
                fails.append("GET /api/patches: deleted patch still listed")
            status, _, _ = get("/api/patch?name=" + urllib.parse.quote(pname))
            if status != 404:
                fails.append(f"GET /api/patch after delete: status {status} "
                             "(want 404)")
        finally:
            try:
                os.unlink(pfile)
            except OSError:
                pass

        # ---- #43: user samples --------------------------------------
        sname = "smoke poot"
        sslug = "smoke_poot"
        user_dir = os.path.join(ROOT, "build", "user")
        swav = os.path.join(user_dir, sslug + ".wav")
        sdef = os.path.join(user_dir, sslug + ".json")
        stem_id = None
        try:
            # upload a generated 0.5 s 44.1 kHz mono s16 WAV
            body_wav = wav_bytes()
            status, _, body = post_typed(
                "/api/upload-sample?name=" + urllib.parse.quote(sname),
                body_wav, "audio/wav")
            payload = json.loads(body) if body else {}
            if status != 200 or payload.get("user") is not True:
                fails.append(f"POST /api/upload-sample wav: status {status} "
                             f"{payload!r}")
            elif not payload.get("id") or not payload.get("url"):
                fails.append(f"upload: def missing id/url {payload!r}")
            elif not os.path.isfile(swav) or not os.path.isfile(sdef):
                fails.append("upload: 200 but build/user/<slug>.{wav,json} "
                             "missing")
            else:
                stem_id = payload["id"]
                if payload.get("trimSec") != 0.5:
                    fails.append(f"upload: trimSec {payload.get('trimSec')!r} "
                                 "(want 0.5)")

            # the saved wav is served at the def's url
            if stem_id:
                status, _, body = get(payload["url"])
                if status != 200 or body != body_wav:
                    fails.append(f"GET {payload['url']}: status {status}, "
                                 "or body mismatch")

            # /api/state lists it flagged user:true + a Sample crate —
            # 503 means stems can't build on this box (checked above)
            status, _, body = get("/api/state")
            st = json.loads(body) if body else {}
            if status == 200:
                stems = st.get("stems") or []
                match = [s for s in stems if s.get("id") == stem_id]
                if not match:
                    fails.append("/api/state: uploaded stem not listed")
                elif match[0].get("user") is not True:
                    fails.append("/api/state: uploaded stem lacks user:true")
                if stem_id:
                    crates = [t for t in st.get("tracks") or []
                              if t.get("kind") == "Sample"
                              and t.get("url") == payload["url"]]
                    if not crates:
                        fails.append("/api/state: no 'Sample' crate for upload")
            elif status != 503:
                fails.append(f"/api/state after upload: status {status}")

            # rejections: wrong rate / stereo / float / 8-bit / non-WAV
            for label, blob in (
                ("48 kHz", wav_bytes(rate=48000)),
                ("stereo", wav_bytes(ch=2)),
                ("float", float_wav()),
                ("8-bit", wav_bytes(width=1)),
                ("not a WAV", b"definitely not riff data"),
            ):
                status, _, body = post_typed(
                    "/api/upload-sample?name=bad" + label.replace(" ", "-"),
                    blob, "audio/wav")
                if status != 400:
                    fails.append(f"upload {label}: status {status} (want 400)")

            # pathy / over-long names 400
            for bad_name in ("../x", "a/b", "", "x" * 49, "name;rm"):
                status, _, _ = post_typed(
                    "/api/upload-sample?name=" + urllib.parse.quote(bad_name),
                    body_wav, "audio/wav")
                if status != 400:
                    fails.append(f"upload name {bad_name!r}: status {status} "
                                 "(want 400)")
            if os.path.exists(os.path.join(ROOT, "build", "x.wav")) or \
                    os.path.exists(os.path.join(ROOT, "x.wav")):
                fails.append("pathy sample name escaped build/user/")

            # declared Content-Length over the 10 MiB cap -> 413, answered
            # before the body lands (sending it would just broken-pipe)
            host, port = BASE.split("://", 1)[1].split(":", 1)
            conn = http.client.HTTPConnection(host, int(port), timeout=10)
            conn.putrequest("POST", "/api/upload-sample?name=toobig")
            conn.putheader("Content-Type", "audio/wav")
            conn.putheader("Content-Length", str(10 * 1024 * 1024 + 1))
            conn.endheaders()
            resp = conn.getresponse()
            status = resp.status
            resp.read()
            conn.close()
            if status != 413:
                fails.append(f"upload over-cap: status {status} (want 413)")

            # WAV body with a JSON content type is not an upload
            status, _, _ = post("/api/upload-sample",
                                json.dumps({"name": sname}).encode())
            if status != 400:
                fails.append(f"json without delete: status {status} "
                             "(want 400)")

            # delete removes the files and the /api/state entry
            status, _, body = post("/api/upload-sample", json.dumps(
                {"name": sname, "delete": True}).encode())
            payload = json.loads(body) if body else {}
            if status != 200 or payload.get("ok") is not True:
                fails.append(f"sample delete: status {status} {payload!r}")
            if os.path.exists(swav) or os.path.exists(sdef):
                fails.append("sample delete: files still in build/user/")
            status, _, body = get("/api/state")
            st = json.loads(body) if body else {}
            if status == 200 and \
                    any(s.get("id") == stem_id for s in st.get("stems") or []):
                fails.append("/api/state: deleted stem still listed")
            # deleting again 404s
            status, _, _ = post("/api/upload-sample", json.dumps(
                {"name": sname, "delete": True}).encode())
            if status != 404:
                fails.append(f"sample re-delete: status {status} (want 404)")
        finally:
            for p in (swav, sdef):
                try:
                    os.unlink(p)
                except OSError:
                    pass

        if fails:
            for f in fails:
                print(f"bad: {f}")
            return 1
        print("ok: / /mixer /pads /library serve HTML; "
              "/api/state sane; unknown POST 404s; "
              "patch save/list/load/delete round-trip + bad names 400; "
              "sample upload/list/delete round-trip + bad WAVs 400, "
              "over-cap 413; " + render_note)
        return 0
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


if __name__ == "__main__":
    sys.exit(main())
