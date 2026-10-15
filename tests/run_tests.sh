#!/usr/bin/env bash
#
# brownnote test suite.  Plain bash, no frameworks.  python3 is used
# as a test helper for fixtures and measurement; every use is guarded.
#
# Real-bench tests need the GuitarPedal bench (see README); without it
# they SKIP and the suite still passes.  Plumbing tests run against
# /bin/cat and so need nothing but brownnote itself.
#
set -u
cd "$(dirname "$0")/.." || exit

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
	if python3 - "$TMP" <<'PYEOF'
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
	then
		:
	else
		bad "fixtures"
		echo "SUMMARY: FAILED"
		exit 1
	fi
fi

# ------------------------------------------------------------------
# (b) plumbing identity: raw through /bin/cat is byte-identical
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	if "$BIN" --bench /bin/cat < "$TMP/raw.f32" > "$TMP/echo.f32" 2>/dev/null &&
		cmp -s "$TMP/raw.f32" "$TMP/echo.f32"; then
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
	if "$KUJAMBA" render --phrase "kujamba karibu" --seed 42 \
		--out "$TMP/t_in.wav" >/dev/null 2>&1; then
		if "$BIN" --effect fuzz < "$TMP/t_in.wav" > "$TMP/t_out.wav" \
			2>/dev/null; then
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
#     Gated: ~12 GiB streams through the pipes, tens of seconds —
#     streaming made the RAM side cheap, the I/O is still real.
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
	# 4 GiB of float32 stereo is 2 GiB of s16 — legal, must not die
	"$BIN" --bench "$TMP/gen4g.sh" < "$TMP/tiny48s.wav" > /dev/null \
		2> "$TMP/under4g.err"
	rc=$?
	if [ $rc -eq 0 ]; then
		ok "(q) 4 GiB wrap: legal 2 GiB result streams through"
	else
		bad "(q) 4 GiB wrap: legal result rc=$rc, stderr: $(head -1 "$TMP/under4g.err")"
	fi
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
	skip "(q) 4 GiB wrap: set BROWNNOTE_HEAVY=1 (~12 GiB of I/O, tens of seconds)"
fi

# ------------------------------------------------------------------
# (r) data chunk of declared size 0: empty, not "runs to EOF" —
#     trailing metadata chunks must not come out as audio (#8)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	python3 - "$TMP" <<'PYEOF'
import struct, sys, os
def chunk(cid, body):
    out = cid + struct.pack('<I', len(body)) + body
    if len(body) & 1: out += b'\0'
    return out
fmt = chunk(b'fmt ', struct.pack('<HHIIHH', 1, 1, 44100, 88200, 2, 16))
wav = (b'RIFF' + struct.pack('<I', 4 + len(fmt) + 8 + 28) + b'WAVE' + fmt
       + b'data' + struct.pack('<I', 0)
       + chunk(b'LIST', b'INFO' + chunk(b'INAM', b'12345678')))
open(os.path.join(sys.argv[1], 'data0_list.wav'), 'wb').write(wav)
PYEOF
	"$BIN" --bench /bin/cat < "$TMP/data0_list.wav" > "$TMP/data0_out.wav" \
		2>/dev/null
	rc=$?
	outsize=$(wc -c < "$TMP/data0_out.wav" | tr -d ' ')
	if [ $rc -eq 0 ] && [ "$outsize" = "44" ]; then
		ok "(r) size-0 data chunk: empty result, metadata not played"
	else
		bad "(r) size-0 data chunk: rc=$rc, $outsize bytes out (metadata as audio?)"
	fi
else
	skip "(r) size-0 data chunk: python3 absent"
fi

# ------------------------------------------------------------------
# (s) a bench path that cannot be executed says so, instead of a
#     silent exit 127 (#9)
# ------------------------------------------------------------------
"$BIN" --bench /nonexistent-brownnote-test < /dev/null > /dev/null \
	2> "$TMP/exec1.err"
