#!/usr/bin/env python3
"""
studio_smoke.py — smoke test for scripts/studio.py (stdlib only).

Boots the studio server on a free loopback port, then checks:
  - GET /, /mixer, /pads, /library  -> 200 + text/html
  - GET /api/state                  -> 200 with stems, or 503 JSON if the
                                     stem build cannot run on this machine
  - POST /api/no-such-endpoint      -> 404 JSON {"ok": false, ...}
  - AUDIT (#63-#73): no/foreign Origin+Referer and missing
    X-Studio-Client 403 on /api GETs+POSTs (incl. absolute-form request
    lines), while a cross-site Referer on plain page GETs is fine;
    non-loopback Host 403s (incl. '[::1]junk'), nosniff+CSP headers,
    .git/source/listings 404 behind the static whitelist while real
    audio under build/tune/ and recordings/ serves 200 and a directory
    named like audio still 404s, over-cap/garbage/chunked/conflicting
    bodies get 413/400 (not a dropped connection), absurd bars clamp to
    64, render-preset names 400, pad labels with '<' or a trailing
    newline past the cap 400 while plain punctuation passes, and
    HEAD matches GET on the in-memory pages
  - PATCHES (#45): POST /api/patch saves a v1 snapshot under
    build/patches/, GET /api/patches lists it, GET /api/patch?name=
    returns it, delete removes it, and pathy/oversized/empty names
    plus non-v1 schemas 400 without writing outside build/patches/
  - SAMPLES (#43): POST /api/upload-sample stores a 44.1 kHz mono s16
    WAV + stem def under build/user/, /api/state lists it user:true,
    the wav is served, stereo/48 kHz/float/pathy/non-WAV uploads 400,
    over-cap bodies 413, and {name, delete:true} removes both files
  - LIBRARY (#53): GET /api/library returns the manifest + a resolved
    default; a conforming WAV dropped in build/library/ becomes a lib-*
    stem on rescan; a non-conforming drop stays catalog-only until
    promote normalizes it; intake 400s bad WAVs; deleting the default
    falls back to a playable stem and rescan can't resurrect it
  - POST /api/render-sequence       -> when stems are available: a length-16
                                     steps grid repeats identically every
                                     bar, while a length-16*bars grid places
                                     hits on their own steps (issue #48
                                     looper export)
  - SCALE QUANTIZE (#47): the shared JS block on / carries SCALES +
    q(); with node present its minor_penta table (incl. ties snapping
    down and octave wrap) is evaluated for real
  - JAM BRIDGE (#49): POST /api/jam against a fake kujamba spawns the
    child with creds in a 0600 config (not argv), tails its transcript
    for CONFIG (bpm/bpi) and RESULT lines, swaps build/jam/loop.wav to
    the server interval, rejects a second join, scrubs the password
    from status, and stop kills the child + wipes build/jam/.  A second
    server boot with a bogus KUJAMBA path must answer {ok:false}.

Prints "ok: ..." / "bad: ..." so tests/run_tests.sh can case-match the
verdict like the inline python3 heredocs do.  Exit 0 on pass, 1 on fail.
"""
import array
import http.client
import io
import json
import math
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import wave

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
BASE = None  # set in main()

# Stand-in for `kujamba join` (fart-app#98): parses the documented argv,
# reads creds from the --config file, writes a transcript (a CONFIG line,
# then RESULT counters including a deliberate password leak to prove the
# server scrubs its own status surfaces), then idles until killed.
FAKE_KUJAMBA = r'''#!/usr/bin/env python3
import os, re, sys, time

argv = sys.argv[1:]

def opt(flag):
    return argv[argv.index(flag) + 1] if flag in argv else None

# presets.sh also honors the KUJAMBA env var for stem synthesis — the
# render subcommand just has to emit a valid WAV at --out and exit.
if argv and argv[0] == "render":
    out = opt("--out")
    if out:
        import math, struct, wave
        with wave.open(out, "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(44100)
            w.writeframes(b"".join(
                struct.pack("<h", int(8000 * math.sin(
                    2 * math.pi * 220 * i / 44100)))
                for i in range(4410)))
    sys.exit(0)

transcript = opt("--transcript") or "transcript.log"
pidfile = os.environ.get("FAKE_KUJAMBA_PIDFILE")
if pidfile:
    with open(pidfile, "w") as f:
        f.write(str(os.getpid()))

pw = host = user = ""
cfg = opt("--config")
if cfg:
    try:
        with open(cfg) as f:
            for ln in f:
                m = re.match(r'\s*(\w+)\s*=\s*"(.*)"\s*$', ln)
                if m:
                    if m.group(1) == "host":
                        host = m.group(2)
                    elif m.group(1) == "user":
                        user = m.group(2)
                    elif m.group(1) == "pass":
                        pw = m.group(2)
    except OSError:
        pass

def log(line):
    with open(transcript, "a") as f:
        f.write(line + "\n")

need = ("--wav", "--wav-reload", "--pattern", "--reconnect",
        "--transcript", "--config")
args_ok = bool(argv) and argv[0] == "join" and all(f in argv for f in need)
argv_clean = bool(pw) and pw not in " ".join(argv)

log("C>S hello user=%s host=%s" % (user, host))
time.sleep(0.2)
log("S>C 0x02 CONFIG bpm=120 bpi=8")
time.sleep(0.3)
log("S>C 0x07 RESULT args_ok=%d argv_clean=%d intervals=1 note=%s"
    % (args_ok, argv_clean, pw))
time.sleep(0.6)
log("S>C 0x02 CONFIG bpm=120 bpi=6")
time.sleep(0.3)
log("S>C 0x07 RESULT args_ok=%d argv_clean=%d intervals=2 note=%s"
    % (args_ok, argv_clean, pw))
while True:
    time.sleep(0.5)
'''


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def get(path, headers=None):
    """-> (status, content-type, body bytes); HTTPError still returns data."""
    return get_to(BASE, path, headers=headers)


