#!/usr/bin/env bash
#
# brownnote test suite.  Plain bash, no frameworks.  python3 is used
# as a test helper for fixtures and measurement; every use is guarded.
#
# Real-bench tests need the GuitarPedal bench (see README); without it
# they SKIP (and the suite still passes), which is what CI relies on
# never having built it.  Plumbing tests run against /bin/cat and so
# need nothing but brownnote itself.
#
set -u
cd "$(dirname "$0")/.."

BIN=build/brownnote
BENCH="${BENCH:-vendor/GuitarPedal/Validation/bench/bench}"
KUJAMBA="${KUJAMBA:-.refs/fart-app/zig-out/bin/kujamba}"
ZCN="${ZCN:-.refs/zcn/zig-out/bin/zcn}"

PASS=0; FAIL=0; SKIP=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

ok()   { PASS=$((PASS + 1)); echo "PASS: $*"; }
bad()  { FAIL=$((FAIL + 1)); echo "FAIL: $*"; }
skip() { SKIP=$((SKIP + 1)); echo "SKIP: $*"; }

have_bench=0
[ -x "$BENCH" ] && have_bench=1

have_python=0
command -v python3 >/dev/null 2>&1 && have_python=1

# ------------------------------------------------------------------
# (a) build
# ------------------------------------------------------------------
if make -s >/dev/null 2>&1 && [ -x "$BIN" ]; then
	ok "(a) build: make -> $BIN"
else
	bad "(a) build: make failed"
	echo "SUMMARY: FAILED (1/1)"
	exit 1
fi

# ------------------------------------------------------------------
# fixtures
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	python3 - "$TMP" <<'PYEOF'
import struct, sys, wave, math, os
d = sys.argv[1]

