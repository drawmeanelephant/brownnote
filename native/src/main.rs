mod ui;
#[cfg(test)]
mod ui_pedals_tests;
#[cfg(test)]
mod ui_test_support;
#[cfg(test)]
mod ui_tests;

use brownnote_native::{
    kit::{DEFAULT_SAMPLES, Kit},
    output::{OutputSelection, SelectionSource, check_audio},
    pedals::{
        brownnote::{self, DEFAULT_BIN},
        catalog::Catalog,
    },
};
use std::{path::PathBuf, time::Duration};

/// What `--check-audio` should measure. `None` keeps today's system default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionMode {
    SystemDefault,
    Saved,
}

/// What the probe does once the headless guard has been applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeRequest {
    Refused,
    SystemDefault,
    Saved,
}

struct Options {
    samples: PathBuf,
    no_audio: bool,
    check_audio: bool,
    selection: Option<SelectionMode>,
    list_pedals: bool,
    version: bool,
    brownnote: PathBuf,
    pedal_cache: PathBuf,
}

fn options(args: Vec<String>) -> Result<Options, String> {
    let mut options = Options {
        samples: DEFAULT_SAMPLES.into(),
        no_audio: false,
        check_audio: false,
        selection: None,
        list_pedals: false,
        version: false,
        brownnote: DEFAULT_BIN.into(),
        pedal_cache: brownnote_native::paths::DEFAULT_CACHE.into(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--samples" => options.samples = args.next().ok_or("--samples requires DIR")?.into(),
            "--brownnote" => {
                options.brownnote = args.next().ok_or("--brownnote requires PATH")?.into()
            }
            "--pedal-cache" => {
                options.pedal_cache = args.next().ok_or("--pedal-cache requires DIR")?.into()
            }
            "--no-audio" => options.no_audio = true,
            "--check-audio" => options.check_audio = true,
            "--selection" => {
                let value = args.next().ok_or("--selection requires saved or default")?;
                options.selection = Some(match value.as_str() {
                    "saved" => SelectionMode::Saved,
                    "default" => SelectionMode::SystemDefault,
                    _ => return Err(format!("Unknown --selection value: {value}")),
                });
            }
            "--list-pedals" => options.list_pedals = true,
            "--version" => options.version = true,
            _ => return Err(format!("Unknown app option: {arg}")),
        }
    }
    if options.selection.is_some() && !options.check_audio {
        return Err("--selection requires --check-audio".into());
    }
    Ok(options)
}

/// The headless/`--no-audio` refusal applies to the saved probe exactly as it
/// does to the system-default one, before any preference is read.
fn probe_request(options: &Options, headless: bool) -> ProbeRequest {
    if headless || options.no_audio {
        return ProbeRequest::Refused;
    }
    match options.selection {
        Some(SelectionMode::Saved) => ProbeRequest::Saved,
        _ => ProbeRequest::SystemDefault,
    }
}

/// Resolve the machine root the app itself uses, then read the saved selection
/// strictly: no file, bad JSON, another version, a malformed ID or a zero
/// configuration is an error, never an implicit system default (D16).
fn saved_selection() -> Result<OutputSelection, String> {
    let root = brownnote_native::paths::Resolver::current()
        .and_then(|paths| paths.resolve(brownnote_native::paths::Resource::Preferences, None))
        .map_err(|error| error.to_string())?;
    ui::devices::preferences::load_saved(&root)
}

/// The identity `--version` prints: crate version, then the full commit this executable
/// was built from, marked `dirty` only when that build had uncommitted changes.
fn version_text(version: &str, commit: &str, dirty: bool) -> String {
    let marker = if dirty { " (dirty)" } else { "" };
    format!("brownnote-native {version}\ncommit {commit}{marker}")
}

/// The identity compiled in by `build.rs`, so `--version` needs no repository or network.
fn build_version() -> String {
    version_text(
        env!("CARGO_PKG_VERSION"),
        env!("BROWNNOTE_SOURCE_COMMIT"),
        matches!(env!("BROWNNOTE_SOURCE_DIRTY"), "1"),
    )
}

