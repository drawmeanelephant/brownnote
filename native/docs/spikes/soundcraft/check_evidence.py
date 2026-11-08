#!/usr/bin/env python3
"""Check the published research artifacts, including privacy and explicit failures."""
import json
from pathlib import Path


def main():
    root = Path(__file__).resolve().parent
    evidence = root / "evidence"
    pin = json.loads((root / "pin.json").read_text())
    release = json.loads((evidence / "release.json").read_text())
    assert release["pin"] == pin
    assert release["architecture"] == "arm64"
    assert release["binary_sha256"] == pin["binary_sha256"]
    commands = json.loads((evidence / "commands.json").read_text())
    ids = {c["id"] for c in commands}
    assert {"file.import_audio", "file.import_midi", "session.save_as", "session.open",
            "midi.notes", "midi.note_edit", "session.audio_health",
            "mix.sampler_load", "edit.move_clips", "edit.clear"} <= ids
    records = json.loads((evidence / "app.json").read_text())
    exits = [r["app_exit"] for r in records if "app_exit" in r]
    assert exits == [0, 0, 0, 0], exits
    assert not any("shutdown_error" in r for r in records)
    errors = [r for r in records if r.get("exit", 0)]
    assert len(errors) == 1 and errors[0]["method"] == "mix.sampler_load"
    assert errors[0]["params"]["params"] == {"choke_group": 1}
    opens = [r["reply"] for r in records if r.get("method") == "session.open"]
    assert len(opens) == 3
    assert opens[0]["missing"] == opens[1]["missing"] == []
    assert opens[2]["missing"]
    health = [r["reply"]["healthy"] for r in records
              if r.get("method") == "session.audio_health"]
    assert health == [True, True, False]
    assertions = [r["assertion"] for r in records if "assertion" in r]
    assert any("source-free relocation" in r for r in assertions)
    assert any("negative control" in r for r in assertions)
    for path in evidence.iterdir():
        text = path.read_text()
        assert str(Path.home()) not in text and str(root) not in text, path.name
        assert "/Users/" not in text and "/home/" not in text, path.name
    print(f"PASS: {len(commands)} command descriptions, four GUI exits, "
          "two portable opens, expected choke rejection and missing-media control")


if __name__ == "__main__":
    main()
