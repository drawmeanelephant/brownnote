#!/usr/bin/env python3
"""
studio_smoke.py — smoke test for scripts/studio.py (stdlib only).

Boots the studio server on a free loopback port, then checks:
  - GET /, /mixer, /pads, /library  -> 200 + text/html
  - GET /api/state                  -> 200 with stems, or 503 JSON if the
                                     stem build cannot run on this machine
  - POST /api/no-such-endpoint      -> 404 JSON {"ok": false, ...}

Prints "ok: ..." / "bad: ..." so tests/run_tests.sh can case-match the
verdict like the inline python3 heredocs do.  Exit 0 on pass, 1 on fail.
"""
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

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


def post(path, body=b"{}"):
    req = urllib.request.Request(
        BASE + path, data=body, method="POST",
        headers={"Content-Type": "application/json"})
    try:
        r = urllib.request.urlopen(req, timeout=10)
        return r.status, r.headers.get("Content-Type", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", ""), e.read()


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
        try:
            payload = json.loads(body)
        except Exception:
            payload = None
        if status == 200:
            if not isinstance(payload, dict) or "stems" not in payload:
                fails.append("GET /api/state: 200 but no 'stems' key")
        elif status == 503:
            if not isinstance(payload, dict) or payload.get("ok") is not False:
                fails.append("GET /api/state: 503 but body is not "
                             '{"ok": false, ...}')
        else:
            fails.append(f"GET /api/state: status {status} (want 200 or 503)")

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

        if fails:
            for f in fails:
                print(f"bad: {f}")
            return 1
        print("ok: / /mixer /pads /library serve HTML; "
              "/api/state sane; unknown POST 404s")
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
