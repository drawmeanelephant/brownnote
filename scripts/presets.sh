#!/usr/bin/env bash
#
# presets.sh — preset pedalboard & multi-track song generator for brownnote.
#
# Synthesizes farts with `kujamba` (or a built-in Python formant/flutter
# fallback when `kujamba` is not present), routes them through Linus
# Torvalds' GuitarPedal bench via `brownnote`, and writes WAV or 320 kbps MP3.
#
# Usage:
#   scripts/presets.sh list
#   scripts/presets.sh fx <preset> [--phrase TEXT] [--seed N] [--out FILE] [--play]
#   scripts/presets.sh song <genre> [--bpm N] [--bars N] [--seed N] [--out FILE] [--play]
#   scripts/presets.sh stems [--out-dir DIR] [--seed N]
#   scripts/presets.sh pack [--out-dir DIR] [--play]
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BN="${BROWNNOTE:-$ROOT/build/brownnote}"
BENCH="${BENCH:-$ROOT/vendor/GuitarPedal/Validation/bench/bench}"

find_kujamba() {
	if [ -n "${KUJAMBA:-}" ] && [ -x "$KUJAMBA" ]; then
		echo "$KUJAMBA"; return
	fi
	for cand in \
		"$ROOT/../fart-app/zig-out/bin/kujamba" \
		"$ROOT/.refs/fart-app/zig-out/bin/kujamba" \
		"$(command -v kujamba 2>/dev/null || true)"; do
		if [ -n "$cand" ] && [ -x "$cand" ]; then
			echo "$cand"; return
		fi
	done
	echo ""
}

# Synthesize a phrase to WAV using kujamba when available, or a deterministic
# Python flutter/formant synth fallback so presets & tests work standalone.
synth_phrase() {
	local phrase="$1"
	local voice="$2"
	local seed="$3"
	local out_wav="$4"
	local kujamba
	kujamba="$(find_kujamba)"

	if [ -n "$kujamba" ]; then
		"$kujamba" render --phrase "$phrase" --voice "$voice" --seed "$seed" --out "$out_wav" >/dev/null
		return
	fi

	python3 - "$phrase" "$voice" "$seed" "$out_wav" <<'PYEOF'
import math, random, struct, sys, wave

phrase, voice, seed_s, out_wav = sys.argv[1:5]
rng = random.Random(int(seed_s) ^ hash(phrase))
SR = 44100

# Parse voice multipliers (noise=...,wobble=...,attack=...)
kv = {"noise": 1.0, "wobble": 1.0, "attack": 1.0}
for part in voice.split(","):
    if "=" in part:
        k, v = part.split("=", 1)
        try:
            kv[k.strip()] = float(v)
        except ValueError:
            pass

words = [w for w in phrase.split() if w] or ["bubu"]
pcm = bytearray()
for w in words:
    dur = max(0.18, min(0.75, 0.09 * len(w)))
    n = int(dur * SR)
    f0 = 62.0 + (sum(ord(c) for c in w) % 45)
    wob_hz = 14.0 + (rng.random() * 12.0) * kv["wobble"]
    phase = 0.0
    for i in range(n):
        t = i / SR
        att = min(1.0, t / max(0.004, 0.025 / max(0.2, kv["attack"])))
        rel = math.exp(-3.2 * t / dur)
        env = att * rel
        wob = 1.0 + 0.22 * kv["wobble"] * math.sin(2 * math.pi * wob_hz * t)
        phase += 2 * math.pi * (f0 * wob) / SR
        # Rich brassy pulse + sub + flutter noise
        sig = (
            0.55 * math.sin(phase)
            + 0.30 * math.sin(2 * phase + 0.4)
            + 0.18 * math.sin(3 * phase)
            + 0.12 * kv["noise"] * (rng.random() * 2.0 - 1.0) * (0.5 + 0.5 * math.sin(phase))
        )
        v = max(-1.0, min(1.0, 0.65 * env * sig))
        pcm += struct.pack("<h", int(v * 32767))
    # short inter-word gap
    pcm += b"\x00\x00" * int(0.04 * SR)

with wave.open(out_wav, "wb") as wf:
    wf.setnchannels(1)
    wf.setsampwidth(2)
    wf.setframerate(SR)
    wf.writeframes(bytes(pcm))
PYEOF
}

