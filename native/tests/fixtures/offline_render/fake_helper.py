#!/usr/bin/env python3
"""A scripted offline-render helper for tests. It speaks the protocol in
native/src/pedals/offline/PROTOCOL.md and hosts fake plugins chosen by ID.

The artifact identity is the SHA-256 of the file at the requested location,
so tests can change the "installed plugin" by rewriting that file.
"""
import hashlib, json, math, os, signal, struct, sys, time

inp, out = sys.stdin.buffer, sys.stdout.buffer

if len(sys.argv) == 3 and sys.argv[1] == "--pid-file":
    with open(sys.argv[2], "w") as f:
        f.write(str(os.getpid()))


def read_exact(n):
    data = b""
    while len(data) < n:
        chunk = inp.read(n - len(data))
        if not chunk:
            sys.exit(0)
        data += chunk
    return data


def read_msg():
    tag = read_exact(1)
    (n,) = struct.unpack("<I", read_exact(4))
    return tag, read_exact(n)


def send(tag, payload):
    out.write(tag + struct.pack("<I", len(payload)) + payload)
    out.flush()


def error(code, message):
    send(b"X", json.dumps({"code": code, "message": message}).encode())
    sys.exit(3)


tag, payload = read_msg()
assert tag == b"O", tag
(hlen,) = struct.unpack("<I", payload[:4])
header = json.loads(payload[4:4 + hlen])
state = payload[4 + hlen:] if header["state"] else None
pid = header["plugin_id"]
rate, ch = header["rate"], header["channels"]
params = {p["id"]: p["value"] for p in header["params"]}

if pid == "fake.hang-open":
    time.sleep(3600)
loc = header["location"]
if not os.path.isfile(loc):
    error("bundle-missing", "no plugin at the requested location")
with open(loc, "rb") as f:
    artifact = hashlib.sha256(f.read()).hexdigest()
known = {"fake.gain", "fake.echo", "fake.latency", "fake.drone", "fake.crash",
         "fake.hang", "fake.nonfinite", "fake.realtime-only", "fake.wrong-length",
         "fake.garbage", "fake.fail", "fake.wrong-id", "fake.slow"}
if pid not in known:
    error("unknown-plugin-id", "no plugin with ID " + pid)
if pid == "fake.realtime-only":
    error("unsupported", "the plugin requires hard realtime processing")

gain = 0.5
if state is not None:
    if len(state) != 8:
        error("state-rejected", "state must be 8 bytes")
    (gain,) = struct.unpack("<d", state)
gain = params.get(0, gain)
latency = 64 if pid == "fake.latency" else 0
declared_tail = {"fake.echo": int(0.05 * rate), "fake.drone": 0xFFFFFFFF}.get(pid)
ready = {
    "protocol": 1,
    "plugin": {"backend": header["backend"],
               "id": "fake.other" if pid == "fake.wrong-id" else pid,
               "artifact": artifact, "abi": "fake 1", "version": "1.0"},
    "name": "Fake " + pid.split(".")[1],
    "latency_frames": latency,
    "tail_frames": declared_tail,
    "params": [{"id": 0, "name": "Gain", "min": 0.0, "max": 4.0,
                "default": 0.5, "stepped": False}],
}
send(b"R", json.dumps(ready).encode())

delay = [0.0] * ch * max(1, int(0.01 * rate))
dpos = 0
pipe = [0.0] * ch * latency
block = 0
while True:
    tag, payload = read_msg()
    if tag == b"E":
        send(b"D", b"\x01" + struct.pack("<d", gain))
        sys.exit(0)
    assert tag == b"P", tag
    (frames,) = struct.unpack("<I", payload[:4])
    x = list(struct.unpack("<%df" % (frames * ch), payload[4:]))
    if block == 3:
        if pid == "fake.crash":
            os.kill(os.getpid(), signal.SIGSEGV)
        if pid == "fake.hang":
            time.sleep(3600)
        if pid == "fake.fail":
            error("process-failed", "the plugin reported an error")
    if pid == "fake.slow":
        time.sleep(0.05)
    if pid == "fake.echo":
        y = []
        for s in x:
            d = delay[dpos]
            v = s + 0.5 * d
            delay[dpos] = v
            dpos = (dpos + 1) % len(delay)
            y.append(v)
    elif pid == "fake.latency":
        pipe.extend(x)
        y, pipe = pipe[:len(x)], pipe[len(x):]
    elif pid == "fake.drone":
        y = [s + 0.1 for s in x]
    else:
        y = [s * gain for s in x]
    if pid == "fake.nonfinite" and block == 3:
        y[5 * ch] = math.nan
    if pid == "fake.wrong-length":
        y = y[:-ch]
    if pid == "fake.garbage" and block == 1:
        out.write(b"Z\x00\x00\x00\x00")
        out.flush()
        time.sleep(3600)
    send(b"B", struct.pack("<%df" % len(y), *y))
    block += 1