fn print_catalog(catalog: &Catalog) {
    let mut count = 0;
    for effect in catalog.routable() {
        let pots: Vec<_> = effect
            .pots
            .iter()
            .map(|pot| format!("{} 0..{}={}", pot.label, pot.max, pot.default))
            .collect();
        if pots.is_empty() {
            println!("{} [mix: {}]", effect.name, effect.mix.text());
        } else {
            println!(
                "{} [mix: {}]  {}",
                effect.name,
                effect.mix.text(),
                pots.join(", ")
            );
        }
        count += 1;
    }
    println!("{count} routable pedals");
}

fn main() {
    let options = match options(rune::extra_args().to_vec()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    // Identity first, before any kit, device or window work: a downloaded binary must
    // report what it is even when its resources are missing or it has no display.
    if options.version {
        println!("{}", build_version());
        return;
    }
    let kit = Kit::load(&options.samples);
    if options.list_pedals {
        if rune::is_headless() || options.no_audio {
            eprintln!("--list-pedals cannot be combined with headless flags or --no-audio");
            std::process::exit(3);
        }
        match brownnote::list(&options.brownnote, Duration::from_secs(5)) {
            Ok(catalog) => print_catalog(&catalog),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(3);
            }
        }
        return;
    }
    if options.check_audio {
        let request = probe_request(&options, rune::is_headless());
        if request == ProbeRequest::Refused {
            eprintln!("--check-audio cannot be combined with headless flags or --no-audio");
            std::process::exit(3);
        }
        // Only the saved probe reads preferences, and only after the refusal
        // above, so headless and --no-audio sessions still touch no machine
        // preference. A saved selection that cannot be loaded or resolved
        // stops here without opening a stream or measuring the default.
        let (selection, source) = if request == ProbeRequest::Saved {
            match saved_selection() {
                Ok(selection) => (selection, SelectionSource::Saved),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(3);
                }
            }
        } else {
            (OutputSelection::default(), SelectionSource::SystemDefault)
        };
        if let Some(error) = &kit.errors[0] {
            eprintln!("{error}");
            std::process::exit(3);
        }
        match check_audio(kit.samples, &selection, source) {
            Ok(report) => println!("{report}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(3);
            }
        }
        return;
    }
    rune::App::new(rune::AppSpec {
        min_size: Some((820.0, 600.0)),
        on_key: Some(ui::on_key),
        poll: Some(ui::poll),
        setup: Some(ui::setup),
        scenes: ui::SCENES,
        ..rune::AppSpec::new(
            "Brownnote · Native pads",
            (1120.0, 800.0),
            ui::init,
            ui::view,
        )
    })
    .init({
        let cache = options.pedal_cache.clone();
        let brownnote = options.brownnote.clone();
        // The same path for the catalog and for renders, so one build's pedal
        // list never edits against another build's pedals.
        move || ui::State::with_cache(kit, rune::is_headless(), options.no_audio, cache, brownnote)
    })
    .run();
}

#[cfg(test)]
mod option_tests {
    use super::{
        DEFAULT_BIN, DEFAULT_SAMPLES, ProbeRequest, SelectionMode, options, probe_request,
    };
    use brownnote_native::paths::DEFAULT_CACHE;
    use std::path::PathBuf;