usage() {
	cat <<'EOF'
usage:
  scripts/presets.sh list
  scripts/presets.sh fx <preset> [options]
  scripts/presets.sh song <genre> [options]
  scripts/presets.sh stems [--out-dir DIR] [--seed N]
  scripts/presets.sh pack [--out-dir DIR] [--play]

FX PRESETS (single-shot phrase + pedalboard):
  doom          Sub-octave Pitch -> maxed Rat Sketch fuzz -> driven Cabinet
  cathedral     Auto-Wah -> Phaser -> wobbly Tape Echo -> massive Reverb
  machinegun    Compressor -> Klonlike OD -> Frenchie Amp -> choppy Tremolo
  swamp         Sloppy Vibrato -> deep Flanger -> Rat Sketch mode 2
  klon-crunch   Boost -> Klonlike overdrive -> Frenchie Amp -> Cabinet
  cyber-laser   High-Q Auto-Wah -> Pitch +1 oct -> Flanger -> Tape Echo
  dry           Pure unprocessed kujamba fart

SONG GENRES (multi-track arranged & mixed stereo songs -> WAV / MP3):
  funk          116 BPM P-Funk bassline, crisp splat backbeat, wah riff & fuzz solo
  metal         168 BPM double-kick blast, chugging Rat/Frenchie guitars & shred lead
  doom          66 BPM tectonic sub-bass drone, cathedral reverb & sludge chords
  techno        132 BPM four-on-the-floor sub-kick, acid wah 303 squelch & delay hats

OPTIONS:
  --phrase TEXT   Swahili phrase for `fx` mode (default: preset's signature phrase)
  --seed N        Determinism seed (default: preset's signature seed)
  --bpm N         Override tempo for `song` mode
  --bars N        Number of 4-beat bars for `song` mode (default: 8)
  --out FILE      Output path (.wav or .mp3; default: build/presets/<name>.mp3)
  --play          Play the result immediately via afplay / ffplay
EOF
}

play_file() {
	local f="$1"
	if command -v afplay >/dev/null 2>&1; then
		echo "▶ Playing $f ..."
		afplay "$f"
	elif command -v ffplay >/dev/null 2>&1; then
		echo "▶ Playing $f ..."
		ffplay -nodisp -autoexit -loglevel quiet "$f"
	else
		echo "  (no afplay/ffplay found to auto-play $f)"
	fi
}

encode_output() {
	local src_wav="$1"
	local dst="$2"
	local title="${3:-Brownnote Preset}"
	mkdir -p "$(dirname "$dst")"
	case "$dst" in
		*.mp3)
			if ! command -v ffmpeg >/dev/null 2>&1; then
				echo "presets.sh: ffmpeg required for .mp3 output" >&2
				exit 2
			fi
			ffmpeg -y -loglevel error -i "$src_wav" \
				-codec:a libmp3lame -b:a 320k \
				-metadata title="$title" \
				-metadata artist="kujamba x GuitarPedal" \
				"$dst"
			;;
		*.wav)
			cp "$src_wav" "$dst"
			;;
		*)
			echo "presets.sh: output file must end in .wav or .mp3 (got $dst)" >&2
			exit 2
			;;
	esac
}