rc1=$?
BROWNNOTE_BENCH=/nonexistent-brownnote-test "$BIN" < /dev/null \
	> /dev/null 2> "$TMP/exec2.err"
rc2=$?
if [ $rc1 -eq 127 ] && [ $rc2 -eq 127 ] && \
   grep -q "cannot exec /nonexistent-brownnote-test" "$TMP/exec1.err" && \
   grep -q "cannot exec /nonexistent-brownnote-test" "$TMP/exec2.err"; then
	ok "(s) exec failure: exit 127 with the path and reason on stderr"
else
	bad "(s) exec failure: rc1=$rc1 rc2=$rc2, stderr1: $(head -1 "$TMP/exec1.err")"
fi

# ------------------------------------------------------------------
# (t) stdout write errors that are not EPIPE — disk full, file size
#     limit — exit 1, not the documented 141 "downstream closed" (#10)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	( trap '' XFSZ; ulimit -f 2; exec "$BIN" --bench /bin/cat \
		< "$TMP/raw.f32" > "$TMP/limited.bin" ) 2> "$TMP/efbig.err"
	rc=$?
	if [ $rc -eq 1 ] && grep -q "write stdout" "$TMP/efbig.err"; then
		ok "(t) EFBIG on stdout: exit 1, not a fake 141"
	else
		bad "(t) EFBIG on stdout: rc=$rc (141 = still masquerading as SIGPIPE), stderr: $(head -1 "$TMP/efbig.err")"
	fi
else
	skip "(t) EFBIG on stdout: python3 absent"
fi

# ------------------------------------------------------------------
# (u) WAV mode, downstream closes: exit 141 like raw mode, with the
#     errno, not a generic exit-1 "short write" (#11)
# ------------------------------------------------------------------
if [ "$have_bench" = 1 ] && [ "$have_python" = 1 ]; then
	"$BIN" < "$TMP/in.wav" 2> "$TMP/wavepipe.err" | head -c 10 \
		> /dev/null
	rc=${PIPESTATUS[0]}
	if [ "$rc" -eq 141 ] && grep -q "Broken pipe" "$TMP/wavepipe.err"; then
		ok "(u) WAV-mode EPIPE: exit 141, errno in the message"
	else
		bad "(u) WAV-mode EPIPE: rc=$rc (want 141), stderr: $(tail -1 "$TMP/wavepipe.err")"
	fi
elif [ "$have_bench" = 0 ]; then
	skip "(u) WAV-mode EPIPE: bench not built ($BENCH)"
else
	skip "(u) WAV-mode EPIPE: python3 absent"
fi

# ------------------------------------------------------------------
# (v) targeted SIGINT with a shell-script bench: the shell defers
#     SIGINT while waiting on its own children, so the handler must
#     not block on it — grace period, then SIGKILL (#23)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	printf '#!/bin/sh\nsleep 30\n' > "$TMP/slowsh.sh"
	chmod +x "$TMP/slowsh.sh"
	"$BIN" --bench "$TMP/slowsh.sh" < "$TMP/raw.f32" > /dev/null \
		2>/dev/null &
	sig_pid=$!
	sleep 0.5			# let it spawn the bench
	kill -INT $sig_pid 2>/dev/null
	for i in $(seq 1 50); do	# 5 s bound
		kill -0 $sig_pid 2>/dev/null || break
		sleep 0.1
	done
	if kill -0 $sig_pid 2>/dev/null; then
		kill -9 $sig_pid 2>/dev/null
		wait $sig_pid 2>/dev/null
		pkill -9 -f "$TMP/slowsh.sh" 2>/dev/null
		bad "(v) SIGINT with shell bench: handler blocked, killed"
	else
		wait $sig_pid 2>/dev/null
		rc=$?
		if [ $rc -eq 130 ]; then
			ok "(v) SIGINT with shell bench: exit 130, no block"
		else
			bad "(v) SIGINT with shell bench: rc=$rc (want 130)"
		fi
	fi