# raw float32 stereo 48 kHz: 0.5 s of a decaying 80 Hz buzz, both channels
SR = 48000
frames = bytearray()
for i in range(SR // 2):
    t = i / SR
    env = min(1.0, t / 0.005) * math.exp(-4.0 * t)
    s = 0.4 * env * math.sin(2 * math.pi * 80 * t)
    frames += struct.pack('<ff', s, s)
open(os.path.join(d, 'raw.f32'), 'wb').write(frames)

# s16le mono 44100 WAV: 1.0 s of a 220 Hz tone with an envelope
w = wave.open(os.path.join(d, 'in.wav'), 'wb')
w.setnchannels(1); w.setsampwidth(2); w.setframerate(44100)
pcm = bytearray()
for i in range(44100):
    t = i / 44100.0
    env = min(1.0, t / 0.01) * math.exp(-2.5 * t)
    v = int(12000 * env * math.sin(2 * math.pi * 220 * t))
    pcm += struct.pack('<h', v)
w.writeframes(bytes(pcm)); w.close()
PYEOF
	[ $? -eq 0 ] || { bad "fixtures"; echo "SUMMARY: FAILED"; exit 1; }
fi

# ------------------------------------------------------------------
# (b) plumbing identity: raw through /bin/cat is byte-identical
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	"$BIN" --bench /bin/cat < "$TMP/raw.f32" > "$TMP/echo.f32" 2>/dev/null
	if [ $? -eq 0 ] && cmp -s "$TMP/raw.f32" "$TMP/echo.f32"; then
		ok "(b) raw plumbing: /bin/cat round trip is byte-identical"
	else
		bad "(b) raw plumbing: /bin/cat round trip differs"
	fi
else
	skip "(b) raw plumbing: python3 absent (fixture)"
fi

# ------------------------------------------------------------------
# (c) raw fuzz through the real bench changes the signal
# ------------------------------------------------------------------
if [ "$have_bench" = 1 ] && [ "$have_python" = 1 ]; then
	"$BIN" --effect fuzz < "$TMP/raw.f32" > "$TMP/fuzz.f32" 2>"$TMP/fuzz.err"
	rc=$?
	sz_in=$(wc -c < "$TMP/raw.f32" | tr -d ' ')
	sz_out=$(wc -c < "$TMP/fuzz.f32" | tr -d ' ')
	if [ $rc -eq 0 ] && [ "$sz_in" = "$sz_out" ] && \
	   ! cmp -s "$TMP/raw.f32" "$TMP/fuzz.f32"; then
		ratio=$(python3 - "$TMP/raw.f32" "$TMP/fuzz.f32" <<'PYEOF'
import array, math, sys
def rms(p):
    a = array.array('f'); a.frombytes(open(p,'rb').read())
    return math.sqrt(sum(x*x for x in a) / len(a))
r = rms(sys.argv[2]) / max(rms(sys.argv[1]), 1e-12)
print(f"{r:.4f}")
PYEOF
)
		pass=$(python3 -c "print(1 if 0.2 <= float('$ratio') <= 5.0 else 0)")
		if [ "$pass" = "1" ]; then
			ok "(c) raw fuzz: output differs, rms ratio $ratio"
		else
			bad "(c) raw fuzz: rms ratio $ratio outside 0.2..5"
		fi
	else
		bad "(c) raw fuzz: rc=$rc size in=$sz_in out=$sz_out"
	fi
elif [ "$have_bench" = 0 ]; then
	skip "(c) raw fuzz: bench not built ($BENCH)"
else
	skip "(c) raw fuzz: python3 absent"
fi

# ------------------------------------------------------------------
# (d) unknown effect: nonzero exit, bench's own message on stderr
# ------------------------------------------------------------------
if [ "$have_bench" = 0 ]; then
	skip "(d) unknown effect: bench not built ($BENCH)"
elif [ "$have_python" = 1 ]; then
	"$BIN" --effect nosucheffectxyz < "$TMP/raw.f32" > /dev/null \
		2> "$TMP/unknown.err"
	rc=$?
	if [ $rc -ne 0 ] && grep -qi "no effect called" "$TMP/unknown.err"; then
		ok "(d) unknown effect: exit $rc with the bench's message"
	else
		bad "(d) unknown effect: rc=$rc, stderr:"
		sed 's/^/      /' "$TMP/unknown.err" | head -5
	fi
else
	skip "(d) unknown effect: python3 absent"
fi

# ------------------------------------------------------------------
# (e) chaining: fuzz + delay differs from fuzz alone
# ------------------------------------------------------------------
if [ "$have_bench" = 1 ] && [ "$have_python" = 1 ]; then
	"$BIN" --effect fuzz --effect delay < "$TMP/raw.f32" \
		> "$TMP/chain.f32" 2>/dev/null
	rc=$?
	if [ $rc -eq 0 ] && ! cmp -s "$TMP/fuzz.f32" "$TMP/chain.f32"; then
		ok "(e) chain: fuzz+delay differs from fuzz alone"
	else
		bad "(e) chain: rc=$rc or identical output"
	fi
elif [ "$have_bench" = 0 ]; then
	skip "(e) chain: bench not built"
else
	skip "(e) chain: python3 absent"
fi

# ------------------------------------------------------------------
# (f) SIGPIPE: a closed downstream reader must not hang us
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	( "$BIN" --bench /bin/cat < "$TMP/raw.f32" 2>/dev/null \
		| head -c 1000 > /dev/null ) &
	pipel=$!
	watchdog=0
	for i in $(seq 1 100); do
		kill -0 $pipel 2>/dev/null || break
		sleep 0.1
		watchdog=$i
	done
	if kill -0 $pipel 2>/dev/null; then
		kill $pipel 2>/dev/null
		wait $pipel 2>/dev/null
		bad "(f) SIGPIPE: pipeline hung"
	else
		wait $pipel 2>/dev/null
		prc=$?
		if [ $prc -eq 0 ] || [ $prc -eq 141 ] || [ $prc -eq 1 ]; then
			ok "(f) SIGPIPE: pipeline finished (rc $prc), no hang"
		else
			bad "(f) SIGPIPE: unexpected rc $prc"
		fi
	fi
else
	skip "(f) SIGPIPE: python3 absent"
fi

# ------------------------------------------------------------------
# (g) WAV passthrough: through /bin/cat, header mirrors input
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	"$BIN" --bench /bin/cat < "$TMP/in.wav" > "$TMP/out.wav" 2>/dev/null
	rc=$?
	if [ $rc -eq 0 ]; then
		verdict=$(python3 - "$TMP/in.wav" "$TMP/out.wav" <<'PYEOF'
import struct, sys, math

def parse(p):
    b = open(p, 'rb').read()
    if len(b) < 44 or b[0:4] != b'RIFF' or b[8:12] != b'WAVE':
        return None
    fmt, ch, rate, bits = None, None, None, None
    pos, data = 12, None
    while pos + 8 <= len(b):
        cid = b[pos:pos+4]
        csz = struct.unpack('<I', b[pos+4:pos+8])[0]
        if pos + 8 + csz > len(b):
            csz = len(b) - pos - 8
        if cid == b'fmt ' and csz >= 16:
            fmt, ch, rate = struct.unpack('<HHI', b[pos+8:pos+16])
            bits = struct.unpack('<H', b[pos+22:pos+24])[0]
        elif cid == b'data':
            data = b[pos+8:pos+8+csz]
        pos += 8 + csz + (csz & 1)
    if fmt is None or data is None:
        return None
    return dict(fmt=fmt, ch=ch, rate=rate, bits=bits, data=data)

a = parse(sys.argv[1]); b = parse(sys.argv[2])
if not a or not b:
    print("bad: unparseable"); raise SystemExit
if (b['fmt'], b['ch'], b['rate'], b['bits']) != (1, a['ch'], a['rate'], 16):
    print(f"bad: header {b['fmt']}/{b['ch']}ch/{b['rate']}Hz vs input"); raise SystemExit
fa, fb = len(a['data']) // 2, len(b['data']) // 2
if abs(fa - fb) > 2:
    print(f"bad: frames {fa} vs {fb}"); raise SystemExit
ra = [struct.unpack('<h', a['data'][2*i:2*i+2])[0] / 32768.0 for i in range(fa)]
rb = [struct.unpack('<h', b['data'][2*i:2*i+2])[0] / 32768.0 for i in range(fb)]
rmsa = math.sqrt(sum(x*x for x in ra) / max(fa,1))
rmsb = math.sqrt(sum(x*x for x in rb) / max(fb,1))
d = abs(rmsa - rmsb)
if d < 5e-3:
    print(f"ok: frames {fa}->{fb}, rms {rmsa:.5f}->{rmsb:.5f} (delta {d:.5f})")
else:
    print(f"bad: rms delta {d:.5f}")
PYEOF
)
		case "$verdict" in
			ok:*) ok "(g) WAV passthrough: $verdict" ;;
			*)   bad "(g) WAV passthrough: $verdict" ;;
		esac
	else
		bad "(g) WAV passthrough: rc=$rc"
	fi