run_fx() {
	local preset="$1"; shift
	local phrase="" seed="" out="" do_play=0
	while [ $# -gt 0 ]; do
		case "$1" in
			--phrase) phrase="$2"; shift 2 ;;
			--seed)   seed="$2"; shift 2 ;;
			--out)    out="$2"; shift 2 ;;
			--play)   do_play=1; shift ;;
			-h|--help) usage; exit 0 ;;
			*) echo "presets.sh fx: unknown option '$1'" >&2; exit 2 ;;
		esac
	done

	[ -x "$BN" ] || make -s -C "$ROOT"

	local def_phrase def_voice def_seed
	local -a bn_args=()

	case "$preset" in
		doom)
			def_phrase="shuzi kubwa sana la radi"
			def_voice="noise=1.2,wobble=2.8,attack=0.5"
			def_seed=7
			bn_args=(
				--effect pitch --pot "Pitch:Octave=20"
				--effect fuzz --pot "Rat Sketch:Distortion=115" --pot "Rat Sketch:Filter=30"
				--effect cabinet --pot "Cabinet:Drive=90"
			)
			;;
		cathedral)
			def_phrase="harufu mbaya angani"
			def_voice="noise=2.0,wobble=1.8,attack=1.7"
			def_seed=42
			bn_args=(
				--effect wah --pot "Auto-Wah:Resonance=105"
				--effect phaser --pot "Phaser:Feedback=95"
				--effect delay --pot "Tape Echo:Blend=85" --pot "Tape Echo:Sustain=90" --pot "Tape Echo:WowFlut=75"
				--effect reverb --pot "Reverb:Room=115" --mix Reverb=95
			)
			;;
		machinegun)
			def_phrase="piga kelele tumboni"
			def_voice="noise=0.7,wobble=3.0,attack=2.0"
			def_seed=108
			bn_args=(
				--effect compressor --pot "Compressor:Ratio=100"
				--effect overdrive --pot "Klonlike:Gain=110"
				--effect amp --pot "Frenchie Amp:Gain=105" --pot "Frenchie Amp:Sag=95"
				--effect tremolo --pot "Tremolo:Rate=95" --pot "Tremolo:Depth=110"
			)
			;;
		swamp)
			def_phrase="mamba anakula maharage"
			def_voice="noise=2.5,wobble=3.5,attack=0.4"
			def_seed=666
			bn_args=(
				--effect vibrato --pot "Vibrato:Rate=85" --pot "Vibrato:Depth=95"
				--effect flanger --pot "Flanger:Depth=110" --pot "Flanger:Feedback=100"
				--effect fuzz --pot "Rat Sketch:Distortion=80" --pot "Rat Sketch:Mode=2"
			)
			;;
		klon-crunch)
			def_phrase="kujamba kwa nguvu sana"
			def_voice="noise=0.8,wobble=1.6,attack=1.5"
			def_seed=55
			bn_args=(
				--effect boost --pot "Boost:Boost=75" --pot "Boost:Basscut=40"
				--effect overdrive --pot "Klonlike:Gain=95" --pot "Klonlike:Treble=80" --pot "Klonlike:Output=90"
				--effect amp --pot "Frenchie Amp:Gain=75" --pot "Frenchie Amp:Level=60"
				--effect cabinet --pot "Cabinet:Cabinet=3" --pot "Cabinet:Drive=70"
				--effect reverb --pot "Reverb:Room=50" --mix Reverb=30
			)
			;;
		cyber-laser)
			def_phrase="nyota za kijani"
			def_voice="noise=0.3,wobble=4.0,attack=2.4"
			def_seed=303
			bn_args=(
				--effect wah --pot "Auto-Wah:Sens=110" --pot "Auto-Wah:Resonance=115" --pot "Auto-Wah:Range=115"
				--effect pitch --pot "Pitch:Octave=95" --pot "Pitch:Feedback=85"
				--effect flanger --pot "Flanger:Freq=90" --pot "Flanger:Depth=105" --pot "Flanger:Feedback=95"
				--effect delay --pot "Tape Echo:Time=45" --pot "Tape Echo:Sustain=75" --pot "Tape Echo:Blend=65"
			)
			;;
		dry)
			def_phrase="kujamba karibu"
			def_voice="noise=1.0,wobble=1.0,attack=1.0"
			def_seed=1
			bn_args=()
			;;
		*)
			echo "presets.sh fx: unknown preset '$preset' (run 'scripts/presets.sh list')" >&2
			exit 2
			;;
	esac

	[ -n "$phrase" ] || phrase="$def_phrase"
	[ -n "$seed" ]   || seed="$def_seed"
	if [ -z "$out" ]; then
		if command -v ffmpeg >/dev/null 2>&1; then
			out="$ROOT/build/presets/fx_${preset}.mp3"
		else
			out="$ROOT/build/presets/fx_${preset}.wav"
		fi
	fi

	local tmp
	tmp="$(mktemp -d)"

	synth_phrase "$phrase" "$def_voice" "$seed" "$tmp/dry.wav"
	if [ "${#bn_args[@]}" -eq 0 ] || [ ! -x "$BENCH" ]; then
		cp "$tmp/dry.wav" "$tmp/wet.wav"
	else
		"$BN" --bench "$BENCH" "${bn_args[@]}" < "$tmp/dry.wav" > "$tmp/wet.wav" 2>/dev/null
	fi

	encode_output "$tmp/wet.wav" "$out" "brownnote fx: $preset ($phrase)"
	rm -rf "$tmp"
	echo "✓ Rendered fx '$preset' -> $out"
	if [ "$do_play" = 1 ]; then
		play_file "$out"
	fi
}

