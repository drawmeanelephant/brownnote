"""Generate an original MIT mechanics fixture, not a reference instrument."""

import argparse
import hashlib
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent / "fixtures"


def chunk(tag, data):
    return tag + struct.pack("<I", len(data)) + data + b"\0" * (len(data) % 2)


def name(text):
    return text.encode("ascii").ljust(20, b"\0")


def soundfont():
    info = chunk(b"LIST", b"INFO"
                 + chunk(b"ifil", struct.pack("<HH", 2, 1))
                 + chunk(b"isng", b"EMU8000\0")
                 + chunk(b"INAM", b"Brownnote mechanics\0")
                 + chunk(b"ICOP", b"Original generated fixture; MIT\0"))
    # Two periods of an integer triangle, then SF2's 46 guard samples.
    samples = [((i % 64) if i % 64 < 32 else 64 - i % 64) * 512 - 8192
               for i in range(128)] + [0] * 46
    sdta = chunk(b"LIST", b"sdta" + chunk(b"smpl", struct.pack("<174h", *samples)))
    generators = [(34, -12000), (38, -2400), (54, 1), (58, 69), (53, 0)]
    pdta = chunk(b"LIST", b"pdta"
                 + chunk(b"phdr", name("Triangle") + struct.pack("<HHHIII", 0, 0, 0, 0, 0, 0)
                         + name("EOP") + struct.pack("<HHHIII", 0, 0, 1, 0, 0, 0))
                 + chunk(b"pbag", struct.pack("<HHHH", 0, 0, 1, 0))
                 + chunk(b"pmod", b"\0" * 10)
                 + chunk(b"pgen", struct.pack("<HHHH", 41, 0, 0, 0))
                 + chunk(b"inst", name("Triangle") + struct.pack("<H", 0)
                         + name("EOI") + struct.pack("<H", 1))
                 + chunk(b"ibag", struct.pack("<HHHH", 0, 0, len(generators), 0))
                 + chunk(b"imod", b"\0" * 10)
                 + chunk(b"igen", b"".join(struct.pack("<Hh", *g) for g in generators)
                         + b"\0" * 4)
                 + chunk(b"shdr", name("Triangle") + struct.pack("<IIIIIBbHH",
                         0, 128, 0, 64, 28160, 69, 0, 0, 1)
                         + name("EOS") + b"\0" * 26))
    return chunk(b"RIFF", b"sfbk" + info + sdta + pdta)


def events():
    rows = ["scenario\tframe\top\tchannel\tkey_or_cc\tvalue\tnote_id"]

    def case(label, entries, end=72000):
        for frame, op, channel, key, value, identity in entries:
            rows.append(f"{label}\t{frame}\t{op}\t{channel}\t{key}\t{value}\t{identity}")
        rows.append(f"{label}\t{end}\tend\t0\t0\t0\t-")

    def on(frame, key=69, velocity=100, identity="a", channel=0):
        return frame, "on", channel, key, velocity, identity

    def off(frame, key=69, identity="a", channel=0):
        return frame, "off", channel, key, 0, identity

    def cc(frame, key, value):
        return frame, "cc", 0, key, value, "-"

    case("overlap", [on(4800, 60), on(14400, 67, identity="b"),
                     off(24000, 60), off(43200, 67, "b")])
    case("repeat", [on(4800), on(9600, identity="b"), off(19200),
                    off(43200, identity="b")])
    case("repeat_held", [on(4800), on(9600, identity="b"),
                         off(43200), off(43200, identity="b")])
    case("separate_channels", [on(4800), on(9600, identity="b", channel=1),
                               off(19200), off(43200, identity="b", channel=1)])
    for label, velocity in [("soft", 32), ("loud", 112)]:
        case(label, [on(4800, velocity=velocity), off(24000)])
    case("zero_velocity", [on(4800), on(19200, velocity=0)])
    case("sustain", [on(4800), cc(9600, 64, 127), off(14400), cc(43200, 64, 0)])
    case("sustain_below_threshold", [on(4800), cc(9600, 64, 63), off(14400)])
    case("panic", [on(4800), cc(9600, 64, 127), off(14400),
                   (19201, "panic", 0, 0, 0, "-")])
    case("reset_tail", [on(4800), (19201, "reset", 0, 0, 0, "-")])
    case("polyphony", [on(4800, key, identity=f"p{key}") for key in range(60, 76)]
         + [cc(48000, 120, 0)])
    case("fresh", [on(14400), off(33600)])
    case("reset_recall", [on(4800, 60), cc(6400, 64, 127),
                          cc(6400, 7, 20), (9600, "reset", 0, 0, 0, "-"),
                          on(14400), off(33600)])
    case("reopen", [on(14400), off(33600)])
    case("missing_program", [(0, "program", 0, 127, 0, "-"),
                             on(14400), off(33600)])
    return ("\n".join(rows) + "\n").encode()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    files = {"mechanics.sf2": soundfont(), "events.tsv": events()}
    hashes = "".join(f"{hashlib.sha256(data).hexdigest()}  {filename}\n"
                     for filename, data in files.items()).encode()
    files["SHA256SUMS"] = hashes
    ROOT.mkdir(exist_ok=True)
    for filename, data in files.items():
        path = ROOT / filename
        if args.check:
            if path.read_bytes() != data:
                raise SystemExit(f"fixture differs: {path}")
        else:
            path.write_bytes(data)
    print("fixture bytes and SHA256SUMS verified" if args.check else hashes.decode(), end="\n")


if __name__ == "__main__":
    main()
