#!/usr/bin/env python3
"""Dynamic attack checks against a running studio.

This is the dynamic counterpart to tests/studio_security.py: it boots the real
server and sends the requests an attacker would send at the HTTP boundary
(traversal, source disclosure, reflected payloads, header injection, method
tampering, body floods, CSRF and Host tricks) and asserts the server's
observable answer.

Usage:
  python3 tests/dast_studio.py            # boots its own server
  python3 tests/dast_studio.py --url ...  # scan an already running studio
"""

from __future__ import annotations

import argparse
import http.client
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def client(port: int) -> dict[str, str]:
    """Headers the studio's own pages send, with the exact origin it serves."""
    return {"X-Studio-Client": "1",
            "Origin": f"http://127.0.0.1:{port}"}
failures: list[str] = []
checks = 0


def check(ok: bool, what: str, detail: str = "") -> None:
    global checks
    checks += 1
    if not ok:
        failures.append(f"{what}{(': ' + detail) if detail else ''}")


def request(port: int, method: str, path: str, body: bytes | None = None,
            headers: dict[str, str] | None = None,
            timeout: float = 10.0) -> tuple[int, dict[str, str], str]:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        conn.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
        conn.putheader("Host", headers.get("Host", f"127.0.0.1:{port}")
                       if headers else f"127.0.0.1:{port}")
        for key, value in (headers or {}).items():
            if key == "Host":
                continue
            conn.putheader(key, value)
        if body is not None and "Content-Length" not in (headers or {}):
            conn.putheader("Content-Length", str(len(body)))
        conn.endheaders(body)
        resp = conn.getresponse()
        raw = resp.read()
        return resp.status, {k.lower(): v for k, v in resp.getheaders()}, \
            raw.decode("utf-8", "replace")
    finally:
        conn.close()


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def boot(port: int) -> subprocess.Popen:
    env = dict(os.environ)
    env["STUDIO_PORT"] = str(port)
    env["STUDIO_HOST"] = "127.0.0.1"
    env["STUDIO_ANALYTICS"] = "0"
    proc = subprocess.Popen(
        [sys.executable, "scripts/studio.py"], cwd=ROOT, env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    deadline = time.time() + 120
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SystemExit(
                f"dast: studio exited early:\n{proc.stderr.read()[-2000:]}")
        try:
            status, _, _ = request(port, "GET", "/healthz", timeout=2)
            if status == 200:
                return proc
        except OSError:
            pass
        time.sleep(0.5)
    proc.terminate()
    raise SystemExit("dast: studio did not become ready in 120s")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", help="host:port of a running studio")
    args = ap.parse_args()

    proc = None
    if args.url:
        host, _, port_s = args.url.partition(":")
        port = int(port_s or "8765")
        if host not in ("127.0.0.1", "localhost"):
            raise SystemExit("dast: only loopback targets are allowed")
    else:
        port = free_port()
        proc = boot(port)

    try:
        for path in ("/../../etc/passwd", "/%2e%2e/%2e%2e/etc/passwd",
                     "/..%2f..%2fetc%2fpasswd", "/recordings/../src/wav.c"):
            status, _, body = request(port, "GET", path)
            check(status in (400, 404), f"traversal {path} is refused",
                  f"status {status}")
            check("root:" not in body and "BROWNNOTE_" not in body,
                  f"traversal {path} leaks nothing")

        for path in ("/.git/config", "/.git/HEAD", "/scripts/studio.py",
                     "/scripts/", "/tests/", "/src/log.c"):
            status, _, body = request(port, "GET", path)
            check(status == 404, f"{path} is not served", f"status {status}")
            check("def " not in body and "[core]" not in body,
                  f"{path} returns no source")

        payload = "<script>alert(1)</script>"
        status, _, body = request(port, "GET", f"/?q={payload}")
        check(payload not in body, "query input is not reflected verbatim")
        check(status in (200, 400), "query input does not error", str(status))

        status, headers, _ = request(
            port, "GET", "/", headers={"X-Request-ID": "a" * 5000})
        check(status in (200, 400), "oversized request id is bounded",
              str(status))
        check(len(headers.get("x-request-id", "")) <= 64,
              "request id header stays bounded")

        marker = "dast-marker-9381"
        for method in ("PUT", "DELETE", "TRACE", "PATCH"):
            status, _, body = request(
                port, method, "/", headers={"X-Dast-Marker": marker})
            check(status in (400, 404, 405, 501),
                  f"{method} / is not handled", str(status))
            check(marker not in body, f"{method} does not echo headers")

        status, _, _ = request(
            port, "POST", "/api/patch", body=b"{" * 4096,
            headers={**client(port), "Content-Type": "application/json"})
        check(status in (400, 413), "malformed body is rejected", str(status))

        # Declare a body far over the cap and send none of it: the refusal must
        # come from the Content-Length check, before the server reads bytes.
        status, _, _ = request(
            port, "POST", "/api/upload-sample?name=dast",
            headers={**client(port), "Content-Type": "audio/wav",
                     "Content-Length": str(1 << 30)})
        check(status == 413, "1 GiB declared body is refused", str(status))

        status, _, _ = request(
            port, "POST", "/api/patch",
            body=b'{"name":"dast","patch":{"v":1}}',
            headers={"Content-Type": "application/json"})
        check(status == 403, "missing client header is refused", str(status))

        status, _, _ = request(
            port, "POST", "/api/patch",
            body=b'{"name":"dast","patch":{"v":1}}',
            headers={"X-Studio-Client": "1",
                     "Origin": "https://evil.example",
                     "Content-Type": "application/json"})
        check(status == 403, "foreign origin is refused", str(status))

        status, headers, _ = request(
            port, "GET", "/", headers={"Host": "evil.example"})
        check(status == 403, "foreign Host is refused", str(status))

        status, headers, _ = request(
            port, "POST", "/api/patch",
            body=b'{"name":"dast\\r\\nX-Injected: 1","patch":{"v":1}}',
            headers={**client(port), "Content-Type": "application/json"})
        check(status == 400, "CRLF in a name is refused", str(status))
        check("x-injected" not in headers, "no header is injected")

        status, headers, _ = request(port, "GET", "/metrics")
        check(status == 200, "metrics is served", str(status))
        check("BROWNNOTE_" not in headers.get("server", ""),
              "server header carries no config")

        status, _, _ = request(port, "GET", "/" + "a" * 9000)
        check(status in (400, 404, 414), "very long path is refused",
              str(status))

        # The API is closed without the client header and with a foreign
        # origin, and reachable with both, so the gate is not incidentally
        # open on the routes a scanner would try.
        for path in ("/api/state", "/api/library"):
            status, _, _ = request(port, "GET", path)
            check(status == 403, f"{path} without the client header is 403",
                  str(status))
            status, _, _ = request(
                port, "GET", path,
                headers={"X-Studio-Client": "1",
                         "Origin": "https://evil.example"})
            check(status == 403, f"{path} from a foreign origin is 403",
                  str(status))
            status, _, _ = request(port, "GET", path, headers=client(port))
            check(status in (200, 503), f"{path} answers same-origin requests",
                  str(status))
    finally:
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()

    for line in failures:
        print(f"DAST FAIL: {line}", file=sys.stderr)
    print(f"dast_studio: {checks - len(failures)}/{checks} checks")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