else
	skip "(v) SIGINT with shell bench: python3 absent"
fi

# ------------------------------------------------------------------
# (w) downsample anti-aliasing: a tone above the output Nyquist must
#     be filtered out, not folded back into the passband (#32)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	verdict=$(python3 - "$BIN" <<'PYEOF'
import array, math, os, struct, subprocess, sys, tempfile, wave

bin_ = sys.argv[1]
SR = 192000					# input rate
N = SR // 2					# 0.5 s
# 30 kHz aliases to 18 kHz when dropped to the bench's 48 kHz;
# 10 kHz is the in-band control that must survive the trip.
frames = bytearray()
for i in range(N):
    t = i / SR
    v = 0.4 * math.sin(2 * math.pi * 30000 * t) \
      + 0.4 * math.sin(2 * math.pi * 10000 * t)
    frames += struct.pack('<h', int(v * 32767))
fd, path = tempfile.mkstemp(suffix='.wav')
os.close(fd)
w = wave.open(path, 'wb')
w.setnchannels(1); w.setsampwidth(2); w.setframerate(SR)
w.writeframes(bytes(frames)); w.close()

with open(path, 'rb') as f:
    r = subprocess.run([bin_, '--bench', '/bin/cat'], stdin=f,
                       capture_output=True, timeout=120)
os.unlink(path)
if r.returncode != 0:
    raise SystemExit(f"bad: rc={r.returncode} err={r.stderr[:80]!r}")

b = r.stdout
if len(b) < 44 or b[0:4] != b'RIFF':
    raise SystemExit("bad: output is not a WAV")
a = array.array('h')
a.frombytes(b[44:44 + struct.unpack('<I', b[40:44])[0]])
vals = [x / 32768.0 for x in a]

def amp(f):
    # Goertzel amplitude of frequency f in vals
    k = 2.0 * math.cos(2 * math.pi * f / SR)
    s1 = s2 = 0.0
    for x in vals:
        s0 = x + k * s1 - s2
        s2, s1 = s1, s0
    return 2.0 * math.sqrt(max(s1 * s1 + s2 * s2 - k * s1 * s2, 0.0)) \
        / len(vals)

a30, a18, a10 = amp(30000), amp(18000), amp(10000)
if a10 < 0.30:
    raise SystemExit(f"bad: in-band 10 kHz control lost (amp {a10:.4f})")
if a18 > 0.01:
    raise SystemExit(f"bad: 30 kHz folded to 18 kHz at amp {a18:.4f}")
if a30 > 0.01:
    raise SystemExit(f"bad: 30 kHz passed through at amp {a30:.4f}")
print(f"ok: 10 kHz control {a10:.4f}, alias {a18:.4f}, 30 kHz {a30:.4f}")
PYEOF
)
	case "$verdict" in
	ok:*) ok "(w) anti-aliasing: $verdict" ;;
	*)   bad "(w) anti-aliasing: $verdict" ;;
	esac
else
	skip "(w) anti-aliasing: python3 absent"
fi