else
	skip "(g) WAV passthrough: python3 absent"
fi

# ------------------------------------------------------------------
# (h) WAV fuzz through the real bench
# ------------------------------------------------------------------
if [ "$have_bench" = 1 ] && [ "$have_python" = 1 ]; then
	"$BIN" --effect fuzz < "$TMP/in.wav" > "$TMP/fuzz.wav" 2>/dev/null
	rc=$?
	if [ $rc -eq 0 ]; then
		verdict=$(python3 - "$TMP/in.wav" "$TMP/fuzz.wav" <<'PYEOF'
import struct, sys, math

def rms_and_header(p):
    b = open(p, 'rb').read()
    pos, data, ch, rate = 12, None, None, None
    while pos + 8 <= len(b):
        cid = b[pos:pos+4]; csz = struct.unpack('<I', b[pos+4:pos+8])[0]
        if cid == b'fmt ' and csz >= 16:
            ch, rate = struct.unpack('<HI', b[pos+10:pos+16])
        elif cid == b'data':
            data = b[pos+8:pos+8+csz]
        pos += 8 + csz + (csz & 1)
    n = len(data) // 2
    vals = [struct.unpack('<h', data[2*i:2*i+2])[0] / 32768.0 for i in range(n)]
    return ch, rate, math.sqrt(sum(x*x for x in vals) / max(n,1))

cha, ra, rmsa = rms_and_header(sys.argv[1])
chb, rb, rmsb = rms_and_header(sys.argv[2])
if (chb, rb) != (cha, ra):
    print(f"bad: header {chb}ch/{rb}Hz vs input {cha}ch/{ra}Hz"); raise SystemExit
if abs(rmsb - rmsa) / max(rmsa, 1e-9) < 0.01:
    print(f"bad: rms {rmsa:.5f} -> {rmsb:.5f}, <1% change")
else:
    print(f"ok: {cha}ch/{ra}Hz kept, rms {rmsa:.5f} -> {rmsb:.5f}")
PYEOF
)
		case "$verdict" in
			ok:*) ok "(h) WAV fuzz: $verdict" ;;
			*)   bad "(h) WAV fuzz: $verdict" ;;
		esac
	else
		bad "(h) WAV fuzz: rc=$rc"
	fi
