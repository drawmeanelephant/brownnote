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
if [ "$have_python" = 1 ]; then
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
	args=""
	for i in $(seq 1 256); do args="$args --effect fuzz"; done
	for i in $(seq 1 256); do args="$args --pot \"Rat Sketch:Distortion=60\""; done
	for i in $(seq 1 256); do args="$args --mix \"Boost=60\""; done
	eval "timeout 10 \"$BIN\" --bench /bin/cat $args < /dev/null" \
		> /dev/null 2> "$TMP/ovf.err"
	rc=$?
	if [ $rc -eq 0 ] || [ $rc -eq 1 ]; then
		ok "(k) argv overflow: 768 flags, exit $rc, no crash"
	else
		bad "(k) argv overflow: exit $rc (abort/segv?)"
	fi
else
	skip "(k) argv overflow: python3 absent"
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
