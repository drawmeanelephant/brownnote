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
    fn actual_binary_mounts_output_settings_and_recovery_scenes() {
        for scene in ["output-selection", "output-missing", "output-invalid"] {
            let tree = run(scene, &["--scene", scene, "--size", "820x600"]);
            assert!(
                tree["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|element| element["key"] == "output-panel")
            );
            assert!(text(&tree, "output-status").starts_with("SILENT"));
            if scene == "output-invalid" {
                assert!(!text(&tree, "output-error").is_empty());
            }
            assert_eq!(tree["warnings"].as_array().unwrap().len(), 0);
        }
    }

    #[test]
    #[ignore = "needs a GPU adapter"]
    fn actual_binary_mounts_import_controls_and_all_status_scenes_at_minimum_size() {
        for (scene, status) in [
            ("import-path", "Choose a path"),
            ("import-busy", "IMPORTING"),
            ("import-error", "IMPORT FAILED"),
            ("import-success", "IMPORTED"),
        ] {
            let tree = run(scene, &["--scene", scene, "--size", "820x600"]);
            let elements = tree["elements"].as_array().unwrap();
            assert!(elements.iter().any(|element| {
                element["key"] == "import-panel" && element["role"] == "dialog"
            }));
            assert!(text(&tree, "import-status").starts_with(status));
            for key in [
                "import-path",
                "import-submit",
                "import-cancel",
                "import-status",
            ] {
                let element = elements
                    .iter()
                    .find(|element| element["key"] == key)
                    .unwrap();
                let rect: [f64; 4] =
                    std::array::from_fn(|index| element["rect"][index].as_f64().unwrap());
                let [x, y, w, h] = rect;
                assert!(
                    x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0 && x + w <= 820.1 && y + h <= 600.1,
                    "{scene}: {key} is outside the minimum window: {rect:?}"
                );
            }
            assert!(
                tree["warnings"].as_array().unwrap().is_empty(),
                "{scene}: {}",
                tree["warnings"]
            );
        }
    }

    #[test]
    #[ignore = "needs a GPU adapter"]
    fn actual_binary_mounts_project_save_open_and_error_controls_at_minimum_size() {
        for scene in ["project-path", "project-error"] {
            let tree = run(scene, &["--scene", scene, "--size", "820x600"]);
            let elements = tree["elements"].as_array().unwrap();
            for key in [
                "projects-toggle",
                "projects-panel",
                "projects-path",
                "projects-save",
                "projects-open",
                "projects-recover",
                "projects-cancel",
            ] {
                let element = elements
                    .iter()
                    .find(|element| element["key"] == key)
                    .unwrap();
                let [x, y, w, h]: [f64; 4] =
                    std::array::from_fn(|index| element["rect"][index].as_f64().unwrap());
                assert!(
                    x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0 && x + w <= 820.1 && y + h <= 600.1,
                    "{scene}: {key} overflows"
                );
            }
            if scene == "project-error" {
                assert!(!text(&tree, "projects-error").is_empty());
            }
            assert!(
                tree["warnings"].as_array().unwrap().is_empty(),
                "{}",
                tree["warnings"]
            );
        }
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
            let [x, y, w, h] = rect("import-toggle");
            assert!(x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0);
            assert!(x + w <= 820.1 && y + h <= 600.1);
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

    #[test]
    #[ignore = "needs a GPU adapter"]
    fn actual_binary_records_a_keyboard_take_replays_it_and_panics() {
        let at = |name: &str, time: &str| {
            let tree = run(name, &["--script", "scripts/transport.json", "--at", time]);
            assert_eq!(tree["warnings"].as_array().unwrap().len(), 0, "{name}");
            text(&tree, "transport-status").to_string()
        };
        let recording = at("transport-recording", "1.0");
        assert!(recording.starts_with("REC T1 · 3 NOTES"), "{recording}");
        let replaying = at("transport-replaying", "3.1");
        assert!(replaying.starts_with("PLAYING"), "{replaying}");
        assert!(replaying.contains("1 TAKES · 3 NOTES"), "{replaying}");
        assert!(
            replaying.contains("REPLAY 3 SENT · 3 PLAYED"),
            "{replaying}"
        );
        assert!(
            replaying.contains("take 1 recorded: 3 note(s)"),
            "{replaying}"
        );
        let stopped = at("transport-panic", "3.6");
        assert!(stopped.starts_with("STOPPED"), "{stopped}");
        assert!(
            stopped.contains("3 PLAYED · SEL T1: 3 NOTES · Panic"),
            "{stopped}"
        );
    }

    #[test]
    #[ignore = "needs a GPU adapter"]
    fn actual_binary_records_virtual_midi_held_durations_at_minimum_size() {
        let tree = run(
            "transport-midi",
            &[
                "--scene",
                "transport-midi",
                "--size",
                "820x600",
                "--at",
                "2.5",
            ],
        );
        let status = text(&tree, "transport-status");
        for part in [
            "1 TAKES · 4 NOTES",
            "REPLAY 2 SENT · 2 PLAYED",
            "2 HELD NOT SOUNDED (NO INSTRUMENT LOADED)",
            "SEL T1: 4 NOTES · HELD 500ms/800ms",
        ] {
            assert!(status.contains(part), "{part}: {status}");
        }
        let elements = tree["elements"].as_array().unwrap();
        for key in [
            "transport-record",
            "transport-play",
            "transport-loop",
            "transport-mode",
            "transport-count-in",
            "transport-status",
        ] {
            let element = elements.iter().find(|e| e["key"] == key).unwrap();
            let [x, y, w, h]: [f64; 4] =
                std::array::from_fn(|index| element["rect"][index].as_f64().unwrap());
            assert!(
                x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0 && x + w <= 820.1 && y + h <= 600.1,
                "{key} overflows"
            );
        }
        assert_eq!(tree["warnings"].as_array().unwrap().len(), 0);
    }
}