def post(path, body=b"{}", timeout=10, headers=None):
    return post_to(BASE, path, body, timeout, headers)


def _api_headers(extra):
    """The studio's own pages stamp X-Studio-Client on /api/* calls;
    the helpers send it everywhere so they behave like a same-origin
    page (#63)."""
    h = {"X-Studio-Client": "1"}
    h.update(extra or {})
    return h


def post_to(base, path, body=b"{}", timeout=10, headers=None):
    req = urllib.request.Request(
        base + path, data=body, method="POST",
        headers=_api_headers({"Content-Type": "application/json",
                              **(headers or {})}))
    try:
        r = urllib.request.urlopen(req, timeout=timeout)
        return r.status, r.headers.get("Content-Type", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", ""), e.read()


def get_to(base, path, timeout=10, headers=None):
    req = urllib.request.Request(base + path,
                                 headers=_api_headers(headers))
    try:
        r = urllib.request.urlopen(req, timeout=timeout)
        return r.status, r.headers.get("Content-Type", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", ""), e.read()


def post_typed(path, body, ctype):
    """POST with an explicit Content-Type (sample uploads are not JSON)."""
    req = urllib.request.Request(
        BASE + path, data=body, method="POST",
        headers=_api_headers({"Content-Type": ctype}))
    try:
        r = urllib.request.urlopen(req, timeout=10)
        return r.status, r.headers.get("Content-Type", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", ""), e.read()


def raw(path, method="GET", body=None, headers=None):
    """-> (status, headers, body): a request with EXACTLY these headers —
    for asserting the request gate rejects what's absent (#63)."""
    req = urllib.request.Request(BASE + path, data=body, method=method,
                                 headers=dict(headers or {}))
    try:
        r = urllib.request.urlopen(req, timeout=10)
        return r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


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
    fake_dir = tempfile.mkdtemp(prefix="fake-kujamba-")
    fake_bin = os.path.join(fake_dir, "kujamba")
    pidfile = os.path.join(fake_dir, "kujamba.pid")
    with open(fake_bin, "w") as f:
        f.write(FAKE_KUJAMBA)
    os.chmod(fake_bin, 0o755)
    env = dict(os.environ, STUDIO_PORT=str(port), STUDIO_HOST="127.0.0.1",
               KUJAMBA=fake_bin, FAKE_KUJAMBA_PIDFILE=pidfile)
    # A SIGKILLed earlier run can leave creds/transcript behind; boot must
    # wipe build/jam/ before it ever serves a byte.
    jam_dir = os.path.join(ROOT, "build", "jam")
    stale_cfg = os.path.join(jam_dir, "kujamba.toml")
    stale_log = os.path.join(jam_dir, "transcript.log")
    os.makedirs(jam_dir, exist_ok=True)
    for p, txt in ((stale_cfg, 'pass = "stale"\n'), (stale_log, "stale\n")):
        with open(p, "w") as f:
            f.write(txt)
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

        for p in (stale_cfg, stale_log):
            if os.path.exists(p):
                fails.append(
                    f"boot did not wipe stale build/jam/{os.path.basename(p)}")

        for path in ("/", "/mixer", "/pads", "/library", "/jam"):
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
                           b"gassySave", b'href="/jam"'):
                if marker not in body:
                    fails.append(f"GET /: missing {marker.decode()!r}")

        # ---- #44: Web MIDI input on /pads and /mixer -------------------
        # Pure client-side; assert the served pages carry the shared
        # wiring, the badge, and each page's note/CC map.
        status, _, body = get("/pads")
        if status == 200:
            for marker in (b"requestMIDIAccess", b'midi-badge" id="midiBadge"',
                           b"initMidi", b"midiBase", b"0x90",
                           b"hitPad(row * 4 + col, vel / 127)"):
                if marker not in body:
                    fails.append(f"GET /pads: missing {marker.decode()!r}")
        status, _, body = get("/mixer")
        if status == 200:
            for marker in (b"requestMIDIAccess", b'midi-badge" id="midiBadge"',
                           b"initMidi", b"num - 20", b"num - 30",
                           b"num === 7", b"hooks.cc"):
                if marker not in body:
                    fails.append(f"GET /mixer: missing {marker.decode()!r}")

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

        # ---- 2026-10-14 audit hardening (#63-#73) ---------------------
        # #63 CSRF: a cross-site "simple request" (text/plain, no custom
        # headers, foreign Origin) must 403 on POSTs AND on side-effect
        # GETs like /api/state, while the served pages' stamped header
        # passes.  urllib always sends a loopback Host here.
        for label, path, method, body, headers in (
            ("POST text/plain no hdr", "/api/patch", "POST",
             b'{"name":"x","patch":{"v":1}}',
             {"Content-Type": "text/plain"}),
            ("POST json no hdr", "/api/patch", "POST",
             b'{"name":"x","patch":{"v":1}}',
             {"Content-Type": "application/json"}),
            ("GET api no hdr", "/api/state", "GET", None, {}),
            ("POST foreign Origin", "/api/jam", "POST", b'{"stop": true}',
             {"Origin": "http://evil.example",
              "X-Studio-Client": "1",
              "Content-Type": "text/plain"}),
            ("GET foreign Referer", "/api/state", "GET", None,
             {"Referer": "http://evil.example/x",
              "X-Studio-Client": "1"}),
        ):
            status, _, _ = raw(path, method, body, headers)
            if status != 403:
                fails.append(f"csrf gate {label}: status {status} "
                             "(want 403)")
        # the stamped header reaches the handler even over a "simple"
        # request's text/plain body — the gate is the header, not the
        # Content-Type (bad patch name -> 400, not 403)
        status, _, _ = raw("/api/patch", "POST",
                           b'{"name":"../x","patch":{"v":1}}',
                           {"Content-Type": "text/plain",
                            "X-Studio-Client": "1"})
        if status != 400:
            fails.append(f"csrf gate same-origin POST: status {status} "
                         "(want 400)")
        # same-origin Origin/Referer values pass the gate
        status, _, _ = raw(
            "/api/state", "GET", None,
            {"Origin": BASE, "X-Studio-Client": "1"})
        if status not in (200, 503):
            fails.append(f"csrf gate same-origin Origin: {status}")
        # a non-loopback Host (the DNS-rebinding shape) 403s everywhere
        host, sport = BASE.split("://", 1)[1].split(":", 1)
        conn = http.client.HTTPConnection(host, int(sport), timeout=10)
        conn.putrequest("GET", "/api/state", skip_host=True)
        conn.putheader("Host", "evil.example.com")
        conn.putheader("X-Studio-Client", "1")
        conn.endheaders()
        resp = conn.getresponse()
        if resp.status != 403:
            fails.append(f"host gate: status {resp.status} (want 403)")
        resp.read()
        conn.close()
        # #72: hardening headers ride on every response
        _, hdrs, _ = raw("/", "GET", None, {})
        if hdrs.get("X-Content-Type-Options") != "nosniff":
            fails.append("GET /: missing X-Content-Type-Options nosniff")
        if "default-src 'self'" not in \
                (hdrs.get("Content-Security-Policy") or ""):
            fails.append("GET /: missing Content-Security-Policy")

        # #65: static serving is a whitelist — repo source, dotfiles,
        # manifests, and directory listings must all 404
        for path in ("/.git/config", "/.gitignore", "/scripts/studio.py",
                     "/scripts/", "/docs/", "/build/library.json",
                     "/build/tune/", "/Makefile", "/README.md"):
            status, _, _ = get(path)
            if status != 404:
                fails.append(f"static whitelist GET {path}: {status} "
                             "(want 404)")

        # the whitelist must also SERVE: a wav under a two-level dir and
        # under recordings/ — a single-level SERVE_DIRS entry the first
        # cut of the prefix check missed — both come back audio + 200
        rec_dir = os.path.join(ROOT, "recordings")
        rec_existed = os.path.isdir(rec_dir)
        serve_wavs = ("build/tune/smoke_serve.wav",
                      "recordings/smoke_serve.wav")
        try:
            for rel in serve_wavs:
                wp = os.path.join(ROOT, rel)
                os.makedirs(os.path.dirname(wp), exist_ok=True)
                with open(wp, "wb") as f:
                    f.write(wav_bytes())
                status, ctype, body = get("/" + rel)
                if status != 200 or not body.startswith(b"RIFF"):
                    fails.append(f"serve whitelist GET /{rel}: {status} "
                                 "(want 200 + wav)")
                elif not ctype.startswith("audio/"):
                    fails.append(f"serve whitelist GET /{rel}: "
                                 f"content-type {ctype!r}")
            # a DIRECTORY named like audio is not a file — no index.html,
            # no listing (#65 regression: build/library/pack.wav/)
            pack_dir = os.path.join(ROOT, "build", "library", "pack.wav")
            os.makedirs(pack_dir, exist_ok=True)
            try:
                with open(os.path.join(pack_dir, "index.html"), "w") as f:
                    f.write("<script>alert(1)</script>")
                for path in ("/build/library/pack.wav/",
                             "/build/library/pack.wav/index.html"):
                    status, _, body = get(path)
                    if status != 404:
                        fails.append(f"dir-as-audio GET {path}: {status} "
                                     "(want 404)")
                    elif b"alert(1)" in body:
                        fails.append(f"GET {path} served planted html")
            finally:
                shutil.rmtree(pack_dir, ignore_errors=True)
        finally:
            for rel in serve_wavs:
                try:
                    os.unlink(os.path.join(ROOT, rel))
                except OSError:
                    pass
            if not rec_existed:
                try:
                    os.rmdir(rec_dir)
                except OSError:
                    pass

        # the request gate reads the PARSED path — an absolute-form
        # request line must not skip the /api header check
        conn = http.client.HTTPConnection(host, int(sport), timeout=10)
        conn.putrequest("GET", f"{BASE}/api/jam", skip_host=True)
        conn.putheader("Host", f"{host}:{sport}")
        conn.endheaders()
        resp = conn.getresponse()
        if resp.status != 403:
            fails.append(f"absolute-form /api/jam: {resp.status} "
                         "(want 403)")
        resp.read()
        conn.close()
        # a cross-site Referer on a side-effect-free page GET is fine —
        # the Origin/Referer check only guards /api/*
        status, _, _ = raw("/", "GET", None,
                           {"Referer": "http://evil.example/x"})
        if status != 200:
            fails.append(f"cross-site Referer GET /: {status} (want 200)")
        # '[::1]junk' is not an IPv6 literal with a port — reject it
        conn = http.client.HTTPConnection(host, int(sport), timeout=10)
        conn.putrequest("GET", "/api/state", skip_host=True)
        conn.putheader("Host", "[::1]junk")
        conn.putheader("X-Studio-Client", "1")
        conn.endheaders()
        resp = conn.getresponse()
        if resp.status != 403:
            fails.append(f"Host '[::1]junk': {resp.status} (want 403)")
        resp.read()
        conn.close()
        # HEAD answers like GET for the in-memory pages (sans body)
        status, _, body = raw("/", "HEAD", None, {})
        if status != 200:
            fails.append(f"HEAD /: {status} (want 200)")
        elif body:
            fails.append("HEAD / returned a body")

        # #68: over-cap, garbage, and chunked bodies are answered, not
        # dropped; render endpoints carry the shared 1 MiB cap
        for label, hdrs_extra in (
            ("over-cap", {"Content-Length": str(1024 * 1024 + 1)}),
            ("bad length", {"Content-Length": "abc"}),
            ("underscore length", {"Content-Length": "1_0"}),
            ("signed length", {"Content-Length": "+2"}),
            ("chunked", {"Transfer-Encoding": "chunked"}),
        ):
            conn = http.client.HTTPConnection(host, int(sport), timeout=10)
            conn.putrequest("POST", "/api/render-sequence")
            conn.putheader("Content-Type", "application/json")
            conn.putheader("X-Studio-Client", "1")
            for k, v in hdrs_extra.items():
                conn.putheader(k, v)
            if "Content-Length" not in hdrs_extra:
                conn.endheaders(b"0\r\n\r\n")
            else:
                conn.endheaders()
            try:
                resp = conn.getresponse()
                want = 413 if label == "over-cap" else 400
                if resp.status != want:
                    fails.append(f"body gate {label}: {resp.status} "
                                 f"(want {want})")
                resp.read()
            except Exception as e:
                fails.append(f"body gate {label}: no response ({e})")
            finally:
                conn.close()

        # duplicate Content-Length fields that disagree must 400
        conn = http.client.HTTPConnection(host, int(sport), timeout=10)
        conn.putrequest("POST", "/api/render-sequence")
        conn.putheader("Content-Type", "application/json")
        conn.putheader("X-Studio-Client", "1")
        conn.putheader("Content-Length", "2")
        conn.putheader("Content-Length", "5")
        conn.endheaders()
        try:
            resp = conn.getresponse()
            if resp.status != 400:
                fails.append(f"conflicting Content-Length: {resp.status} "
                             "(want 400)")
            resp.read()
        except Exception as e:
            fails.append(f"conflicting Content-Length: no response ({e})")
        finally:
            conn.close()

        # #64: an absurd bars is clamped, not honored — all-muted tracks
        # render silence so this exercises the clamp without the mix
        status, _, body = post("/api/render-sequence", json.dumps({
            "bpm": 240, "bars": 10000000, "swing": 0,
            "tracks": {i: {"mute": True}
                       for i in ("kick", "snare", "hat",
                                 "wah", "lead", "bomb")},
        }).encode(), timeout=60)
        p = json.loads(body) if body else {}
        want_sec = 64 * 1.0 + 1.8       # 64 bars @ 240 BPM + tail
        if status != 200 or abs((p.get("durationSec") or 0) - want_sec) \
                > 0.01:
            fails.append(f"bars clamp: status {status} "
                         f"durationSec {p.get('durationSec')!r} "
                         f"(want {want_sec})")

        # #73: render-preset names are validated before any path is built
        for bad in ("../escape", "a/b", "UPPER", "funk;rm", "x" * 33):
            status, _, body = post("/api/render-preset", json.dumps(
                {"mode": "song", "name": bad}).encode())
            if status != 400:
                fails.append(f"render-preset name {bad!r}: {status} "
                             "(want 400)")

        # #66: stored XSS via a patch pad label 400s server-side
        status, _, body = post("/api/patch", json.dumps({
            "name": "xss pads", "patch": {"v": 1, "pads": [
                {"stem": "bomb", "semi": 0,
                 "label": "<img src=x onerror=alert(1)>"}]}}).encode())
        if status != 400:
            fails.append(f"patch pad html label: status {status} "
                         "(want 400)")
        # labels render via textContent — plain punctuation is fine,
        # but '<' still 400s and a trailing newline can't smuggle a
        # 65th char past the cap (fullmatch, not $-anchored match)
        for label_txt, want in (("R&B", 200), ("Mom's pad", 200),
                                ("x" * 64, 200), ("x" * 64 + "\n", 400),
                                ("a<b", 400)):
            status, _, body = post("/api/patch", json.dumps({
                "name": "smoke labels", "patch": {"v": 1, "pads": [
                    {"stem": "bomb", "semi": 0,
                     "label": label_txt}]}}).encode())
            if status != want:
                fails.append(f"patch pad label {label_txt!r}: {status} "
                             f"(want {want})")
        post("/api/patch", json.dumps(
            {"name": "smoke labels", "delete": True}).encode())
        try:
            os.unlink(os.path.join(ROOT, "build", "patches",
                                   "smoke labels.json"))
        except OSError:
            pass

        # ---- #47: scale quantize ------------------------------------
        # The q() helper lives in the shared STUDIO_JS block served on
        # every page; slice it out of / and run the issue's worked
        # examples under node when node is on this machine.
        status, _, body = get("/")
        m = re.search(
            rb"// =+ scale-quantize block start \(#47\) =+\n(.*?)"
            rb"// =+ scale-quantize block end =+",
            body, re.S)
        if status != 200 or not m:
            fails.append("GET /: shared scale-quantize block missing")
        else:
            block = m.group(1).decode("utf-8", "replace")
            for tok in ("SCALES", "PITCHED", "function q("):
                if tok not in block:
                    fails.append(f"scale-quantize block: missing {tok!r}")
            node = shutil.which("node")
            if node:
                script = block + """
KEY = {scale: 'minor_penta', root: 0, q: true};
const got = [1, 2, 6, 11, -1, -5, 14].map(q);
if (JSON.stringify(got) !== '[0,3,5,10,-2,-5,15]')
  throw new Error('q table ' + JSON.stringify(got));
KEY = {scale: 'major', root: 2, q: true};
if (q(3) !== 2) throw new Error('major root+2 q(3)=' + q(3));
KEY = {scale: 'chromatic', root: 0, q: true};
if (q(9) !== 9) throw new Error('chromatic not identity');
"""
                r = subprocess.run([node, "-e", script],
                                   capture_output=True, text=True, timeout=15)
                if r.returncode != 0:
                    fails.append("node q() table: "
                                 + (r.stderr.strip() or r.stdout.strip()))

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
            conn.putheader("X-Studio-Client", "1")
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

        # ---- #49: NINJAM jam bridge ----------------------------------
        # The server was booted with KUJAMBA pointing at the fake (top of
        # file); a second boot with a bogus path covers missing-binary.
        jam_cfg = os.path.join(jam_dir, "kujamba.toml")
        jam_log = os.path.join(jam_dir, "transcript.log")
        jam_wav = os.path.join(jam_dir, "loop.wav")
        jam_pass = "s3cret!fart"

        def jam_get(want=None, timeout=40):
            """Poll GET /api/jam until want(payload) or the timeout."""
            deadline = time.time() + timeout
            last = {}
            while time.time() < deadline:
                scode, _, jb = get("/api/jam")
                try:
                    last = json.loads(jb)
                except Exception:
                    last = {}
                if want is None or want(last):
                    return last
                time.sleep(0.3)
            return last

        status, _, body = get("/api/jam")
        st = json.loads(body) if body else {}
        if status != 200 or st.get("ok") is not True or \
                st.get("state") != "off" or st.get("kujamba") is not True:
            fails.append(f"GET /api/jam idle: {status} {st!r}")

        # malformed joins 400 before anything is spawned or written
        for label, jb in (
            ("non-dict", b"[]"),
            ("bad host", json.dumps(
                {"host": "h;rm -rf", "user": "u", "pass": "p"}).encode()),
            ("empty user", json.dumps(
                {"host": "h", "user": "", "pass": "p"}).encode()),
            ("non-string pass", json.dumps(
                {"host": "h", "user": "u", "pass": 7}).encode()),
            ("empty join", b"{}"),
        ):
            status, _, body = post("/api/jam", jb)
            p = json.loads(body) if body else {}
            if status != 400 or p.get("ok") is not False:
                fails.append(f"jam {label}: status {status} {p!r}")
        if os.path.exists(jam_cfg):
            fails.append("jam: rejected joins still wrote kujamba.toml")

        # join: spawns the child + the 0600 creds file
        jam_seq = {"bpm": 116, "bars": 4, "swing": 0, "tracks": {
            i: {"mute": True}
            for i in ("kick", "snare", "hat", "wah", "lead", "bomb")}}
        if "snare" in stem_ids:
            jam_seq["tracks"]["snare"] = {
                "mute": False, "steps": [1] + [0] * 15,
                "stepPitches": [0] * 16,
                "pitch": 0, "gain": 0.9, "pan": 0, "trimSec": 0.2}
        status, _, body = post("/api/jam", json.dumps({
            "host": "nin.example.org:2049", "user": "stinker",
            "pass": jam_pass, "seq": jam_seq}).encode())
        p = json.loads(body) if body else {}
        if status != 200 or p.get("ok") is not True:
            fails.append(f"jam join: status {status} {p!r}")
        if jam_pass.encode() in body:
            fails.append("jam join: response echoes the password")

        st = jam_get(
            lambda s: s.get("state") == "live" and s.get("resultLine"),
            timeout=30)
        if st.get("bpm") != 120 or st.get("bpi") != 8:
            fails.append(f"jam: CONFIG not tailed: {st!r}")
        # status exposes only the scrubbed RESULT line — no parsed
        # 'result' dict the password could hide inside (#70)
        if "result" in st:
            fails.append("jam: status still carries a 'result' dict")
        line = st.get("resultLine") or ""
        if "args_ok=1" not in line:
            fails.append(f"jam: fake kujamba argv wrong: {line!r}")
        if "argv_clean=1" not in line:
            fails.append("jam: password found on the child's argv")
        if "note=***" not in line:
            fails.append(f"jam: leaked password not scrubbed: {line!r}")
        if jam_pass in json.dumps(st):
            fails.append("jam: password in /api/jam status JSON")

        # the creds file is the only place the password may persist
        if not os.path.isfile(jam_cfg):
            fails.append("jam: no 0600 kujamba.toml written")
        else:
            mode = os.stat(jam_cfg).st_mode & 0o777
            if mode != 0o600:
                fails.append(f"jam: kujamba.toml mode {mode:o} (want 600)")
            cfg_txt = open(jam_cfg).read()
            if jam_pass not in cfg_txt or "stinker" not in cfg_txt:
                fails.append("jam: kujamba.toml missing creds")
        for fn in (os.listdir(jam_dir) if os.path.isdir(jam_dir) else []):
            if fn in ("kujamba.toml", "transcript.log"):
                continue   # the transcript is kujamba's own log
            pth = os.path.join(jam_dir, fn)
            if os.path.isfile(pth) and \
                    jam_pass.encode() in open(pth, "rb").read():
                fails.append(f"jam: password in build/jam/{fn}")

        # build/jam/ is not served over HTTP: the creds file and the
        # transcript exist on disk right now yet must 404 (incl. dot and
        # %-encoded path tricks), while the rendered loop stays public.
        for path in ("/build/jam/kujamba.toml",
                     "/build/jam/transcript.log",
                     "/build/jam",
                     "/build/jam/../jam/kujamba.toml",
                     "/%62uild/jam/kujamba.toml"):
            status, _, body = get(path)
            if status != 404:
                fails.append(f"jam: GET {path} -> {status} (want 404)")
            elif jam_pass.encode() in body:
                fails.append(f"jam: GET {path} leaks the password")

        # a second join while live must not spawn another child
        status, _, body = post("/api/jam", json.dumps({
            "host": "other:2049", "user": "x", "pass": "y"}).encode())
        p = json.loads(body) if body else {}
        if status != 409 or p.get("ok") is not False:
            fails.append(f"jam duplicate join: status {status} {p!r}")

        # loop.wav: 44.1 kHz stereo s16 sized to the interval — bpi=8 is
        # 4.0 s; the fake's second CONFIG (bpi=6, not a multiple of 4)
        # must re-render to exactly 3.0 s.
        if stem_ids:
            st = jam_get(lambda s: s.get("loopSec") == 4.0, timeout=60)
            if st.get("loopSec") != 4.0 or not os.path.isfile(jam_wav):
                fails.append(f"jam: no loop.wav after CONFIG: {st!r}")
            else:
                with wave.open(jam_wav, "rb") as w:
                    if (w.getframerate(), w.getnchannels(),
                            w.getsampwidth()) != (44100, 2, 2):
                        fails.append("jam: loop.wav not 44.1k stereo s16")
                    elif w.getnframes() != 176400:
                        fails.append(f"jam: loop.wav {w.getnframes()} "
                                     "frames (want 176400)")
            st = jam_get(lambda s: s.get("loopSec") == 3.0, timeout=60)
            if st.get("loopSec") != 3.0:
                fails.append(f"jam: bpi=6 re-render missing: {st!r}")
            else:
                with wave.open(jam_wav, "rb") as w:
                    if w.getnframes() != 132300:
                        fails.append(f"jam: bpi=6 loop {w.getnframes()} "
                                     "frames (want 132300)")
            if os.path.isfile(jam_wav):
                status, _, body = get("/build/jam/loop.wav")
                if status != 200 or not body.startswith(b"RIFF"):
                    fails.append(f"jam: GET loop.wav -> {status}")
            leftovers = [f for f in os.listdir(jam_dir)
                         if f.startswith(".tmp-")]
            if leftovers:
                fails.append(f"jam: tmp files left behind: {leftovers}")

        # pushing a new seq while live re-renders the loop
        status, _, body = post("/api/jam", json.dumps(
            {"seq": jam_seq}).encode(), timeout=60)
        p = json.loads(body) if body else {}
        if status != 200 or p.get("ok") is not True or \
                p.get("state") != "live":
            fails.append(f"jam seq push: status {status} {p!r}")

        # stop: the child dies and build/jam/ is wiped incl. the creds file
        status, _, body = post("/api/jam", b'{"stop": true}')
        p = json.loads(body) if body else {}
        if status != 200 or p.get("state") != "off":
            fails.append(f"jam stop: status {status} {p!r}")
        fake_pid = None
        try:
            fake_pid = int(open(pidfile).read().strip())
        except Exception:
            fails.append("jam: fake kujamba never wrote its pidfile")
        if fake_pid:
            deadline = time.time() + 5
            while time.time() < deadline:
                try:
                    os.kill(fake_pid, 0)
                except OSError:
                    break
                time.sleep(0.2)
            else:
                fails.append("jam: stop did not kill the kujamba child")
        for gone in (jam_cfg, jam_log, jam_wav):
            if os.path.exists(gone):
                fails.append(f"jam stop: {os.path.basename(gone)} survived")

        # missing binary: a separate boot with a bogus KUJAMBA path
        port2 = free_port()
        base2 = f"http://127.0.0.1:{port2}"
        env2 = dict(os.environ, STUDIO_PORT=str(port2),
                    STUDIO_HOST="127.0.0.1",
                    KUJAMBA=os.path.join(fake_dir, "no-such-kujamba"))
        proc2 = subprocess.Popen(
            [sys.executable, os.path.join(ROOT, "scripts", "studio.py")],
            cwd=ROOT, env=env2,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            if not wait_ready(proc2, port2):
                fails.append("jam: missing-binary studio did not boot")
            else:
                status, _, body = post_to(base2, "/api/jam", json.dumps({
                    "host": "h", "user": "u", "pass": "p"}).encode())
                p = json.loads(body) if body else {}
                if status != 503 or p.get("ok") is not False:
                    fails.append(f"jam missing kujamba: {status} {p!r}")
                status, _, body = get_to(base2, "/api/jam")
                p = json.loads(body) if body else {}
                if status != 200 or p.get("kujamba") is not False:
                    fails.append(
                        f"jam missing kujamba status: {status} {p!r}")
        finally:
            proc2.terminate()
            try:
                proc2.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc2.kill()
                proc2.wait()

        # ---- #53: flatulence library -----------------------------------
        # build/library.json is the catalog; build/library/ is the intake
        # dir.  A conforming WAV dropped there becomes a lib-* stem on
        # rescan, a non-conforming one is cataloged raw until promote
        # normalizes it, and the manifest's default falls back cleanly
        # when its stem is deleted.
        lib_dir = os.path.join(ROOT, "build", "library")
        os.makedirs(lib_dir, exist_ok=True)
        lwav = os.path.join(lib_dir, "smoke_drop.wav")
        bwav = os.path.join(lib_dir, "smoke_bad.wav")
        ldef = os.path.join(lib_dir, "lib-smoke-drop.json")
        bdef = os.path.join(lib_dir, "lib-smoke-bad.json")
        bwav_norm = os.path.join(lib_dir, "lib-smoke-bad.wav")
        iwav = os.path.join(lib_dir, "smoke-intake.wav")
        idef = os.path.join(lib_dir, "lib-smoke-intake.json")
        cwav = os.path.join(lib_dir, "smoke_clobber.wav")
        zwav = os.path.join(lib_dir, "lib-zep-take.wav")
        zdef = os.path.join(lib_dir, "lib-zep-take.json")
        twav = os.path.join(ROOT, "build", "user", "smoke_trunc.wav")
        tdef = os.path.join(ROOT, "build", "user", "smoke_trunc.json")
        lib_files = (lwav, bwav, ldef, bdef, bwav_norm, iwav, idef,
                     cwav, zwav, zdef, twav, tdef)

        def unretired(r):
            return not str(r).startswith("lib-smoke-") \
                and r != "lib-zep-take"

        prev_default = None
        lib_manifest = os.path.join(ROOT, "build", "library.json")

        def lib_entries():
            st_, _, b_ = get("/api/library")
            try:
                m_ = json.loads(b_)
            except Exception:
                m_ = {}
            return st_, m_

        def find_entry(man, **kw):
            for e in man.get("entries") or []:
                if all(e.get(k) == v for k, v in kw.items()):
                    return e
            return None

        try:
            status, _, body = get("/api/library")
            man = json.loads(body) if body else {}
            if status == 200 and isinstance(man, dict):
                prev_default = man.get("defaultSet")
            # retire tombstones persist in the manifest — clear ours so a
            # previous run can't poison this one
            for p in lib_files:
                try:
                    os.unlink(p)
                except OSError:
                    pass
            try:
                with open(lib_manifest, "r", encoding="utf-8") as f:
                    m0 = json.load(f)
                m0["retired"] = [
                    r for r in m0.get("retired") or [] if unretired(r)]
                with open(lib_manifest, "w", encoding="utf-8") as f:
                    f.write(json.dumps(m0))
            except Exception:
                pass

            if status != 200 or man.get("ok") is not True \
                    or not isinstance(man.get("entries"), list):
                fails.append(f"GET /api/library: status {status} {man!r}")
            else:
                if stem_ids:
                    by_stem = {e.get("stem") for e in man["entries"]}
                    for sid in stem_ids:
                        if sid not in by_stem:
                            fails.append(f"/api/library: stem {sid!r} "
                                         "not backed by an entry")
                    if man.get("default") not in stem_ids:
                        fails.append("/api/library: resolved default "
                                     f"{man.get('default')!r} not playable")

            # conforming WAV dropped into the intake dir -> stem on rescan
            with open(lwav, "wb") as f:
                f.write(wav_bytes())
            status, _, body = post("/api/library/rescan")
            payload = json.loads(body) if body else {}
            if status != 200 or payload.get("ok") is not True:
                fails.append(f"POST /api/library/rescan: status {status} "
                             f"{payload!r}")
            status, man = lib_entries()
            e = find_entry(man, file="build/library/smoke_drop.wav")
            if not e or e.get("stem") != "lib-smoke-drop":
                fails.append(f"intake drop not promoted: {e!r}")
            elif not os.path.isfile(ldef):
                fails.append("intake: stem def file missing")
            status, _, body = get("/api/state")
            st = json.loads(body) if body else {}
            if status == 200 and not any(
                    s.get("id") == "lib-smoke-drop"
                    for s in st.get("stems") or []):
                fails.append("/api/state: intake stem not playable")

            # non-conforming drop catalogs raw; promote normalizes it
            with open(bwav, "wb") as f:
                f.write(wav_bytes(rate=48000, ch=2))
            post("/api/library/rescan")
            _, man = lib_entries()
            e = find_entry(man, file="build/library/smoke_bad.wav")
            if not e or e.get("stem") is not None \
                    or e.get("conform") is not False:
                fails.append(f"non-conforming drop should stay raw: {e!r}")
            else:
                status, _, body = post("/api/library/promote", json.dumps(
                    {"id": e["id"]}).encode())
                payload = json.loads(body) if body else {}
                stem = payload.get("stem") or {}
                if status != 200 or stem.get("id") != "lib-smoke-bad":
                    fails.append(f"promote: status {status} {payload!r}")
                elif not os.path.isfile(bwav_norm) \
                        or not os.path.isfile(bdef):
                    fails.append("promote: normalized wav/def missing")
                else:
                    with wave.open(bwav_norm, "rb") as w:
                        if (w.getframerate(), w.getnchannels(),
                                w.getsampwidth()) != (44100, 1, 2):
                            fails.append("promote: normalized wav not "
                                         "44.1k/mono/s16")

            # promote rejects unknown ids and keeps name validation
            status, _, _ = post("/api/library/promote", json.dumps(
                {"id": "no-such-entry"}).encode())
            if status != 404:
                fails.append(f"promote unknown: status {status} (want 404)")
            _, man = lib_entries()
            mp3 = next((x for x in man.get("entries") or []
                        if x.get("file", "").endswith(".mp3")), None)
            if mp3:
                status, _, _ = post("/api/library/promote", json.dumps(
                    {"id": mp3["id"]}).encode())
                if status != 400:
                    fails.append(f"promote mp3: status {status} (want 400)")

            # pick the dropped fart as THE default; /api/state agrees
            status, _, body = post("/api/library/default", json.dumps(
                {"id": "lib-smoke-drop"}).encode())
            payload = json.loads(body) if body else {}
            if status != 200 or payload.get("default") != "lib-smoke-drop":
                fails.append(f"set default: status {status} {payload!r}")
            status, _, body = get("/api/state")
            st = json.loads(body) if body else {}
            if status == 200 and st.get("default") != "lib-smoke-drop":
                fails.append("/api/state: default does not follow "
                             f"manifest ({st.get('default')!r})")
            status, _, _ = post("/api/library/default", json.dumps(
                {"id": "not-a-stem"}).encode())
            if status != 400:
                fails.append(f"set default bogus: status {status} (want 400)")

            # delete the default fart -> resolved default falls back,
            # retired so rescan can't resurrect it, source wav cataloged
            status, _, body = post("/api/upload-sample", json.dumps(
                {"id": "lib-smoke-drop", "delete": True}).encode())
            payload = json.loads(body) if body else {}
            if status != 200 or payload.get("ok") is not True:
                fails.append(f"stem delete by id: status {status} "
                             f"{payload!r}")
            _, man = lib_entries()
            if man.get("default") == "lib-smoke-drop":
                fails.append("deleted default still resolves")
            elif stem_ids and man.get("default") not in stem_ids:
                fails.append("fallback default is not a playable stem")
            e = find_entry(man, file="build/library/smoke_drop.wav")
            if e and e.get("stem") is not None:
                fails.append("deleted stem still attached to its entry")
            post("/api/library/rescan")
            _, man = lib_entries()
            e = find_entry(man, file="build/library/smoke_drop.wav")
            if e and e.get("stem") is not None:
                fails.append("rescan resurrected a retired stem")
            # the tombstone must persist on disk, and a same-name drop
            # afterwards still can't resurrect the retired id
            try:
                with open(lib_manifest, "r", encoding="utf-8") as f:
                    mm = json.load(f)
                if "lib-smoke-drop" not in (mm.get("retired") or []):
                    fails.append("deleted stem id missing from manifest "
                                 "retired set")
            except Exception as ex:
                fails.append(f"manifest unreadable after delete: {ex}")
            with open(lwav, "wb") as f:
                f.write(wav_bytes())
            post("/api/library/rescan")
            _, man = lib_entries()
            e = find_entry(man, file="build/library/smoke_drop.wav")
            if e and e.get("stem") is not None:
                fails.append("same-name drop resurrected a retired stem")
            if os.path.isfile(ldef):
                fails.append("retired stem def recreated by rescan")

            # HTTP intake: conforming WAV -> stem, bad shapes 400
            status, _, body = post_typed(
                "/api/library/intake?name=smoke+intake",
                wav_bytes(), "audio/wav")
            payload = json.loads(body) if body else {}
            stem = payload.get("stem") or {}
            if status != 200 or stem.get("id") != "lib-smoke-intake":
                fails.append(f"intake wav: status {status} {payload!r}")
            for label, blob in (
                ("48 kHz", wav_bytes(rate=48000)),
                ("stereo", wav_bytes(ch=2)),
                ("not a WAV", b"definitely not riff data"),
            ):
                status, _, _ = post_typed(
                    "/api/library/intake?name=bad-" + label.replace(" ", "-"),
                    blob, "audio/wav")
                if status != 400:
                    fails.append(f"intake {label}: status {status} "
                                 "(want 400)")
            status, _, _ = post_typed(
                "/api/library/intake?name=..%2Fx",
                wav_bytes(), "audio/wav")
            if status != 400:
                fails.append(f"intake pathy name: status {status} (want 400)")

            # a WAV whose header lies about the frame count is a 400 on
            # both upload surfaces — never a broken stem
            trunc = wav_bytes()[:-256]
            for path in ("/api/upload-sample?name=smoke-trunc",
                         "/api/library/intake?name=smoke-trunc"):
                status, _, _ = post_typed(path, trunc, "audio/wav")
                if status != 400:
                    fails.append(f"truncated wav {path}: status {status} "
                                 "(want 400)")

            # intake must not clobber a stem-owned wav: promote a
            # non-conforming drop so its normalized file sits at
            # lib-zep-take.wav, then an intake named "lib zep take"
            # targets that same path under a different stem id
            with open(cwav, "wb") as f:
                f.write(wav_bytes(rate=48000, ch=2))
            post("/api/library/rescan")
            _, man = lib_entries()
            e = find_entry(man, file="build/library/smoke_clobber.wav")
            if not e:
                fails.append("clobber setup: non-conforming drop not "
                             "cataloged")
            else:
                status, _, body = post("/api/library/promote", json.dumps(
                    {"id": e["id"], "name": "zep take"}).encode())
                payload = json.loads(body) if body else {}
                if status != 200 or (payload.get("stem") or {}).get("id") \
                        != "lib-zep-take":
                    fails.append(f"clobber setup promote: {status} "
                                 f"{payload!r}")
                else:
                    with open(zwav, "rb") as f:
                        owned = f.read()
                    status, _, body = post_typed(
                        "/api/library/intake?name=lib+zep+take",
                        wav_bytes(), "audio/wav")
                    payload = json.loads(body) if body else {}
                    if status != 409 or payload.get("ok") is not False:
                        fails.append(f"intake onto owned slug: status "
                                     f"{status} {payload!r} (want 409)")
                    with open(zwav, "rb") as f:
                        if f.read() != owned:
                            fails.append("intake clobbered a stem-owned "
                                         "wav")

            # the pads page boots pad 1 off the manifest default
            status, _, body = get("/pads")
            if status == 200 and b"studioPadsBoot" not in body:
                fails.append("GET /pads: manifest default not wired into "
                             "boot")
        finally:
            for p in lib_files:
                try:
                    os.unlink(p)
                except OSError:
                    pass
            try:
                post("/api/library/default", json.dumps(
                    {"id": prev_default}).encode())
                post("/api/library/rescan")
                with open(lib_manifest, "r", encoding="utf-8") as f:
                    m0 = json.load(f)
                m0["retired"] = [
                    r for r in m0.get("retired") or [] if unretired(r)]
                with open(lib_manifest, "w", encoding="utf-8") as f:
                    f.write(json.dumps(m0))
            except Exception:
                pass

        if fails:
            for f in fails:
                print(f"bad: {f}")
            return 1
        print("ok: / /mixer /pads /library /jam serve HTML; "
              "/api/state sane; unknown POST 404s; "
              "patch save/list/load/delete round-trip + bad names 400; "
              "sample upload/list/delete round-trip + bad WAVs 400, "
              "over-cap 413; jam join/CONFIG/RESULT/stop + missing-binary "
              "clean against fake kujamba; library manifest/rescan/"
              "intake/promote/default round-trip, retired stems stay "
              "retired; " + render_note)
        return 0
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        # belt & suspenders: a leaked fake kujamba would idle forever
        try:
            os.kill(int(open(pidfile).read().strip()), 9)
        except Exception:
            pass
        shutil.rmtree(fake_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