# ------------------------------------------------------------------
# (x) streaming output header: a pipe keeps the 0xFFFFFFFF streaming
#     sizes; a regular file gets them backfilled at the end, so a
#     seekable result is byte-exact (#28)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	"$BIN" --bench /bin/cat < "$TMP/in.wav" 2>/dev/null |
		head -c 44 > "$TMP/pipe.hdr"
	"$BIN" --bench /bin/cat < "$TMP/in.wav" > "$TMP/file.wav" \
		2>/dev/null
	verdict=$(python3 - "$TMP/pipe.hdr" "$TMP/file.wav" <<'PYEOF'
import os, sys
hdr = open(sys.argv[1], 'rb').read()
path = sys.argv[2]
b = open(path, 'rb').read(44)
size = os.path.getsize(path)
if len(hdr) != 44:
    raise SystemExit("bad: short piped header")
riff = int.from_bytes(hdr[4:8], 'little')
data = int.from_bytes(hdr[40:44], 'little')
if riff != 0xFFFFFFFF or data != 0xFFFFFFFF:
    raise SystemExit(
        f"bad: piped sizes riff={riff:#x} data={data:#x}")
if len(b) != 44:
    raise SystemExit("bad: short file header")
riff = int.from_bytes(b[4:8], 'little')
data = int.from_bytes(b[40:44], 'little')
if riff != size - 8 or data != size - 44:
    raise SystemExit(
        f"bad: backfill riff={riff} data={data} for a {size}-byte file")
if b[0:4] != b'RIFF' or b[8:12] != b'WAVE':
    raise SystemExit("bad: not a RIFF/WAVE")
print("ok")
PYEOF
)
	if [ "$verdict" = "ok" ]; then
		ok "(x) WAV header: pipe keeps FFFFFFFF sizes, file backfilled exact"
	else
		bad "(x) WAV header: $verdict"
	fi
else
	skip "(x) WAV header: python3 absent"
fi

# ------------------------------------------------------------------
# (y) a big WAV through a pipe: the input streams in blocks, so a
#     file much larger than memory must come through unchanged.  A
#     64 MiB address space makes whole-file buffering impossible
#     where the limit is enforced (probed first: macOS ignores it,
#     sanitizer builds need more room) (#28)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ]; then
	python3 - "$TMP/big.wav" <<'PYEOF'
import math, struct, sys
SR, SECS = 192000, 20			# stereo 192 kHz s16: 30 MB
blk = bytearray()
for i in range(SR):
    v = int(6000 * math.sin(2 * math.pi * 440 * i / SR))
    blk += struct.pack('<hh', v, v)
with open(sys.argv[1], 'wb') as f:
    f.write(b'RIFF' + struct.pack('<I', 36 + len(blk) * SECS) + b'WAVE')
    f.write(b'fmt ' + struct.pack('<IHHIIHH', 16, 1, 2, SR, SR * 4, 4, 16))
    f.write(b'data' + struct.pack('<I', len(blk) * SECS))
    for _ in range(SECS):
        f.write(blk)
PYEOF
	"$BIN" --bench /bin/cat < "$TMP/big.wav" \
		> "$TMP/big_file.wav" 2>/dev/null
	rc1=$?
	# shellcheck disable=SC2002  # stdin being a pipe is the point
	cat "$TMP/big.wav" | "$BIN" --bench /bin/cat \
		> "$TMP/big_pipe.wav" 2>/dev/null
	rc2=$?
	bounded=-1
	if ( ulimit -v 65536 2>/dev/null || true
	     "$BIN" --bench /bin/cat < /dev/null > /dev/null 2>&1 ); then
		if ( ulimit -v 65536 2>/dev/null || true
		     "$BIN" --bench /bin/cat < "$TMP/big.wav" \
			> "$TMP/big_lim.wav" 2>/dev/null ) &&
		    cmp -s "$TMP/big_file.wav" "$TMP/big_lim.wav"; then
			bounded=1
		else
			bounded=0
		fi
	fi
	if [ $rc1 -eq 0 ] && [ $rc2 -eq 0 ] && [ $bounded -ne 0 ] && \
	   cmp -s "$TMP/big_file.wav" "$TMP/big_pipe.wav"; then
		ok "(y) large WAV: 30 MB streams in blocks, pipe == file"
	else
		bad "(y) large WAV: rc=$rc1/$rc2 bounded=$bounded or mismatched output"
	fi
else
	skip "(y) large WAV: python3 absent"
fi

