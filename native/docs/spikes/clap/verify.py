"""Independent checks over one check.py run directory.

The oracle comes from the fixtures and the CLAP contract, not from the
probe: WAVs are decoded here, onsets and silences are measured here, and
spectral checks use a Goertzel filter at the fixture's note frequencies.
Characterizations (known plugin defects or limits) are recorded as passing
checks named `characterize:*` so a change in plugin behavior fails loudly.
"""

import json
import math
from pathlib import Path
import struct
import sys

RUN = Path(sys.argv[1])
SR = 48000
CHECKS = []
# Fixture name -> expected probe error kind; check.py writes one report each.
LOAD_FAILURES = {
    "missing-bundle": "bundle-missing",
    "garbage-binary": "dlopen-failed",
    "not-a-clap": "no-clap-entry",
    "unknown-id": "unknown-plugin-id",
    "init-fails": "init-failed",
    "activate-fails": "activate-failed",
}
MEAS = {}


def check(name, ok, **detail):
    CHECKS.append({"name": name, "pass": bool(ok), **detail})
    print(("PASS " if ok else "FAIL ") + name + (f" {detail}" if detail else ""), flush=True)


def wav(path):
    b = (RUN / path).read_bytes()
    assert b[:4] == b"RIFF" and b[8:16] == b"WAVEfmt "
    fmt, ch, sr = struct.unpack("<HHI", b[20:28])
    bits = struct.unpack("<H", b[34:36])[0]
    assert (fmt, bits, sr) == (3, 32, SR), (path, fmt, bits, sr)
    assert b[36:40] == b"data"
    n = struct.unpack("<I", b[40:44])[0] // 4
    data = struct.unpack(f"<{n}f", b[44:44 + 4 * n])
    return [data[c::ch] for c in range(ch)]


def trace(path):
    rows = []
    for line in (RUN / path).read_text().splitlines()[1:]:
        f, src, kind, nid, key, val = line.split("\t")
        rows.append({"frame": int(f), "source": src, "kind": kind, "note_id": int(nid),
                     "key": int(key), "value": float(val)})
    return rows


def summary(path):
    return json.loads((RUN / path).read_text())


def goertzel(x, hz):
    w = 2 * math.pi * hz / SR
    c = 2 * math.cos(w)
    s1 = s2 = 0.0
    for v in x:
        s1, s2 = v + c * s1 - s2, s1
    return math.sqrt(max(0.0, s1 * s1 + s2 * s2 - c * s1 * s2)) / len(x)


def rms(x):
    return math.sqrt(sum(v * v for v in x) / len(x))


def key_hz(key):
    return 440.0 * 2 ** ((key - 69) / 12)


def first_nonzero(x):
    return next((i for i, v in enumerate(x) if v != 0.0), None)


def note_ends(rows):
    return {r["note_id"]: r for r in rows if r["source"] == "out" and r["kind"] == "note-end"}


