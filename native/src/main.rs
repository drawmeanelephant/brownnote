mod ui;
#[cfg(test)]
mod ui_tests;

use brownnote_native::{
    kit::{DEFAULT_SAMPLES, Kit},
    output::check_audio,
    pedals::{
        brownnote::{self, DEFAULT_BIN},
        catalog::Catalog,
    },
};
use std::{path::PathBuf, time::Duration};

struct Options {
    samples: PathBuf,
    no_audio: bool,
    check_audio: bool,
    list_pedals: bool,
    brownnote: PathBuf,
}

fn options(args: Vec<String>) -> Result<Options, String> {
    let mut options = Options {
        samples: DEFAULT_SAMPLES.into(),
        no_audio: false,
        check_audio: false,
        list_pedals: false,
        brownnote: DEFAULT_BIN.into(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--samples" => options.samples = args.next().ok_or("--samples requires DIR")?.into(),
            "--brownnote" => {
                options.brownnote = args.next().ok_or("--brownnote requires PATH")?.into()
            }
            "--no-audio" => options.no_audio = true,
            "--check-audio" => options.check_audio = true,
            "--list-pedals" => options.list_pedals = true,
            _ => return Err(format!("Unknown app option: {arg}")),
        }
    }
    Ok(options)
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
        if rune::is_headless() || options.no_audio {
            eprintln!("--check-audio cannot be combined with headless flags or --no-audio");
            std::process::exit(3);
        }
        if let Some(error) = &kit.errors[0] {
            eprintln!("{error}");
            std::process::exit(3);
        }
        match check_audio(kit.samples) {
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
    .init(move || ui::State::new(kit, rune::is_headless(), options.no_audio))
    .run();
}