run_stems() {
	local out_dir="$ROOT/build/tune" seed=10
	while [ $# -gt 0 ]; do
		case "$1" in
			--out-dir) out_dir="$2"; shift 2 ;;
			--seed)    seed="$2"; shift 2 ;;
			-h|--help) usage; exit 0 ;;
			*) echo "presets.sh stems: unknown option '$1'" >&2; exit 2 ;;
		esac
	done

	[ -x "$BN" ] || make -s -C "$ROOT"
	mkdir -p "$out_dir"

	synth_phrase "bubu" "noise=0.4,wobble=0.8,attack=1.8" "$((seed + 1))" "$out_dir/stem_bass_dry.wav"
	synth_phrase "pa" "noise=2.8,wobble=1.2,attack=2.5" "$((seed + 12))" "$out_dir/stem_snare_dry.wav"
	synth_phrase "kujamba karibu sasa" "noise=0.9,wobble=2.2,attack=1.4" "$((seed + 23))" "$out_dir/stem_funk_dry.wav"
	synth_phrase "shuzi la moto sana" "noise=0.6,wobble=1.5,attack=1.2" "$((seed + 67))" "$out_dir/stem_lead_dry.wav"
	synth_phrase "mamba mkubwa" "noise=1.5,wobble=3.0,attack=0.6" "$((seed + 89))" "$out_dir/stem_bomb_dry.wav"

	if [ -x "$BENCH" ]; then
		"$BN" --bench "$BENCH" \
		      --effect compressor --pot "Compressor:Ratio=95" --pot "Compressor:Boost=50" \
		      --effect pitch --pot "Pitch:Octave=25" --pot "Pitch:Feedback=20" \
		      --effect overdrive --pot "Klonlike:Gain=75" --pot "Klonlike:Output=85" \
		      --effect cabinet --pot "Cabinet:Cabinet=2" --pot "Cabinet:Drive=70" \
		      < "$out_dir/stem_bass_dry.wav" > "$out_dir/stem_bass_sub.wav" 2>/dev/null

		"$BN" --bench "$BENCH" \
		      --effect compressor --pot "Compressor:Ratio=110" --pot "Compressor:Attack=5" \
		      --effect boost --pot "Boost:Boost=80" --pot "Boost:Basscut=90" \
		      --effect reverb --pot "Reverb:Room=55" --mix Reverb=45 \
		      < "$out_dir/stem_snare_dry.wav" > "$out_dir/stem_snare.wav" 2>/dev/null

		"$BN" --bench "$BENCH" \
		      --effect wah --pot "Auto-Wah:Sens=95" --pot "Auto-Wah:Resonance=105" --pot "Auto-Wah:Range=105" \
		      --effect phaser --pot "Phaser:LFO=75" --pot "Phaser:Feedback=85" \
		      --effect overdrive --pot "Klonlike:Gain=65" \
		      < "$out_dir/stem_funk_dry.wav" > "$out_dir/stem_funk_wah.wav" 2>/dev/null

		"$BN" --bench "$BENCH" \
		      --effect fuzz --pot "Rat Sketch:Distortion=105" --pot "Rat Sketch:Filter=45" --pot "Rat Sketch:Volume=85" \
		      --effect flanger --pot "Flanger:Depth=70" --pot "Flanger:Feedback=65" \
		      --effect delay --pot "Tape Echo:Blend=65" --pot "Tape Echo:Sustain=75" --pot "Tape Echo:Time=80" \
		      --effect reverb --pot "Reverb:Room=95" --mix Reverb=55 \
		      < "$out_dir/stem_lead_dry.wav" > "$out_dir/stem_lead_fuzz.wav" 2>/dev/null

		"$BN" --bench "$BENCH" \
		      --effect pitch --pot "Pitch:Octave=15" --pot "Pitch:Feedback=70" \
		      --effect fuzz --pot "Rat Sketch:Distortion=118" --pot "Rat Sketch:Mode=1" \
		      --effect delay --pot "Tape Echo:Blend=80" --pot "Tape Echo:Sustain=95" \
		      --effect reverb --pot "Reverb:Room=118" --mix Reverb=90 \
		      < "$out_dir/stem_bomb_dry.wav" > "$out_dir/stem_bomb.wav" 2>/dev/null
	else
		cp "$out_dir/stem_bass_dry.wav"  "$out_dir/stem_bass_sub.wav"
		cp "$out_dir/stem_snare_dry.wav" "$out_dir/stem_snare.wav"
		cp "$out_dir/stem_funk_dry.wav"  "$out_dir/stem_funk_wah.wav"
		cp "$out_dir/stem_lead_dry.wav"  "$out_dir/stem_lead_fuzz.wav"
		cp "$out_dir/stem_bomb_dry.wav"  "$out_dir/stem_bomb.wav"
	fi
	echo "✓ Rendered 5 dry + 5 pedaled stems -> $out_dir"
}

