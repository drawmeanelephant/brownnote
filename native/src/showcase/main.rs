//! A visual showcase of the native instrument on the real engine: three looks over one
//! instrument, an effect lab, and measurement switches. Same samples, keys, velocity, choke and
//! panic as `brownnote-native`; nothing here touches the audio callback.

mod lab;
mod latency;
#[cfg(test)]
mod motion_tests;
mod params;
mod state;
#[cfg(test)]
mod tests;
mod view;

use brownnote_native::{
    kit::{DEFAULT_SAMPLES, Kit},
    look::{
        fx::{KINDS, Look},
        theme::{FxLevel, ThemeId},
    },
    output::Output,
};
use state::{Options, State, View};
use std::path::PathBuf;

struct Args {
    options: Options,
    samples: PathBuf,
    no_audio: bool,
    probe: bool,
}

fn args(args: Vec<String>) -> Result<Args, String> {
    let mut out = Args {
        options: Options::default(),
        samples: DEFAULT_SAMPLES.into(),
        no_audio: false,
        probe: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} requires a value"));
        match arg.as_str() {
            "--look" => {
                let name = value("--look")?;
                out.options.theme = ThemeId::parse(&name)
                    .ok_or(format!("Unknown look {name}: bunker, space or afterhours"))?;
            }
            "--fx" => {
                let name = value("--fx")?;
                out.options.fx = FxLevel::parse(&name)
                    .ok_or(format!("Unknown --fx {name}: full, light or off"))?;
            }
            "--only" => {
                let mut mask = 0;
                for name in value("--only")?.split(',') {
                    let kind = KINDS
                        .iter()
                        .find(|(_, n)| n.eq_ignore_ascii_case(name))
                        .ok_or(format!("Unknown effect {name}"))?;
                    mask |= 1 << kind.0;
                }
                out.options.mask = mask;
            }
            "--reduced-motion" => out.options.reduced_motion = true,
            "--lab" => out.options.view = View::Lab,
            "--probe" => out.probe = true,
            "--samples" => out.samples = value("--samples")?.into(),
            "--no-audio" => out.no_audio = true,
            _ => return Err(format!("Unknown showcase option: {arg}")),
        }
    }
    Ok(out)
}

fn main() {
    let args = match args(rune::extra_args().to_vec()) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let kit = Kit::load(&args.samples);
    let samples = kit.samples.clone();
    let output = if rune::is_headless() || args.no_audio {
        Output::virtual_driver(
            samples,
            if rune::is_headless() {
                "Headless: no audio device".into()
            } else {
                "Audio off (--no-audio)".into()
            },
            false,
        )
    } else {
        Output::open(samples)
            .unwrap_or_else(|e| Output::virtual_driver(kit.samples.clone(), e.to_string(), true))
    };
    let label = format!(
        "{}/{}{}",
        args.options.theme.slug(),
        args.options.fx.slug(),
        if args.options.mask == brownnote_native::look::fx::ALL_KINDS {
            String::new()
        } else {
            format!("/mask {:#x}", args.options.mask)
        }
    );
    let options = args.options;
    let app = rune::App::new(rune::AppSpec {
        min_size: Some((820.0, 600.0)),
        on_key: Some(state::on_key),
        poll: Some(state::poll),
        setup: Some(state::setup),
        scenes: state::SCENES,
        ..rune::AppSpec::new(
            "Brownnote · Look showcase",
            (1120.0, 800.0),
            || State::headless(&Options::default()),
            view::view,
        )
    })
    .init(move || State::new(kit, output, &options));
    if args.probe {
        app.compositor(Look::probed(&label))
            .on_frame_encoded(latency::frame)
            .run();
    } else {
        app.compositor(Look::default()).run();
    }
}