def synth():
    life = wav("synth/life-256.wav")
    L = life[0]
    check("synth: silence before the first note-on frame", all(v == 0.0 for v in L[:1000]))
    check("synth: first note sounds at its exact fixture frame", first_nonzero(L) == 1000,
          first_nonzero=first_nonzero(L))
    check("synth: stereo channels identical (plugin is mono-summed)", life[0] == life[1])

    without = wav("synth/without-2.wav")[0]
    diff = next((i for i, (a, b) in enumerate(zip(L, without)) if a != b), None)
    check("synth: note id 2 changes the output first at its fixture frame 5003", diff == 5003,
          first_difference=diff)

    raw = {n: (RUN / f"synth/{n}.wav").read_bytes() for n in ("life-256", "life-37", "life-128")}
    check("synth: block sizes 256, 128 and 37 render identical PCM",
          raw["life-256"] == raw["life-37"] == raw["life-128"])
    live = (RUN / "synth/life-live-128.wav").read_bytes()
    check("synth: paced realtime-mode thread renders the same PCM as offline", live == raw["life-128"])
    MEAS["synth_live_128"] = {k: summary("synth/life-live-128.json")[k] for k in (
        "block_budget_ns", "process_ns_p50", "process_ns_p99", "process_ns_max",
        "blocks_over_budget", "late_blocks", "max_lateness_ns")}

    rows = trace("synth/life-256.trace.tsv")
    ends = note_ends(rows)
    offs = {r["note_id"]: r["frame"] for r in rows if r["source"] == "in" and r["kind"] == "note-off"}
    keys = {r["note_id"]: r["key"] for r in rows if r["source"] == "in" and r["kind"] == "note-on"}
    check("synth: exactly one note-end per note id, with the note's key",
          sorted(ends) == [1, 2, 3] and all(ends[i]["key"] == keys[i] for i in ends)
          and sum(1 for r in rows if r["kind"] == "note-end") == 3)
    release = {i: ends[i]["frame"] - offs[i] for i in ends}
    MEAS["synth_note_end_minus_note_off_frames"] = release
    check("synth: 100 ms release ends 4800..4928 frames after note-off",
          all(4800 <= d <= 4928 for d in release.values()), release=release)
    last_end = max(e["frame"] for e in ends.values())
    nz = max(i for i, v in enumerate(L) if v != 0.0)
    check("synth: exact silence from the last note-end onward", nz < last_end,
          last_nonzero=nz, last_note_end=last_end)
    for name in ("life-37", "life-128"):
        e = note_ends(trace(f"synth/{name}.trace.tsv"))
        MEAS[f"synth_note_end_frames_{name}"] = {i: e[i]["frame"] for i in e}

    wild = wav("synth/wildcard-off.wav")[0]
    w_ends = note_ends(trace("synth/wildcard-off.trace.tsv"))
    window = slice(19000, 19960)
    a3_id, a3_wild = goertzel(L[window], key_hz(57)), goertzel(wild[window], key_hz(57))
    e4_id, e4_wild = goertzel(L[window], key_hz(64)), goertzel(wild[window], key_hz(64))
    MEAS["synth_d14_window_19000_19960"] = {"a3_by_id": a3_id, "a3_wildcard": a3_wild,
                                            "e4_by_id": e4_id, "e4_wildcard": e4_wild}
    check("synth: note-off by note id releases only that voice (same-key id 3 keeps sounding)",
          ends[3]["frame"] > 26000 and a3_id > 5 * a3_wild and e4_wild > 0.01,
          a3_by_id=round(a3_id, 5), a3_wildcard=round(a3_wild, 5))
    check("synth: wildcard note-off (id -1) releases both same-key voices together",
          w_ends[1]["frame"] == w_ends[3]["frame"] < 26000)

    vel = wav("synth/velocity.wav")[0]
    ratio = rms(vel[2000:9000]) / rms(vel[21000:28000])
    MEAS["synth_velocity_rms_ratio_1_0_over_0_25"] = ratio
    check("synth: velocity 1.0 vs 0.25 scales RMS by sqrt ratio 2.0 (+/-5%)", 1.9 <= ratio <= 2.1,
          ratio=round(ratio, 4))

    long_rows = trace("synth/release-long.trace.tsv")
    d = note_ends(long_rows)[1]["frame"] - 10000
    MEAS["synth_release_long_frames"] = d
    check("synth: 500 ms release ends 24000..24128 frames after note-off", 24000 <= d <= 24128,
          frames=d)

    stuck = wav("synth/same-key-during-release.wav")[0]
    s_ends = note_ends(trace("synth/same-key-during-release.trace.tsv"))
    check("characterize: same-key note-off during another voice's release leaves id 3 stuck",
          3 not in s_ends and rms(stuck[-4800:]) > 0.05, note_ends=sorted(s_ends),
          final_rms=round(rms(stuck[-4800:]), 4))

    zero = wav("synth/zero-attack.wav")[0]
    z_end = note_ends(trace("synth/zero-attack.trace.tsv"))[1]["frame"]
    z_last = max(i for i, v in enumerate(zero) if v != 0.0)
    check("characterize: with 0 ms attack, note-off cuts the voice within 64 frames (no release)",
          first_nonzero(zero) == 1000 and 9999 <= z_last < 10064 and z_end == 10064,
          last_nonzero=z_last, note_end=z_end)