run_song() {
	local genre="$1"; shift
	local bpm="" bars=8 seed=100 out="" do_play=0
	while [ $# -gt 0 ]; do
		case "$1" in
			--bpm)  bpm="$2"; shift 2 ;;
			--bars) bars="$2"; shift 2 ;;
			--seed) seed="$2"; shift 2 ;;
			--out)  out="$2"; shift 2 ;;
			--play) do_play=1; shift ;;
			-h|--help) usage; exit 0 ;;
			*) echo "presets.sh song: unknown option '$1'" >&2; exit 2 ;;
		esac
	done

	case "$genre" in
		funk)   [ -n "$bpm" ] || bpm=116 ;;
		metal)  [ -n "$bpm" ] || bpm=168 ;;
		doom)   [ -n "$bpm" ] || bpm=66  ;;
		techno) [ -n "$bpm" ] || bpm=132 ;;
		*)
			echo "presets.sh song: unknown genre '$genre' (choose: funk, metal, doom, techno)" >&2
			exit 2
			;;
	esac

	if [ -z "$out" ]; then
		if command -v ffmpeg >/dev/null 2>&1; then
			out="$ROOT/build/presets/song_${genre}.mp3"
		else
			out="$ROOT/build/presets/song_${genre}.wav"
		fi
	fi

	[ -x "$BN" ] || make -s -C "$ROOT"

	local tmp
	tmp="$(mktemp -d)"

	echo "⚙ Synthesizing & pedaling stems for '$genre' ($bpm BPM, $bars bars)..."

	synth_phrase "bubu" "noise=0.4,wobble=0.8,attack=1.9" "$((seed + 1))" "$tmp/bass_dry.wav"
	synth_phrase "pa" "noise=2.8,wobble=1.2,attack=2.5" "$((seed + 2))" "$tmp/snare_dry.wav"
	synth_phrase "kujamba karibu sasa" "noise=0.9,wobble=2.2,attack=1.5" "$((seed + 3))" "$tmp/riff_dry.wav"
	synth_phrase "shuzi la moto sana" "noise=0.6,wobble=1.7,attack=1.3" "$((seed + 4))" "$tmp/lead_dry.wav"
	synth_phrase "mamba mkubwa" "noise=1.5,wobble=3.0,attack=0.6" "$((seed + 5))" "$tmp/bomb_dry.wav"

	if [ -x "$BENCH" ]; then
		"$BN" --bench "$BENCH" \
		      --effect compressor --pot "Compressor:Ratio=95" --pot "Compressor:Boost=55" \
		      --effect pitch --pot "Pitch:Octave=22" --pot "Pitch:Feedback=20" \
		      --effect overdrive --pot "Klonlike:Gain=80" --pot "Klonlike:Output=85" \
		      --effect cabinet --pot "Cabinet:Cabinet=2" --pot "Cabinet:Drive=75" \
		      < "$tmp/bass_dry.wav" > "$tmp/bass.wav" 2>/dev/null

		"$BN" --bench "$BENCH" \
		      --effect compressor --pot "Compressor:Ratio=110" --pot "Compressor:Attack=5" \
		      --effect boost --pot "Boost:Boost=85" --pot "Boost:Basscut=90" \
		      --effect reverb --pot "Reverb:Room=55" --mix Reverb=40 \
		      < "$tmp/snare_dry.wav" > "$tmp/snare.wav" 2>/dev/null

		if [ "$genre" = "metal" ]; then
			"$BN" --bench "$BENCH" \
			      --effect boost --pot "Boost:Boost=90" --pot "Boost:Basscut=75" \
			      --effect fuzz --pot "Rat Sketch:Distortion=115" --pot "Rat Sketch:Filter=35" --pot "Rat Sketch:Volume=90" \
			      --effect amp --pot "Frenchie Amp:Gain=110" --pot "Frenchie Amp:Level=55" \
			      --effect cabinet --pot "Cabinet:Cabinet=4" --pot "Cabinet:Drive=85" \
			      < "$tmp/riff_dry.wav" > "$tmp/riff.wav" 2>/dev/null
		elif [ "$genre" = "techno" ]; then
			"$BN" --bench "$BENCH" \
			      --effect wah --pot "Auto-Wah:Sens=110" --pot "Auto-Wah:Resonance=116" --pot "Auto-Wah:Range=115" \
			      --effect flanger --pot "Flanger:Freq=85" --pot "Flanger:Depth=95" --pot "Flanger:Feedback=90" \
			      --effect delay --pot "Tape Echo:Time=55" --pot "Tape Echo:Sustain=80" --pot "Tape Echo:Blend=70" \
			      < "$tmp/riff_dry.wav" > "$tmp/riff.wav" 2>/dev/null
		else
			"$BN" --bench "$BENCH" \
			      --effect wah --pot "Auto-Wah:Sens=95" --pot "Auto-Wah:Resonance=105" --pot "Auto-Wah:Range=105" \
			      --effect phaser --pot "Phaser:LFO=75" --pot "Phaser:Feedback=85" \
			      --effect overdrive --pot "Klonlike:Gain=70" \
			      < "$tmp/riff_dry.wav" > "$tmp/riff.wav" 2>/dev/null
		fi

		"$BN" --bench "$BENCH" \
		      --effect fuzz --pot "Rat Sketch:Distortion=108" --pot "Rat Sketch:Filter=40" --pot "Rat Sketch:Volume=85" \
		      --effect flanger --pot "Flanger:Depth=70" --pot "Flanger:Feedback=65" \
		      --effect delay --pot "Tape Echo:Blend=65" --pot "Tape Echo:Sustain=78" --pot "Tape Echo:Time=80" \
		      --effect reverb --pot "Reverb:Room=95" --mix Reverb=55 \
		      < "$tmp/lead_dry.wav" > "$tmp/lead.wav" 2>/dev/null

		"$BN" --bench "$BENCH" \
		      --effect pitch --pot "Pitch:Octave=15" --pot "Pitch:Feedback=70" \
		      --effect fuzz --pot "Rat Sketch:Distortion=118" --pot "Rat Sketch:Mode=1" \
		      --effect delay --pot "Tape Echo:Blend=80" --pot "Tape Echo:Sustain=95" \
		      --effect reverb --pot "Reverb:Room=118" --mix Reverb=90 \
		      < "$tmp/bomb_dry.wav" > "$tmp/bomb.wav" 2>/dev/null
	else
		cp "$tmp/bass_dry.wav"  "$tmp/bass.wav"
		cp "$tmp/snare_dry.wav" "$tmp/snare.wav"
		cp "$tmp/riff_dry.wav"  "$tmp/riff.wav"
		cp "$tmp/lead_dry.wav"  "$tmp/lead.wav"
		cp "$tmp/bomb_dry.wav"  "$tmp/bomb.wav"
	fi

	python3 - "$tmp" "$genre" "$bpm" "$bars" "$tmp/song.wav" <<'PYEOF'