elif [ "$have_bench" = 0 ]; then
	skip "(h) WAV fuzz: bench not built"
else
	skip "(h) WAV fuzz: python3 absent"
fi

# ------------------------------------------------------------------
# (i) --list streams the bench's own list
# ------------------------------------------------------------------
if [ "$have_bench" = 1 ]; then
	if "$BIN" --list 2>/dev/null | grep -q "Rat Sketch"; then
		ok "(i) --list: the bench's effect list came through"
	else
		bad "(i) --list: no output or no 'Rat Sketch'"
	fi
else
	skip "(i) --list: bench not built"
fi

# ------------------------------------------------------------------
# (j) the real chain: kujamba -> brownnote -> zcn check-wav
# ------------------------------------------------------------------
if [ -x "$KUJAMBA" ] && [ -x "$ZCN" ] && [ "$have_bench" = 1 ]; then
	"$KUJAMBA" render --phrase "kujamba karibu" --seed 42 \
		--out "$TMP/t_in.wav" >/dev/null 2>&1
	if [ $? -eq 0 ]; then
		"$BIN" --effect fuzz < "$TMP/t_in.wav" > "$TMP/t_out.wav" \
			2>/dev/null
		if [ $? -eq 0 ]; then
			if "$ZCN" check-wav "$TMP/t_out.wav" --min-rms 0.01 \
				>/dev/null 2>&1; then
				ok "(j) round trip: kujamba -> brownnote fuzz -> zcn check-wav"
			else
				bad "(j) round trip: zcn check-wav rejected the output"
			fi
		else
			bad "(j) round trip: brownnote failed on kujamba's WAV"
		fi
	else
		bad "(j) round trip: kujamba render failed"
	fi
else
	skip "(j) round trip: kujamba/zcn/bench not all present"
fi

# ------------------------------------------------------------------
# (k) argv overflow: max flags on every slot at once must not corrupt
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	set --
	for i in $(seq 1 256); do set -- "$@" --effect fuzz; done
	for i in $(seq 1 256); do set -- "$@" --pot 'Rat Sketch:Distortion=60'; done
	for i in $(seq 1 256); do set -- "$@" --mix 'Boost=60'; done
	"$BIN" --bench /bin/cat "$@" < /dev/null > /dev/null 2> "$TMP/ovf.err" &
	ovf_pid=$!
	ovf_rc=0
	for i in $(seq 1 100); do
		kill -0 $ovf_pid 2>/dev/null || break
		sleep 0.1
	done
	if kill -0 $ovf_pid 2>/dev/null; then
		kill -9 $ovf_pid 2>/dev/null
		wait $ovf_pid 2>/dev/null
		ovf_rc=999
	else
		wait $ovf_pid 2>/dev/null || ovf_rc=$?
	fi
	if [ $ovf_rc -eq 0 ] || [ $ovf_rc -eq 1 ]; then
		ok "(k) argv overflow: 768 flags, exit $ovf_rc, no crash"
	else
		bad "(k) argv overflow: exit $ovf_rc (hang/abort/segv?)"
	fi
