"""Independently check a probe run directory produced by check.py.

Float32 WAV output is decoded with this file's own RIFF parser, never the
probe's writer. Expected control values come from the LADSPA SDK's hint
rules worked out by hand below, not from the probe's resolver.
"""

from array import array
import json
import math
from pathlib import Path
import struct
import sys
import wave

THRESHOLD = 10 ** (-90 / 20)
# Existing native pedal renders append a fixed two-second tail
# (native/src/pedals/render.rs TAIL_SECONDS).
PEDAL_TAIL_SECONDS = 2.0

# LADSPA hint table applied by hand. Plate: four [0, 1] ports.
# DEFAULT_MIDDLE = 0.5, DEFAULT_HIGH = 0.25*0 + 0.75*1, DEFAULT_LOW = 0.75*0 + 0.25*1.
PLATE_DEFAULTS = {"bandwidth": 0.5, "tail": 0.75, "damping": 0.5, "blend": 0.25}
# Fixture "hints" at 44.1 kHz: SAMPLE_RATE max 0.5*fs; LOG middle sqrt(20*20000);
# INTEGER low round(0.75*1 + 0.25*8); TOGGLED default 1.
HINT_DEFAULTS = {"rate": 22050.0, "log": math.sqrt(20 * 20000), "int": 3.0, "toggle": 1.0}


def read_float_wav(path):
    data = path.read_bytes()
    assert data[:4] == b"RIFF" and data[8:12] == b"WAVE", path
    assert struct.unpack_from("<I", data, 4)[0] == len(data) - 8, f"{path}: RIFF size"
    pos, fmt, pcm, fact = 12, None, None, None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        size = struct.unpack_from("<I", data, pos + 4)[0]
        body = data[pos + 8:pos + 8 + size]
        if cid == b"fmt ":
            fmt = struct.unpack_from("<HHIIHH", body)
        elif cid == b"fact":
            fact = struct.unpack_from("<I", body)[0]
        elif cid == b"data":
            pcm = body
        pos += 8 + size + (size & 1)
    tag, channels, rate, byte_rate, align, bits = fmt
    assert (tag, bits) == (3, 32), f"{path}: not float32"
    assert align == 4 * channels and byte_rate == rate * align, f"{path}: header"
    assert sys.byteorder == "little"
    values = array("f")
    values.frombytes(pcm)
    frames = len(values) // channels
    assert fact == frames, f"{path}: fact {fact} != {frames}"
    return rate, [values[c::channels] for c in range(channels)]


def read_pcm16(path):
    with wave.open(str(path)) as w:
        assert w.getsampwidth() == 2 and w.getnchannels() == 1
        raw = array("h")
        raw.frombytes(w.readframes(w.getnframes()))
        return w.getframerate(), [v / 32768.0 for v in raw]


def rms(values):
    return math.sqrt(sum(v * v for v in values) / len(values)) if values else 0.0


def dbfs(value):
    return 20 * math.log10(value) if value > 0 else -math.inf


def correlation(a, b):
    ma, mb = sum(a) / len(a), sum(b) / len(b)
    num = sum((x - ma) * (y - mb) for x, y in zip(a, b))
    den = math.sqrt(sum((x - ma) ** 2 for x in a) * sum((y - mb) ** 2 for y in b))
    return num / den


def load(run, name):
    record = json.loads((run / name / "render.json").read_text())
    rate, channels = read_float_wav(run / name / "render.wav")
    assert rate == record["output"]["rate"], name
    assert len(channels) == record["output"]["channels"], name
    assert len(channels[0]) == record["output"]["frames"], name
    for channel in channels:
        assert all(math.isfinite(v) for v in channel), f"{name}: non-finite"
    return record, rate, channels


def controls(record):
    return {c["name"]: c["value"] for c in record["controls"]}


