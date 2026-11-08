#!/usr/bin/env python3
"""Generate only the synthetic v1 pad WAV, not a project, take, or export."""

import argparse
import json
from pathlib import Path
import struct
import wave


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new WAV path (never overwritten)")
    args = parser.parse_args()
    score = json.loads(Path(__file__).with_name("score-v1.json").read_text())
    if score["fixture_version"] != 1:
        parser.error("unsupported fixture version")
    source = score["custom_pad"]["source"]
    pcm = bytearray()
    for frame in range(source["frames"]):
        envelope = min(frame, source["frames"] - 1 - frame, source["fade_frames"])
        magnitude = source["peak_pcm"] * envelope // source["fade_frames"]
        sign = 1 if frame % source["period_frames"] < source["period_frames"] // 2 else -1
        pcm.extend(struct.pack("<h", sign * magnitude))
    # Exclusive creation protects unrelated files, including on a repeated run.
    with args.output.open("xb") as output:
        with wave.open(output, "wb") as wav:
            wav.setnchannels(source["channels"])
            wav.setsampwidth(source["bits_per_sample"] // 8)
            wav.setframerate(source["sample_rate"])
            wav.writeframes(pcm)


if __name__ == "__main__":
    main()