else
	skip "(k) argv overflow: python3 absent"
fi

# ------------------------------------------------------------------
# (l) WAV with fmt but no data chunk: rejected deterministically,
#     never parsed against an uninitialized wav_info (#2)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	python3 - "$TMP" <<'PYEOF'
import struct, sys, os
def chunk(cid, body):
    out = cid + struct.pack('<I', len(body)) + body
    if len(body) & 1: out += b'\0'
    return out
fmt = chunk(b'fmt ', struct.pack('<HHIIHH', 1, 1, 44100, 88200, 2, 16))
blob = (b'RIFF' + struct.pack('<I', 4 + len(fmt) + 8 + 8) + b'WAVE'
        + fmt + chunk(b'LIST', b'INFOjunk'))
open(os.path.join(sys.argv[1], 'nodata.wav'), 'wb').write(blob)
PYEOF
	"$BIN" --bench /bin/cat < "$TMP/nodata.wav" > /dev/null \
		2> "$TMP/nodata.err"
	rc=$?
	if [ $rc -eq 2 ] && grep -q "missing fmt or data" "$TMP/nodata.err"; then
		ok "(l) no-data-chunk WAV: exit 2, deterministic rejection"
	else
		bad "(l) no-data-chunk WAV: rc=$rc (crash or garbage parse?), stderr:"
		sed 's/^/      /' "$TMP/nodata.err" | head -3
	fi
else
	skip "(l) no-data-chunk WAV: python3 absent"
fi

# ------------------------------------------------------------------
# (m) one-frame 192 kHz WAV: resamples to zero bench frames, which
#     must still close the bench's stdin, not deadlock (#3)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	python3 - "$TMP" <<'PYEOF'
import struct, sys, os
fmt = b'fmt ' + struct.pack('<I', 16) + struct.pack('<HHIIHH', 1, 2, 192000, 192000*4, 4, 16)
pcm = struct.pack('<hh', 1000, -1000)
wav = (b'RIFF' + struct.pack('<I', 36 + len(pcm)) + b'WAVE' + fmt
       + b'data' + struct.pack('<I', len(pcm)) + pcm)
open(os.path.join(sys.argv[1], 'one.wav'), 'wb').write(wav)
PYEOF
	"$BIN" --bench /bin/cat < "$TMP/one.wav" > "$TMP/one_out.wav" \
		2>/dev/null &
	one_pid=$!
	watchdog=0
	for i in $(seq 1 50); do
		kill -0 $one_pid 2>/dev/null || break
		sleep 0.1
		watchdog=$i
	done
	if kill -0 $one_pid 2>/dev/null; then
		kill -9 $one_pid 2>/dev/null
		wait $one_pid 2>/dev/null
		bad "(m) 1-frame 192 kHz WAV: hung ($((watchdog + 1))s, killed)"
	else
		wait $one_pid 2>/dev/null
		rc=$?
		if [ $rc -eq 0 ] && [ "$(wc -c < "$TMP/one_out.wav" | tr -d ' ')" = "44" ]; then
			ok "(m) 1-frame 192 kHz WAV: exit 0, empty 44-byte result"
		else
			bad "(m) 1-frame 192 kHz WAV: rc=$rc, $(wc -c < "$TMP/one_out.wav" | tr -d ' ') bytes out"
		fi
	fi
else
	skip "(m) 1-frame 192 kHz WAV: python3 absent"
fi