import sys, os, wave, struct, math, array

tmp_dir, genre, bpm_s, bars_s, out_wav = sys.argv[1:6]
BPM = float(bpm_s)
BARS = int(bars_s)
SR = 44100

def load_wav(name):
    p = os.path.join(tmp_dir, name)
    with wave.open(p, 'rb') as w:
        ch = w.getnchannels()
        frames = w.readframes(w.getnframes())
        samples = array.array('h')
        samples.frombytes(frames)
        if ch == 2:
            samples = array.array('h', samples[0::2])
        return [s / 32768.0 for s in samples]

def pitch_shift(buf, semitones):
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

bass_src  = load_wav("bass.wav")
snare_src = load_wav("snare.wav")
riff_src  = load_wav("riff.wav")
lead_src  = load_wav("lead.wav")
bomb_src  = load_wav("bomb.wav")

bass_hit  = bass_src[:int(0.36 * SR)]
kick_hit  = pitch_shift(bass_src[:int(0.18 * SR)], -7)
snare_hit = snare_src[:int(0.20 * SR)]
hat_hit   = pitch_shift(snare_src[:int(0.07 * SR)], 15)
chug_hit  = riff_src[:int(0.22 * SR)]

BEAT = 60.0 / BPM
BAR  = BEAT * 4.0
STEP = BEAT / 4.0
TOTAL_SEC = BARS * BAR + 2.2
N = int(TOTAL_SEC * SR)