    fn parsed(arguments: &[&str]) -> super::Options {
        options(
            arguments
                .iter()
                .map(|argument| argument.to_string())
                .collect(),
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn rejected(arguments: &[&str]) -> String {
        match options(
            arguments
                .iter()
                .map(|argument| argument.to_string())
                .collect(),
        ) {
            Ok(_) => panic!("accepted {arguments:?}"),
            Err(error) => error,
        }
    }

    #[test]
    fn runtime_defaults_are_shared_by_cli_and_pedal_ui() {
        let defaults = options(Vec::new()).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(defaults.samples, PathBuf::from(DEFAULT_SAMPLES));
        assert_eq!(defaults.brownnote, PathBuf::from(DEFAULT_BIN));
        assert_eq!(defaults.pedal_cache, PathBuf::from(DEFAULT_CACHE));
        assert_eq!(super::ui::pedals::default_cache(), defaults.pedal_cache);
    }

    #[test]
    fn explicit_resource_options_replace_runtime_defaults() {
        let arguments = [
            "--samples",
            "/chosen/samples",
            "--brownnote",
            "/chosen/brownnote",
            "--pedal-cache",
            "/chosen/cache",
        ];
        let selected = options(arguments.into_iter().map(String::from).collect())
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(selected.samples, PathBuf::from("/chosen/samples"));
        assert_eq!(selected.brownnote, PathBuf::from("/chosen/brownnote"));
        assert_eq!(selected.pedal_cache, PathBuf::from("/chosen/cache"));
    }

    #[test]
    fn selection_defaults_to_absent_and_only_saved_opts_in() {
        let defaults = parsed(&[]);
        assert!(!defaults.check_audio);
        assert_eq!(defaults.selection, None);
        assert_eq!(probe_request(&defaults, false), ProbeRequest::SystemDefault);

        let saved = parsed(&["--check-audio", "--selection", "saved"]);
        assert!(saved.check_audio);
        assert_eq!(saved.selection, Some(SelectionMode::Saved));
        assert_eq!(probe_request(&saved, false), ProbeRequest::Saved);

        let explicit_default = parsed(&["--check-audio", "--selection", "default"]);
        assert_eq!(
            explicit_default.selection,
            Some(SelectionMode::SystemDefault)
        );
        assert_eq!(
            probe_request(&explicit_default, false),
            ProbeRequest::SystemDefault
        );
    }

    #[test]
    fn selection_is_refused_without_the_probe_or_a_known_value() {
        assert!(rejected(&["--selection", "saved"]).contains("requires --check-audio"));
        assert!(rejected(&["--check-audio", "--selection"]).contains("saved or default"));
        assert!(
            rejected(&["--check-audio", "--selection", "remembered"])
                .contains("Unknown --selection value: remembered")
        );
    }

    #[test]
    fn saved_probe_keeps_the_headless_and_no_audio_refusal() {
        let saved = parsed(&["--check-audio", "--selection", "saved"]);
        assert_eq!(probe_request(&saved, true), ProbeRequest::Refused);
        let muted = parsed(&["--check-audio", "--selection", "saved", "--no-audio"]);
        assert_eq!(probe_request(&muted, false), ProbeRequest::Refused);
        assert_eq!(probe_request(&muted, true), ProbeRequest::Refused);
    }

    #[test]
    fn version_flag_is_recognized_and_off_by_default() {
        let defaults = options(Vec::new()).unwrap_or_else(|error| panic!("{error}"));
        assert!(!defaults.version);
        let selected =
            options(vec!["--version".to_owned()]).unwrap_or_else(|error| panic!("{error}"));
        assert!(selected.version);
        let shared = options(vec![
            "--version".to_owned(),
            "--samples".to_owned(),
            "/other".to_owned(),
        ])
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(shared.version);
        assert_eq!(shared.samples, PathBuf::from("/other"));
    }

    #[test]
    fn version_text_marks_only_a_dirty_build() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            super::version_text("0.1.0", commit, false),
            format!("brownnote-native 0.1.0\ncommit {commit}")
        );
        let dirty = super::version_text("0.1.0", commit, true);
        assert_eq!(
            dirty,
            format!("brownnote-native 0.1.0\ncommit {commit} (dirty)")
        );
        assert!(!super::version_text("0.1.0", commit, false).contains("dirty"));
    }

    #[test]
    fn built_version_prints_crate_version_and_full_commit() {
        let text = super::build_version();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(concat!("brownnote-native ", env!("CARGO_PKG_VERSION")))
        );
        let commit = lines
            .next()
            .and_then(|line| line.strip_prefix("commit "))
            .unwrap_or_else(|| panic!("no commit line: {text}"));
        let commit = commit.strip_suffix(" (dirty)").unwrap_or(commit);
        assert!(
            commit == "unknown"
                || (commit.len() == 40
                    && commit
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())),
            "not a full lower-case source commit: {commit:?}"
        );
        assert_eq!(lines.next(), None);
    }
}