# ------------------------------------------------------------------
# (n) broken stderr relay: a bench that floods stderr while the
#     reader of brownnote's stderr is gone must not wedge the
#     pipeline (#4)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	printf '#!/bin/sh\nhead -c 200000 /dev/zero >&2\ncat\n' \
		> "$TMP/chatty.sh"
	chmod +x "$TMP/chatty.sh"
	# brownnote's stderr goes to a reader that leaves after 10 bytes
	( "$BIN" --bench "$TMP/chatty.sh" < "$TMP/raw.f32" \
		2>&1 >/dev/null | head -c 10 > /dev/null ) &
	relay_pid=$!
	for i in $(seq 1 100); do
		kill -0 $relay_pid 2>/dev/null || break
		sleep 0.1
	done
	if kill -0 $relay_pid 2>/dev/null; then
		kill -9 $relay_pid 2>/dev/null
		wait $relay_pid 2>/dev/null
		pkill -9 -f "$TMP/chatty.sh" 2>/dev/null
		bad "(n) broken stderr relay: pipeline hung, killed"
	else
		wait $relay_pid 2>/dev/null
		ok "(n) broken stderr relay: pipeline finished, no wedge"
	fi
else
	skip "(n) broken stderr relay: python3 absent"
fi

# ------------------------------------------------------------------
# (o) non-blocking stdin: a late or partial delivery is not EOF;
#     mode detection and the WAV body must wait for data (#5)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	verdict=$(python3 - "$BIN" <<'PYEOF'
import math, os, struct, subprocess, sys, time, wave

bin_ = sys.argv[1]
path = os.environ.get('TMPDIR', '/tmp') + '/nb_test.wav'
w = wave.open(path, 'wb')
w.setnchannels(1); w.setsampwidth(2); w.setframerate(44100)
w.writeframes(b''.join(struct.pack('<h', int(9000 * math.sin(2 * math.pi * 440 * i / 44100.0)))
                       for i in range(4410)))
w.close()
wav = open(path, 'rb').read()
os.unlink(path)

def spawn():
    r, wr = os.pipe()
    os.set_blocking(r, False)          # a poll()-driven parent's stdin
    p = subprocess.Popen([bin_, '--bench', '/bin/cat'], stdin=r,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    os.close(r)
    return p, wr

def checked(p, wr):
    out, err = p.communicate(timeout=20)
    if p.returncode != 0:
        raise SystemExit(f"bad: rc={p.returncode} err={err[:80]!r}")
    return out

ref = subprocess.run([bin_, '--bench', '/bin/cat'], input=wav,
                     capture_output=True, timeout=20)
if ref.returncode != 0:
    raise SystemExit(f"bad: reference rc={ref.returncode}")

p, wr = spawn()
time.sleep(0.4)                        # nothing there at peek time
os.write(wr, wav); os.close(wr)
late = checked(p, wr)

p, wr = spawn()
time.sleep(0.4)
os.write(wr, wav[:20])                 # a truncated RIFF header first
time.sleep(0.2)
os.write(wr, wav[20:]); os.close(wr)
partial = checked(p, wr)

if late != ref.stdout or partial != ref.stdout:
    raise SystemExit("bad: non-blocking output differs from blocking")
print("ok: late and partial deliveries match the blocking run")
PYEOF
)
	case "$verdict" in
		ok:*) ok "(o) non-blocking stdin: $verdict" ;;
		*)   bad "(o) non-blocking stdin: $verdict" ;;
	esac
else
	skip "(o) non-blocking stdin: python3 absent"
fi

# ------------------------------------------------------------------
# (p) character-device stdin (/dev/null): poll says POLLNVAL on
#     macOS, which must resolve to EOF, not a 100%-CPU busy-spin (#6)
# ------------------------------------------------------------------
"$BIN" --bench /bin/cat < /dev/null > /dev/null 2>/dev/null &
devnull_pid=$!
for i in $(seq 1 30); do
	kill -0 $devnull_pid 2>/dev/null || break
	sleep 0.1
done
if kill -0 $devnull_pid 2>/dev/null; then
	kill -9 $devnull_pid 2>/dev/null
	wait $devnull_pid 2>/dev/null
	pkill -9 -f "$BIN --bench /bin/cat" 2>/dev/null
	bad "(p) /dev/null stdin: busy-spun past 3s, killed"