L = [0.0] * N
R = [0.0] * N

def place(buf, start_sec, gain=1.0, pan=0.0):
    angle = (pan + 1.0) * (math.pi / 4.0)
    gl = gain * math.cos(angle)
    gr = gain * math.sin(angle)
    idx0 = int(start_sec * SR)
    for i, s in enumerate(buf):
        idx = idx0 + i
        if 0 <= idx < N:
            L[idx] += s * gl
            R[idx] += s * gr

cache = {}
def shifted(key, buf, semi):
    k = (key, semi)
    if k not in cache:
        cache[k] = pitch_shift(buf, semi)
    return cache[k]

for bar in range(max(1, BARS - 1)):
    t_bar = bar * BAR
    if genre == "funk":
        pat = [(0,0,1.0),(2,0,0.75),(3,3,0.85),(4,5,0.95),(6,7,0.85),(8,0,1.0),(10,10,0.85),(11,7,0.8),(12,5,0.9),(14,3,0.85),(15,-2,0.8)] if bar % 2 == 0 else \
              [(0,0,1.0),(2,12,0.9),(4,10,0.85),(6,7,0.9),(8,5,1.0),(10,3,0.85),(12,0,0.95),(14,-5,0.95),(15,-2,0.85)]
        for step, semi, g in pat:
            place(shifted("b", bass_hit, semi), t_bar + step * STEP, gain=0.95 * g, pan=0.0)
        if bar >= 1:
            place(snare_hit, t_bar + 4 * STEP, gain=0.85, pan=-0.15)
            place(snare_hit, t_bar + 12 * STEP, gain=0.90, pan=0.15)
            for s in range(16):
                if s % 2 == 0 or s in (7, 15):
                    place(hat_hit, t_bar + s * STEP, gain=0.32 if s % 4 == 0 else 0.20, pan=0.35 if s % 2 == 0 else -0.35)
        if bar in (2, 3, 6):
            place(shifted("r", riff_src, 0 if bar % 2 == 0 else 5), t_bar, gain=0.85, pan=-0.45 if bar % 2 == 0 else 0.45)
        if bar in (4, 5, 6):
            place(shifted("l", lead_src, 0 if bar % 2 == 0 else 7), t_bar + (0 if bar < 6 else 1.5 * BEAT), gain=0.88, pan=0.25)

    elif genre == "metal":
        chug_notes = [0, 0, 0, 1, 0, 0, 3, 2, 0, 0, 0, 1, 6, 5, 3, 1]
        for s in range(16):
            place(kick_hit, t_bar + s * STEP, gain=0.85 if s % 2 == 0 else 0.68, pan=0.0)
            semi = chug_notes[s]
            place(shifted("c", chug_hit, semi), t_bar + s * STEP, gain=0.65, pan=-0.75)
            place(shifted("c", chug_hit, semi), t_bar + s * STEP + 0.006, gain=0.65, pan=0.75)
            place(shifted("b", bass_hit, semi - 5), t_bar + s * STEP, gain=0.55, pan=0.0)
            if s % 2 == 0:
                place(hat_hit, t_bar + s * STEP, gain=0.30, pan=0.4)
        snare_steps = [4, 12] if bar % 2 == 0 else [2, 6, 10, 14]
        for s in snare_steps:
            place(snare_hit, t_bar + s * STEP, gain=0.95, pan=0.0)
        if bar >= 3:
            for q, semi in enumerate([12, 15, 18, 19] if bar % 2 == 1 else [19, 18, 15, 12]):
                place(shifted("l", lead_src[:int(0.45 * SR)], semi), t_bar + q * BEAT, gain=0.85, pan=0.25 if q % 2 == 0 else -0.25)

    elif genre == "doom":
        chord_roots = [0, -2, -5, -6]
        root = chord_roots[bar % len(chord_roots)]
        place(shifted("m", bomb_src, root), t_bar, gain=0.95, pan=0.0)
        place(shifted("r", riff_src, root - 5), t_bar, gain=0.80, pan=-0.65)
        place(shifted("r", riff_src, root + 7), t_bar + 0.015, gain=0.80, pan=0.65)
        place(kick_hit, t_bar + 0 * STEP, gain=1.0, pan=0.0)
        place(kick_hit, t_bar + 10 * STEP, gain=0.9, pan=0.0)
        place(shifted("s", snare_hit, -4), t_bar + 8 * STEP, gain=1.0, pan=0.0)
        for s in range(0, 16, 4):
            place(shifted("h", hat_hit, -6), t_bar + s * STEP, gain=0.35, pan=0.3)
        if bar >= 2:
            place(shifted("l", lead_src, root + 3), t_bar + BEAT, gain=0.75, pan=0.2 * (1 if bar % 2 == 0 else -1))

    elif genre == "techno":
        acid_seq = [0, 0, 12, 0, 3, 7, 0, 10, 0, 12, 15, 3, 7, 10, 0, -2]
        for s in range(16):
            if s % 4 == 0:
                place(kick_hit, t_bar + s * STEP, gain=1.05, pan=0.0)
            if s % 4 == 2:
                place(hat_hit, t_bar + s * STEP, gain=0.48, pan=0.25)
            elif s % 2 == 1:
                place(hat_hit, t_bar + s * STEP, gain=0.20, pan=-0.25)
            if bar >= 1 and s in (4, 12):
                place(snare_hit, t_bar + s * STEP, gain=0.80, pan=0.0)
            semi = acid_seq[(s + bar * 3) % 16]
            note_slice = riff_src[:int(0.16 * SR)]
            place(shifted("a", note_slice, semi), t_bar + s * STEP, gain=0.70, pan=0.45 * math.sin(s * 0.6))
        if bar >= 4:
            place(shifted("l", lead_src, 7 if bar % 2 == 0 else 12), t_bar, gain=0.72, pan=-0.3)

