"""Decode PCM independently of Rust/Hound and check observable mechanics.

These checks characterize the pinned backend, including its D14 failure.
They are not an application acceptance suite or a listening test.
"""

import argparse
import array
import csv
import hashlib
import json
import math
from pathlib import Path
import sys
import wave

ROOT = Path(__file__).resolve().parent
RATE = 48000


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def decode(path):
    with wave.open(str(path), "rb") as wav:
        require((wav.getnchannels(), wav.getsampwidth(), wav.getframerate(),
                 wav.getnframes()) == (2, 2, RATE, 72000), f"WAV format: {path.name}")
        pcm = array.array("h", wav.readframes(wav.getnframes()))
    if sys.byteorder != "little":
        pcm.byteswap()
    require(max(abs(x) for x in pcm) < 32767, f"clipped PCM: {path.name}")
    return pcm[::2], pcm[1::2]


def rms(samples, start, end):
    window = samples[start:end]
    return math.sqrt(sum((x / 32768) ** 2 for x in window) / len(window))


def spectrum(samples, start, end):
    # A Hann-windowed projection onto the 16 known fundamentals. The fixture's
    # first non-fundamental harmonic is above the highest tested fundamental.
    segment = samples[start:end]
    window = [0.5 - 0.5 * math.cos(2 * math.pi * i / (len(segment) - 1))
              for i in range(len(segment))]
    amplitudes = {}
    for key in range(60, 76):
        omega = 2 * math.pi * 440 * 2 ** ((key - 69) / 12) / RATE
        real = sum(x * w * math.cos(omega * i) for i, (x, w) in enumerate(zip(segment, window)))
        imag = sum(x * w * math.sin(omega * i) for i, (x, w) in enumerate(zip(segment, window)))
        amplitudes[str(key)] = 2 * math.hypot(real, imag) / sum(window) / 32768
    return amplitudes


def verify(out):
    expected = list(csv.DictReader((ROOT / "fixtures/events.tsv").open(), delimiter="\t"))
    actual = list(csv.DictReader((out / "trace.tsv").open(), delimiter="\t"))
    require(actual == sorted(expected, key=lambda row: row["scenario"]), "event trace differs")
    names = sorted({row["scenario"] for row in expected})
    require(sorted(p.stem for p in out.glob("*.wav")) == names, "missing/extra WAV")
    waves = {name: decode(out / f"{name}.wav") for name in names}
    report = {"backend": "rustysynth 1.3.8", "sample_rate": RATE,
              "block_size": 64, "maximum_polyphony": 8, "effects": False,
              "d14_fifo_supported": False, "native_integration_tested": False,
              "cases": {}, "checks": []}

    def check(condition, message):
        require(condition, message)
        report["checks"].append(message)

    for label, (left, right) in waves.items():
        require(len(left) == len(right) == 72000, "truncated audio")
        first = next((i for i, (l, r) in enumerate(zip(left, right)) if l or r), None)
        report["cases"][label] = {
            "sha256": hashlib.sha256((out / f"{label}.wav").read_bytes()).hexdigest(),
            "first_nonzero_frame": first,
            "rms_36000_40800_left": rms(left, 36000, 40800),
            "tail_peak_65000_72000": max(abs(x) for ch in (left, right) for x in ch[65000:]),
        }
        start = min(int(e["frame"]) for e in expected
                    if e["scenario"] == label and e["op"] == "on")
        check(first is not None and start <= first < start + 128,
              f"{label}: onset within 128 frames of first note")
        check(all(x == 0 for ch in (left, right) for x in ch[:start]),
              f"{label}: exact pre-note silence")
        check(report["cases"][label]["tail_peak_65000_72000"] == 0,
              f"{label}: exact final silence")

    soft = rms(waves["soft"][0], 12000, 18000)
    loud = rms(waves["loud"][0], 12000, 18000)
    report["velocity_rms"] = {"32": soft, "112": loud, "ratio": loud / soft}
    check(soft > 2 / 32768 and loud > soft * 2, "velocity changes decoded level")
    check(rms(waves["zero_velocity"][0], 36000, 40800) == 0,
          "note-on velocity zero releases")
    held = rms(waves["sustain"][0], 36000, 40800)
    not_held = rms(waves["sustain_below_threshold"][0], 36000, 40800)
    report["sustain_rms"] = {"cc64_127": held, "cc64_63": not_held}
    check(held > 2 / 32768 and not_held == 0, "CC64 threshold and pedal-up release")

    overlap_early = spectrum(waves["overlap"][0], 16000, 23000)
    overlap_late = spectrum(waves["overlap"][0], 38000, 42000)
    check(overlap_early["60"] > 0.001 and overlap_early["67"] > 0.001
          and overlap_late["67"] > 0.001
          and overlap_late["60"] < overlap_early["60"] * 0.01,
          "different-pitch overlaps release independently")
    repeat = rms(waves["repeat"][0], 36000, 40800)
    held_repeat = rms(waves["repeat_held"][0], 36000, 40800)
    other_channel = rms(waves["separate_channels"][0], 36000, 40800)
    report["same_pitch_rms"] = {"after_first_off": repeat,
                               "without_first_off": held_repeat,
                               "second_on_separate_channel": other_channel}
    check(repeat == 0 and held_repeat > 2 / 32768,
          "D14 BLOCKER reproduced: first same-pitch off releases both notes")
    check(other_channel > 2 / 32768, "same pitch on another channel survives first off")
    for label, delay in [("panic", 64), ("reset_tail", 0)]:
        left, right = waves[label]
        check(all(x == 0 for ch in (left, right) for x in ch[19201 + delay:]),
              f"{label}: exact silence within {delay} frames")
        last = max(i for i, (l, r) in enumerate(zip(left, right)) if l or r)
        report["cases"][label]["last_nonzero_frame"] = last
    check(waves["fresh"] == waves["reopen"],
          "saved config and fresh construction reproduce exact PCM")
    check(all(fresh[9600:] == reset[9600:]
              for fresh, reset in zip(waves["fresh"], waves["reset_recall"])),
          "reset/reapply reproduces exact PCM from reset frame onward")
    check(waves["missing_program"] == waves["fresh"],
          "missing preset silently falls back to default (integration hazard)")

    amplitudes = spectrum(waves["polyphony"][0], 24000, 44000)
    threshold = max(amplitudes.values()) * 0.1
    present = [int(key) for key, amplitude in amplitudes.items() if amplitude > threshold]
    report["polyphony"] = {"fundamental_amplitudes": amplitudes,
                           "threshold": threshold, "present_keys": present}
    check(len(present) == 8, "16 simultaneous single-region notes leave 8 fundamentals")
    check(all(x == 0 for ch in waves["polyphony"] for x in ch[48064:]),
          "CC120 clears bounded-polyphony output within one block")
    check((out / "recall.txt").read_bytes() == (ROOT / "fixtures/config.txt").read_bytes(),
          "persisted configuration bytes round-trip")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = verify(args.output)
    (args.output / "measurements.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"{len(result['checks'])} behavioral checks passed; D14 remains BLOCKED")