else
	wait $devnull_pid 2>/dev/null
	rc=$?
	if [ $rc -eq 0 ]; then
		ok "(p) /dev/null stdin: exit 0, no spin"
	else
		bad "(p) /dev/null stdin: rc=$rc"
	fi
fi

# ------------------------------------------------------------------
# (q) 4 GiB wrap: output past RIFF's 32-bit size fields must fail
#     loudly, not wrap the header around an empty file (#7).
#     Gated: peaks ~13 GB RAM and tens of seconds.
# ------------------------------------------------------------------
if [ "${BROWNNOTE_HEAVY:-0}" = "1" ] && [ "$have_python" = 1 ]; then
	# stereo 48 kHz, so s16 output is exactly half the bench's float32
	python3 - "$TMP" <<'PYEOF'
import struct, sys, os
fmt = b'fmt ' + struct.pack('<I', 16) + struct.pack('<HHIIHH', 1, 2, 48000, 48000*4, 4, 16)
pcm = struct.pack('<hh', 1000, -1000)
open(os.path.join(sys.argv[1], 'tiny48s.wav'), 'wb').write(
    b'RIFF' + struct.pack('<I', 36 + len(pcm)) + b'WAVE' + fmt
    + b'data' + struct.pack('<I', len(pcm)) + pcm)
PYEOF
	printf '#!/bin/sh\nexec dd if=/dev/zero bs=1048576 count=%s 2>/dev/null\n' \
		4096 > "$TMP/gen4g.sh"
	chmod +x "$TMP/gen4g.sh"
	# 4 GiB of float32 stereo is 2 GiB of s16 — legal, header exact
	"$BIN" --bench "$TMP/gen4g.sh" < "$TMP/tiny48s.wav" 2>/dev/null |
		head -c 44 > "$TMP/limit4g.hdr"
	verdict=$(python3 - "$TMP/limit4g.hdr" <<'PYEOF'
import sys
b = open(sys.argv[1], 'rb').read(44)
if len(b) != 44:
    print("bad: short header"); raise SystemExit
riff = int.from_bytes(b[4:8], 'little')
data = int.from_bytes(b[40:44], 'little')
if data == 2147483648 and riff == 2147483648 + 36:
    print("ok: 2 GiB result (legal) header is exact")
else:
    print(f"bad: riff={riff} data={data}")
PYEOF
)
	case "$verdict" in
		ok:*) ok "(q) 4 GiB wrap: $verdict" ;;
		*)   bad "(q) 4 GiB wrap: $verdict" ;;
	esac
	printf '#!/bin/sh\nexec dd if=/dev/zero bs=1048576 count=%s 2>/dev/null\n' \
		8192 > "$TMP/gen8g.sh"
	chmod +x "$TMP/gen8g.sh"
	# 8 GiB of float32 stereo is 4 GiB of s16 — past the limit
	"$BIN" --bench "$TMP/gen8g.sh" < "$TMP/tiny48s.wav" > /dev/null \
		2> "$TMP/over4g.err"
	rc=$?
	if [ $rc -eq 2 ] && grep -q "4 GiB" "$TMP/over4g.err"; then
		ok "(q) 4 GiB wrap: oversized result exits 2, no wrapped header"
	else
		bad "(q) 4 GiB wrap: rc=$rc (silent wrap?), stderr: $(head -1 "$TMP/over4g.err")"
	fi
else
	skip "(q) 4 GiB wrap: set BROWNNOTE_HEAVY=1 (~13 GB RAM, tens of seconds)"
fi

# ------------------------------------------------------------------
echo
total=$((PASS + FAIL))
if [ $FAIL -eq 0 ]; then
	echo "SUMMARY: PASSED $total/$total (skipped $SKIP)"
	exit 0
else
	echo "SUMMARY: FAILED $PASS/$total (skipped $SKIP)"
	exit 1
fi