t_fin = max(1, BARS - 1) * BAR
place(bomb_src, t_fin, gain=1.1, pan=0.0)
place(shifted("m", bomb_src, -12), t_fin, gain=0.95, pan=0.0)
place(shifted("l", lead_src, 0), t_fin, gain=0.70, pan=-0.6)
place(shifted("l", lead_src, 7), t_fin, gain=0.70, pan=0.6)

peak = max(max(abs(x) for x in L), max(abs(x) for x in R), 1e-6)
out_pcm = bytearray()
for i in range(N):
    l = math.tanh((L[i] / peak) * 1.45) / math.tanh(1.45) * 0.94
    r = math.tanh((R[i] / peak) * 1.45) / math.tanh(1.45) * 0.94
    out_pcm += struct.pack('<hh', int(l * 32767), int(r * 32767))

with wave.open(out_wav, 'wb') as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(SR)
    w.writeframes(bytes(out_pcm))
PYEOF

	encode_output "$tmp/song.wav" "$out" "brownnote $genre ($bpm BPM)"
	rm -rf "$tmp"
	echo "✓ Rendered '$genre' song ($bpm BPM, $bars bars) -> $out"
	if [ "$do_play" = 1 ]; then
		play_file "$out"
	fi
}

run_pack() {
	local out_dir="$ROOT/build/presets" do_play=0
	while [ $# -gt 0 ]; do
		case "$1" in
			--out-dir) out_dir="$2"; shift 2 ;;
			--play)    do_play=1; shift ;;
			-h|--help) usage; exit 0 ;;
			*) echo "presets.sh pack: unknown option '$1'" >&2; exit 2 ;;
		esac
	done
	mkdir -p "$out_dir"
	local ext="mp3"
	command -v ffmpeg >/dev/null 2>&1 || ext="wav"

	local -a play_flag=()
	[ "$do_play" = 1 ] && play_flag=(--play)

	for p in doom cathedral machinegun swamp klon-crunch cyber-laser; do
		run_fx "$p" --out "$out_dir/fx_${p}.${ext}" "${play_flag[@]}"
	done
	for g in funk metal doom techno; do
		run_song "$g" --bars 6 --out "$out_dir/song_${g}.${ext}" "${play_flag[@]}"
	done
	echo "✓ Full preset pack written to $out_dir"
}

cmd="${1:-list}"
shift || true
case "$cmd" in
	list|-h|--help|help)
		usage
		;;
	fx)
		[ $# -ge 1 ] || { echo "presets.sh fx: missing <preset>" >&2; usage; exit 2; }
		run_fx "$@"
		;;
	stems)
		run_stems "$@"
		;;
	song)
		[ $# -ge 1 ] || { echo "presets.sh song: missing <genre>" >&2; usage; exit 2; }
		run_song "$@"
		;;
	pack)
		run_pack "$@"
		;;
	*)
		echo "presets.sh: unknown command '$cmd'" >&2
		usage
		exit 2
		;;
esac