def synth_state():
    s = summary("synth-state/state.json")
    MEAS["synth_state"] = s
    check("synth state: restored blob equals the saved blob; mutated blob differs",
          s["s2_equals_s0"] and not s["s1_equals_s0"])
    check("synth state: parameter value restored", s["value_after_restore"] == s["value_before"]
          and s["value_after_mutation"] != s["value_before"])
    ref, mut, res = ((RUN / f"synth-state/state-{n}.wav").read_bytes()
                     for n in ("reference", "mutated", "restored"))
    check("synth state: mutation changes the render; in-place restore reproduces it exactly",
          mut != ref and res == ref)
    fresh0, fresh1 = ((RUN / f"synth-state/fresh-{n}.wav").read_bytes() for n in ("s0", "s1"))
    check("synth state: a fresh process loading s0 / s1 reproduces reference / mutated exactly",
          fresh0 == ref and fresh1 == mut)
    r, m = wav("synth-state/state-reference.wav")[0], wav("synth-state/state-mutated.wav")[0]
    ratio = rms(m[2000:9000]) / rms(r[2000:9000])
    # Gain range is linear 10^(-36/20)..1; normalized 0.5 is its midpoint, default is -12 dB.
    expect = (10 ** (-36 / 20) + 1) / 2 / 10 ** (-12 / 20)
    check("synth state: Gain mutation scales RMS as its plain values predict (+/-3%)",
          abs(ratio / expect - 1) < 0.03, ratio=round(ratio, 4), expected=round(expect, 4))


def effect():
    desc = json.loads((RUN / "describe-effect.json").read_text())["plugins"][0]
    b512 = summary("effect/burst-512.json")
    MEAS["effect_contract"] = {
        "extensions": desc["extensions"],
        "render_has_hard_realtime_requirement": desc["render_has_hard_realtime_requirement"],
        "offline_mode_accepted": b512["render_mode_accepted"],
        "realtime_mode_accepted": summary("effect/burst-rt-512.json")["render_mode_accepted"],
        "latency_frames": b512["latency"],
        "reported_tail_frames": b512["tail_first"],
        "tail_changes": b512["tail_changes"],
        "process_status_counts": b512["process_status_counts"],
        "audio_ports": [desc["audio_inputs"], desc["audio_outputs"]],
    }
    check("effect: render extension present, no hard realtime requirement, both modes accepted",
          "render" in desc["extensions"] and desc["render_has_hard_realtime_requirement"] is False
          and b512["render_mode_accepted"] is True
          and summary("effect/burst-rt-512.json")["render_mode_accepted"] is True)
    check("effect: reports zero latency", b512["latency"] == 0)
    raw = {n: (RUN / f"effect/{n}.wav").read_bytes() for n in
           ("burst-512", "burst-rt-512", "burst-256", "burst-live-256")}
    check("effect: offline and realtime render modes produce identical PCM",
          raw["burst-512"] == raw["burst-rt-512"])
    check("effect: paced realtime thread at 256 equals offline at 256",
          raw["burst-live-256"] == raw["burst-256"])
    MEAS["effect_block_256_equals_512"] = raw["burst-256"] == raw["burst-512"]
    MEAS["effect_live_256"] = {k: summary("effect/burst-live-256.json")[k] for k in (
        "block_budget_ns", "process_ns_p50", "process_ns_p99", "process_ns_max",
        "blocks_over_budget", "late_blocks", "max_lateness_ns")}
    for n in raw:
        check(f"effect: {n} has no non-finite samples", summary(f"effect/{n}.json")["nonfinite_samples"] == 0)

    out = wav("effect/burst-512.wav")[0]
    check("effect: wet output continues after the 12000-frame input burst ends",
          rms(out[12000:24000]) > 1e-3, rms=round(rms(out[12000:24000]), 5))

    tail = wav("effect/tail-30s.wav")[0]
    def last_above(db):
        th = 10 ** (db / 20)
        return max(i for i, v in enumerate(tail) if abs(v) > th) - 12000
    measured = {db: last_above(db) for db in (-60, -80, -100)}
    floor = 20 * math.log10(max(abs(v) for v in tail[-SR:]))
    MEAS["effect_tail"] = {"reported_frames": b512["tail_first"],
                           "measured_frames_after_input_end": {str(k): v for k, v in measured.items()},
                           "last_second_peak_dbfs": round(floor, 1),
                           "status_counts_30s": summary("effect/tail-30s.json")["process_status_counts"]}
    check("characterize: reported tail (2 s) is shorter than the measured -60 dBFS decay",
          b512["tail_first"] < measured[-60], reported=b512["tail_first"], measured=measured[-60])
    check("characterize: output never reaches exact zero; floor stays below -120 dBFS",
          floor < -120 and any(v != 0.0 for v in tail[-SR:]), floor_dbfs=round(floor, 1))
    st = summary("effect/tail-30s.json")["process_status_counts"]
    check("characterize: process never returns TAIL or SLEEP; the host must detect silence",
          st["tail"] == 0 and st["sleep"] == 0 and st["continue"] > 0)

    byp = wav("effect/bypass.wav")
    src = [0.5 * math.sin(2 * math.pi * 440 * i / SR) if i < 12000 else 0.0 for i in range(SR)]
    err = max(abs(a - b) for a, b in zip(byp[0], src))
    MEAS["effect_bypass_max_abs_error_vs_input"] = err
    check("effect: Bypass=1 passes the known input through (max error < 1e-6)", err < 1e-6,
          max_error=err)