# ------------------------------------------------------------------
# (z) the README alias table is generated from aliases[] in
#     src/brownnote.c — regenerate and diff so docs cannot drift (#27)
# ------------------------------------------------------------------
if [ -x scripts/gen_alias_table.sh ]; then
	scripts/gen_alias_table.sh src/brownnote.c > "$TMP/alias_table.md"
	sed -n '/^### Effect aliases/,/^Exact bench names/p' README.md |
		grep '^|' > "$TMP/readme_table.md" || true
	if cmp -s "$TMP/alias_table.md" "$TMP/readme_table.md"; then
		ok "(z) alias table: README matches aliases[]"
	else
		bad "(z) alias table: README drifted (regenerate with scripts/gen_alias_table.sh):"
		diff "$TMP/readme_table.md" "$TMP/alias_table.md" | head -8
	fi
else
	skip "(z) alias table: scripts/gen_alias_table.sh missing"
fi

# ------------------------------------------------------------------
# (aa) every alias target in aliases[] must still name a routable
#      bench effect — a GuitarPedal rename fails here, not silently
#      at the next --effect invocation (#27)
# ------------------------------------------------------------------
if [ "$have_bench" = 1 ]; then
	"$BIN" --bench "$BENCH" --list > "$TMP/list.out" 2>&1
	missing=$(awk -F'"' '/^\t\{ "[^"]+",\t*"[^"]+" \},/ { print $4 }' \
		src/brownnote.c | sort -u | while IFS= read -r eff; do
			grep -qF "$eff" "$TMP/list.out" || echo "$eff"
		done)
	if [ -z "$missing" ]; then
		ok "(aa) alias targets: every name still in the bench's --list"
	else
		bad "(aa) alias targets missing from --list: $missing"
	fi
else
	skip "(aa) alias targets: bench not built ($BENCH)"
fi

# ------------------------------------------------------------------
# (ab) presets.sh smoke test: list, fx render, and multi-track song
# ------------------------------------------------------------------
if [ "$have_python" = 1 ] && [ -x scripts/presets.sh ]; then
	if scripts/presets.sh list >/dev/null 2>&1 &&
	   scripts/presets.sh fx doom --out "$TMP/preset_fx.wav" >/dev/null 2>&1 &&
	   scripts/presets.sh song funk --bars 2 --out "$TMP/preset_song.wav" >/dev/null 2>&1 &&
	   [ -s "$TMP/preset_fx.wav" ] && [ -s "$TMP/preset_song.wav" ]; then
		ok "(ab) presets.sh: fx and multi-track song WAVs rendered"
	else
		bad "(ab) presets.sh: failed to render fx or song WAV"
	fi
else
	skip "(ab) presets.sh: python3 or scripts/presets.sh absent"
fi

# ------------------------------------------------------------------
# (ac) studio.py smoke test: all five pages serve HTML, /api/state
#      answers JSON (200, or 503 when stems can't build here), an
#      unknown POST endpoint 404s as JSON, the #45 patch API round-trips
#      save -> list -> load -> delete with bad names 400ing, the #43
#      sample API round-trips a 44.1 kHz s16 WAV with bad formats 400ing
#      and over-cap bodies 413ing, — when stems are available —
#      /api/render-sequence honours length-16 (repeat per bar) and
#      length-16*bars (flat looper grid) step arrays, the #47
#      shared q() block is evaluated under node when node is present,
#      and the #49 /api/jam bridge runs a fake kujamba through
#      join -> CONFIG tail -> loop.wav swap -> stop (incl. a
#      non-multiple-of-4 bpi and a missing-binary boot)
# ------------------------------------------------------------------
if [ "$have_python" = 1 ] && [ -f scripts/studio.py ]; then
	verdict=$(python3 tests/studio_smoke.py 2>&1)
	case "$verdict" in
	ok:*) ok "(ac) studio pages: $verdict" ;;
	*)   bad "(ac) studio pages: $verdict" ;;
	esac
else
	skip "(ac) studio pages: python3 or scripts/studio.py absent"
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
