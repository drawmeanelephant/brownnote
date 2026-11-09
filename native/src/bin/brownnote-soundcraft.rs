use brownnote_native::adapters::soundcraft::{Request, convert};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut flags = BTreeMap::new();
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!(
                "brownnote-soundcraft --bundle DIR --app PATH --cli PATH --output NEW_DIR [--mappings JSON]\nPinned SoundCraft 0.19.0; owns an isolated --no-audio GUI. Move the entire output directory.\nOptional mappings: JSON array of part, asset, root_note, key_low, key_high, key_track, one_shot, choke_group.\nReal-app v0.19.0 revalidation and maintainer listening remain required (D19)."
            );
            return Ok(());
        }
        if !["--bundle", "--app", "--cli", "--output", "--mappings"]
            .iter()
            .any(|known| flag == *known)
        {
            return Err(format!("unknown argument: {}", flag.to_string_lossy()).into());
        }
        let value = args.next().ok_or("missing flag value")?;
        if value.to_string_lossy().starts_with("--") {
            return Err("missing flag value".into());
        }
        if flags.insert(flag, value).is_some() {
            return Err("duplicate flag".into());
        }
    }
    let mut path = |flag: &str| -> Result<PathBuf, Box<dyn std::error::Error>> {
        flags
            .remove(std::ffi::OsStr::new(flag))
            .map(PathBuf::from)
            .ok_or_else(|| format!("required: {flag} (see --help)").into())
    };
    let mut request = Request {
        bundle: path("--bundle")?,
        app: path("--app")?,
        cli: path("--cli")?,
        output: path("--output")?,
        mappings: Vec::new(),
        timeout: Duration::from_secs(45),
    };
    if let Some(file) = flags.remove(std::ffi::OsStr::new("--mappings")) {
        request.mappings = serde_json::from_slice(&std::fs::read(file)?)?;
    }
    let report = convert(&request)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