def check_plate(run, source, report):
    rate_in, dry = read_pcm16(source)
    record, rate, (left, right) = load(run, "plate_pad")
    n = len(dry)
    assert rate == rate_in == 44100 and record["input"]["frames"] == n
    assert record["plugin"]["unique_id"] == 1779 and record["plugin"]["label"] == "Plate"
    assert record["channel_map"] == "identity"
    assert controls(record) == PLATE_DEFAULTS, controls(record)
    assert all(c["source"] == "default" for c in record["controls"])
    tail_frames = len(left) - n
    assert tail_frames == record["tail"]["frames"]
    tail_seconds = tail_frames / rate
    peak = max(max(map(abs, left)), max(map(abs, right)))
    assert 0.05 < peak < 1.0, peak
    # The probe trims at the last frame at or above -90 dBFS.
    assert max(abs(left[-1]), abs(right[-1])) >= THRESHOLD
    early = rms(left[n:n + rate // 2] + right[n:n + rate // 2])
    assert dbfs(early) > -60, f"no wet tail: {dbfs(early)} dBFS"
    tail_corr = correlation(left[n:n + rate // 2], right[n:n + rate // 2])
    assert tail_corr < 0.9, f"tail not decorrelated: {tail_corr}"
    assert PEDAL_TAIL_SECONDS < tail_seconds < 30, tail_seconds
    wet_distance = rms([l - d for l, d in zip(left, dry)])
    assert wet_distance > 0.005, wet_distance

    for name in ("plate_pad_block64", "plate_pad_repeat", "plate_pad_ladspa_path"):
        same = (run / name / "render.wav").read_bytes() == (run / "plate_pad" / "render.wav").read_bytes()
        assert same, f"{name} differs from plate_pad"

    record_dry, _, (dl, dr) = load(run, "plate_pad_dry")
    assert controls(record_dry)["blend"] == 0.0 and record_dry["tail"]["frames"] == 0
    assert list(dl) == list(dr) == [float(array("f", [v])[0]) for v in dry], "blend=0 is not the dry input"

    record_wet, _, (wl, _) = load(run, "plate_pad_wet")
    assert controls(record_wet)["blend"] == 1.0
    assert rms([w - d for w, d in zip(wl, dry)]) > wet_distance

    tails = {}
    for rate_name in ("44100", "48000"):
        rec, r, chans = load(run, f"plate_burst_{rate_name}")
        assert r == int(rate_name) and len(chans) == 2
        tails[rate_name] = rec["tail"]["seconds"]
    ratio = tails["48000"] / tails["44100"]
    assert 0.8 < ratio < 1.25, ratio

    report["plate"] = {
        "input": str(source.name), "input_frames": n, "rate": rate,
        "resolved_defaults": controls(record), "peak": peak,
        "measured_tail_seconds": tail_seconds,
        "exceeds_existing_pedal_tail_seconds": PEDAL_TAIL_SECONDS,
        "early_tail_rms_dbfs": round(dbfs(early), 2),
        "tail_left_right_correlation": round(tail_corr, 4),
        "block_64_vs_256": "byte-identical",
        "fresh_process_repeat": "byte-identical",
        "ladspa_path_discovery": "byte-identical",
        "blend_0": "exactly the dry input on both channels, no tail",
        "burst_tail_seconds": tails, "burst_tail_ratio_48k_over_44k": round(ratio, 4),
    }


def check_hints(run, source, report):
    _, dry = read_pcm16(source)
    record, _, (out,) = load(run, "hints")
    assert record["plugin"]["properties"] == ["INPLACE_BROKEN"]
    assert record["plugin"]["run_adding"] is True
    resolved = controls(record)
    for name, expected in HINT_DEFAULTS.items():
        assert math.isclose(resolved[name], expected, rel_tol=1e-12), (name, resolved[name])
        echoed = record["control_outputs"][f"{name}_out"]
        assert math.isclose(echoed, expected, rel_tol=1e-6), (name, echoed)
    expected_audio = [float(array("f", [v])[0]) for v in dry]
    assert list(out) == expected_audio, "fixture passthrough changed audio"

    adding, _, (out2,) = load(run, "hints_run_adding")
    assert adding["process_call"] == "run_adding"
    assert list(out2) == expected_audio
    assert adding["control_outputs"]["int_out"] == 5.0
    assert adding["control_outputs"]["toggle_out"] == 0.0
    report["hints_fixture"] = {
        "resolved": resolved, "echoed": record["control_outputs"],
        "run_adding": "matches run output; overrides echoed",
    }


def check_lifecycle(run, report):
    lines = (run / "lifecycle.log").read_text().split()
    events = list(zip(lines[0::2], lines[1::2]))
    labels = sorted({label for label, _ in events})
    for label in labels:
        calls = [what for name, what in events if name == label]
        assert calls.count("instantiate") == calls.count("cleanup") > 0, (label, calls)
        assert calls.count("activate") == calls.count("deactivate"), (label, calls)
    assert {"nan", "endless", "hints"} <= set(labels)
    report["lifecycle"] = {label: [w for n, w in events if n == label][:4] for label in labels}


def main():
    run, source = Path(sys.argv[1]), Path(sys.argv[2])
    report = {}
    check_plate(run, source, report)
    check_hints(run, source, report)
    check_lifecycle(run, report)
    (run / "verified.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    print("PASS: independent decode and hint checks")


if __name__ == "__main__":
    main()
