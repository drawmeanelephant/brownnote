//! The real binary, drawn by Rune's GPU renderer, so every test here is
//! `#[ignore]`d: `make native-test` runs them with `--include-ignored` and a
//! bare `cargo test` skips them, which is what CI does.
#[cfg(test)]
mod tests {
    use serde_json::Value;
    use std::{path::PathBuf, process::Command};

    fn run(name: &str, extra: &[&str]) -> Value {
        let tree = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("headless-{}-{name}.json", std::process::id()));
        std::fs::create_dir_all(tree.parent().unwrap()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_brownnote-native"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--tree", tree.to_str().unwrap()])
            .args(extra)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "headless {name} failed (GPU required):\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json = serde_json::from_slice(&std::fs::read(&tree).unwrap()).unwrap();
        std::fs::remove_file(tree).unwrap();
        json
    }

    fn text<'a>(tree: &'a Value, key: &str) -> &'a str {
        tree["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["key"] == key)
            .unwrap_or_else(|| panic!("missing {key}"))["text"]
            .as_str()
            .unwrap_or("")
    }

    #[test]
    #[ignore = "needs a GPU adapter"]
    fn actual_binary_script_voices_panic_and_missing_assets() {
        let playing = run(
            "playing",
            &["--script", "scripts/smoke.json", "--at", "0.6"],
        );
        assert_eq!(text(&playing, "pad-0-voices"), "▶ 1");
        assert_eq!(text(&playing, "pad-4-voices"), "▶ 1");
        assert_eq!(text(&playing, "audio-status"), "Headless: no audio device");
        assert_eq!(playing["warnings"].as_array().unwrap().len(), 0);
        assert!(text(&playing, "meter").ends_with(" dB"));
        assert_ne!(text(&playing, "meter"), "−∞ dB");
        let stopped = run(
            "stopped",
            &["--script", "scripts/smoke.json", "--at", "1.0"],
        );
        assert_eq!(text(&stopped, "pad-0-voices"), "");
        assert_eq!(text(&stopped, "pad-4-voices"), "");
        let expired = run(
            "expired",
            &["--script", "scripts/smoke.json", "--at", "3.0"],
        );
        assert_eq!(text(&expired, "pad-0-voices"), "");
        let missing = run("missing", &["--samples", "/nonexistent"]);
        assert_eq!(
            text(&missing, "asset-errors"),
            "Sample folder not found: /nonexistent. Pass --samples DIR."
        );
        assert_eq!(text(&missing, "audio-status"), "Headless: no audio device");
    }

    #[test]
    #[ignore = "needs a GPU adapter"]
    fn the_pedals_scene_shows_a_ready_chain_and_an_error_keeps_its_message() {
        let pedals = run("pedals", &["--scene", "pedals"]);
        assert_eq!(text(&pedals, "pedal-status"), "READY 0.4 s");
        assert_eq!(text(&pedals, "pad-4-fx"), "FX 2");
        assert!(
            pedals["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["key"] == "pedal-0")
        );
        assert!(
            pedals["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["key"] == "pedal-1")
        );
        assert!(
            pedals["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["key"] == "pot-sustain")
        );

        let error = run("pedal-error", &["--scene", "pedal-error"]);
        assert_eq!(
            text(&error, "pedal-status"),
            "ERROR · Rat Sketch / Distortion takes 0..120, not 200"
        );
        assert_eq!(text(&error, "pad-4-fx"), "FX 2");

        let unavailable = run("pedals-unavailable", &["--scene", "pedals-unavailable"]);
        assert_eq!(
            text(&unavailable, "pedal-status"),
            "Pedals need the GuitarPedal bench: set BROWNNOTE_BENCH or run `make -C vendor/GuitarPedal/Validation bench`."
        );
        assert_eq!(unavailable["warnings"].as_array().unwrap().len(), 0);
    }

    #[test]
    #[ignore = "needs a GPU adapter"]
    fn compact_binary_layout_keeps_every_pad_above_the_editor() {
        for missing in [false, true] {
            let mut args = vec!["--size", "820x600"];
            if missing {
                args.extend(["--samples", "/nonexistent"]);
            }
            let tree = run(
                if missing {
                    "compact-missing"
                } else {
                    "compact"
                },
                &args,
            );
            let rect = |key: &str| -> [f64; 4] {
                let element = tree["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|e| e["key"] == key)
                    .unwrap();
                std::array::from_fn(|i| element["rect"][i].as_f64().unwrap())
            };
            let [_, editor_y, _, _] = rect("editor");
            for i in 0..16 {
                let [x, y, w, h] = rect(&format!("pad-{i}"));
                assert!(x >= 0.0 && x + w <= 820.0 && h >= 60.0);
                assert!(
                    y + h <= editor_y,
                    "pad {i} overlaps editor, missing={missing}"
                );
                if i < 12 {
                    assert!(y + h <= rect(&format!("pad-{}", i + 4))[1]);
                }
            }
            assert_eq!(tree["warnings"].as_array().unwrap().len(), 0);
        }
    }
}