def effect_state():
    s = summary("effect-state/state.json")
    MEAS["effect_state"] = s
    check("effect state: restored blob equals the saved blob; mutated blob differs",
          s["s2_equals_s0"] and not s["s1_equals_s0"])
    check("effect state: parameter value restored", s["value_after_restore"] == s["value_before"])
    ref, mut, res = (wav(f"effect-state/state-{n}.wav")[0] for n in ("reference", "mutated", "restored"))
    check("effect state: mutation changes the render", ref != mut)
    fresh_default = wav("effect/burst-512.wav")[0]
    check("effect state: first in-instance render equals a fresh default render", ref == fresh_default)
    dev = max(abs(a - b) for a, b in zip(ref, res))
    f0, f0b = (wav(f"effect-state/fresh-{n}.wav")[0] for n in ("s0", "s0-again"))
    dev_fresh = max(abs(a - b) for a, b in zip(ref, f0))
    MEAS["effect_state_audio"] = {"in_place_restore_max_abs_diff": dev,
                                  "fresh_load_s0_vs_default_max_abs_diff": dev_fresh,
                                  "fresh_load_s0_repeatable": f0 == f0b}
    check("characterize: restoring the same state does not reproduce the reference audio",
          dev > 1e-3 and dev_fresh > 1e-3, in_place=round(dev, 5), fresh=round(dev_fresh, 5))
    check("effect state: loading s0 in two fresh processes renders identical audio", f0 == f0b)


def failures():
    """Failure boundary checks (load errors, crash, hang, non-finite)."""
    for name, kind in LOAD_FAILURES.items():
        r = json.loads((RUN / f"failures/load-{name}.json").read_text())
        check(f"load failure {name}: typed error {kind}, exit 3, no output, state file untouched",
              r["exit_code"] == 3 and r["error"] == kind and not r["output_created"]
              and r["state_unchanged"], got=r["error"], exit_code=r["exit_code"])

    sr = 48000
    src = [0.5 * math.sin(2 * math.pi * 440 * i / sr) for i in range(24000)]

    def matches_input(path):
        w = wav(path)
        return len(w[0]) == 24000 and all(
            abs(a - s) < 1e-7 and abs(b - s) < 1e-7 for a, b, s in zip(w[0], w[1], src))

    ip = lambda n: json.loads((RUN / f"failures/inproc-{n}.json").read_text())
    exits = json.loads((RUN / "failures/inproc-exit-codes.json").read_text())
    progress = lambda n: (RUN / f"failures/inproc-{n}.progress").read_text().splitlines()
    check("in-process passthrough completes all 94 blocks",
          exits["passthrough"] == 0 and ip("passthrough")["blocks_completed"] == 94)
    crash_lines = progress("crash")
    check("in-process crash: the whole host process dies with SIGSEGV after 8 blocks",
          exits["crash"] == -11 and len(crash_lines) == 8
          and not (RUN / "failures/inproc-crash.json").exists(),
          exit=exits["crash"], blocks=len(crash_lines))
    hang = ip("hang")
    check("in-process hang: watchdog detects it, but the audio thread and plugin are lost",
          exits["hang"] == 4 and hang["outcome"] == "hang-detected" and hang["blocks_completed"] == 8
          and hang["audio_thread_reclaimed"] is False and hang["plugin_destroyed"] is False,
          detect_ms=round(hang["detect_ms"], 1))
    nf = ip("nonfinite")
    check("in-process non-finite: completes and returns poisoned samples to the host",
          exits["nonfinite"] == 0 and nf["nonfinite_samples"] > 0, nonfinite=nf["nonfinite_samples"])

    iso = lambda n: json.loads((RUN / f"failures/helper-{n}.json").read_text())
    p = iso("passthrough")
    check("helper passthrough: no failures, output equals the input",
          not p["failures"] and p["disabled"] is None and matches_input("failures/helper-passthrough.wav"))
    c = iso("crash")
    check("helper crash: host survives three SIGSEGVs at blocks 8, 17, 26; restarts twice, "
          "restores state each time, then disables the plugin",
          [f["block"] for f in c["failures"]] == [8, 17, 26]
          and all(f["kind"] == "helper-exited" and f["signal"] == 11 for f in c["failures"])
          and c["restarts"] == 2 and c["states_restored_after_restart"] == 2
          and c["disabled"] is not None and c["host_survived"],
          restart_ms=[round(x, 1) for x in c["restart_ms"]])
    check("helper crash: full-length output; every lost block is replaced by dry input",
          matches_input("failures/helper-crash.wav") and c["dry_substituted_blocks"] == 94 - 24)
    h = iso("hang")
    check("helper hang: each hang is cut off at the 200 ms deadline, helper killed and restarted",
          [f["block"] for f in h["failures"]] == [8, 17, 26]
          and all(f["kind"] == "helper-timed-out" and 200 <= f["detect_ms"] < 400 for f in h["failures"])
          and h["restarts"] == 2 and matches_input("failures/helper-hang.wav"),
          detect_ms=[round(f["detect_ms"], 1) for f in h["failures"]])
    n = iso("nonfinite")
    check("helper non-finite: detected on block 8, plugin disabled, no NaN reaches the output",
          [f["kind"] for f in n["failures"]] == ["nonfinite-output"] and n["failures"][0]["block"] == 8
          and n["disabled"] == "nonfinite-output" and matches_input("failures/helper-nonfinite.wav"))
    i = iso("init-fails")
    check("helper init failure: plugin disabled from the start, dry render completes",
          i["disabled"] is not None and i["dry_substituted_blocks"] == 94
          and matches_input("failures/helper-init-fails.wav"))
    real = (RUN / "failures/helper-effect-256.wav").read_bytes() == (RUN / "effect/burst-256.wav").read_bytes()
    check("helper transport is lossless for the real effect: identical PCM to in-process", real)

    base = {b: json.loads((RUN / f"failures/inproc-passthrough-{b}.json").read_text()) for b in (64, 256)}
    rt = {b: json.loads((RUN / f"failures/helper-passthrough-{b}.json").read_text()) for b in (64, 256)}
    fx = iso("effect-256")
    MEAS["isolation_cost"] = {
        "passthrough_in_process_ns": {b: [base[b]["process_ns_p50"], base[b]["process_ns_p99"]] for b in base},
        "passthrough_helper_roundtrip_ns": {b: [rt[b]["roundtrip_ns_p50"], rt[b]["roundtrip_ns_p99"]] for b in rt},
        "effect_256_helper_roundtrip_ns": [fx["roundtrip_ns_p50"], fx["roundtrip_ns_p99"], fx["roundtrip_ns_max"]],
        "effect_256_in_process_ns": [summary("effect/burst-256.json")["process_ns_p50"],
                                     summary("effect/burst-256.json")["process_ns_p99"]],
        "helper_first_start_ms": {"fixture": p["first_start_ms"], "effect": fx["first_start_ms"]},
        "helper_restart_ms": c["restart_ms"] + h["restart_ms"],
        "block_budget_ns": {64: 1333333, 256: 5333333},
    }
    MEAS["failure_detection_ms"] = {"in_process_hang": hang["detect_ms"],
                                    "helper_crash": [f["detect_ms"] for f in c["failures"]],
                                    "helper_hang": [f["detect_ms"] for f in h["failures"]]}


def repeat():
    for p in sorted((RUN / "repeat").rglob("*.wav")):
        rel = p.relative_to(RUN / "repeat")
        check(f"repeat: fresh process reproduces {rel} byte for byte",
              p.read_bytes() == (RUN / rel).read_bytes())


def main():
    synth()
    synth_state()
    effect()
    effect_state()
    failures()
    repeat()
    failed = [c["name"] for c in CHECKS if not c["pass"]]
    (RUN / "measurements.json").write_text(json.dumps(
        {"checks": CHECKS, "passed": len(CHECKS) - len(failed), "failed": len(failed),
         "measurements": MEAS}, indent=2, default=str) + "\n")
    if failed:
        raise SystemExit(f"{len(failed)} check(s) failed: {failed}")
    print(f"PASS: {len(CHECKS)} checks")


if __name__ == "__main__":
    main()
