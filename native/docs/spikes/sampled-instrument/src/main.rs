use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Config {
    settings: SynthesizerSettings,
    volume: f32,
    bank: i32,
    program: i32,
}

impl Config {
    fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        let fields: BTreeMap<_, _> = text
            .lines()
            .map(|line| line.split_once('=').ok_or("invalid config line"))
            .collect::<std::result::Result<_, _>>()?;
        let get = |key| fields.get(key).copied().ok_or("missing config field");
        let mut settings = SynthesizerSettings::new(get("sample_rate")?.parse()?);
        settings.block_size = get("block_size")?.parse()?;
        settings.maximum_polyphony = get("maximum_polyphony")?.parse()?;
        settings.enable_reverb_and_chorus = get("effects")?.parse()?;
        Ok(Self {
            settings,
            volume: get("master_volume")?.parse()?,
            bank: get("bank")?.parse()?,
            program: get("program")?.parse()?,
        })
    }

    fn apply(&self, synth: &mut Synthesizer) {
        synth.set_master_volume(self.volume);
        for channel in 0..16 {
            synth.process_midi_message(channel, 0xB0, 0, self.bank);
            synth.process_midi_message(channel, 0xC0, self.program, 0);
        }
    }
}

struct Event {
    frame: usize,
    op: String,
    channel: i32,
    key: i32,
    value: i32,
    id: String,
}

fn read_events(path: &Path) -> Result<BTreeMap<String, Vec<Event>>> {
    let mut cases: BTreeMap<String, Vec<Event>> = BTreeMap::new();
    for line in fs::read_to_string(path)?.lines().skip(1) {
        let f: Vec<_> = line.split('\t').collect();
        if f.len() != 7 || !f[0].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("invalid event row".into());
        }
        let event = Event {
            frame: f[1].parse()?,
            op: f[2].into(),
            channel: f[3].parse()?,
            key: f[4].parse()?,
            value: f[5].parse()?,
            id: f[6].into(),
        };
        if event.frame > 480_000
            || !(0..16).contains(&event.channel)
            || !(0..128).contains(&event.key)
            || !(0..128).contains(&event.value)
        {
            return Err("event outside probe bounds".into());
        }
        cases.entry(f[0].into()).or_default().push(event);
    }
    Ok(cases)
}

fn run(sf2: &Path, config_path: &Path, events_path: &Path, out: &Path) -> Result<()> {
    // Load before creating outputs, so an absent/corrupt asset fails explicitly.
    let font = Arc::new(SoundFont::new(&mut File::open(sf2)?)?);
    let cases = read_events(events_path)?;
    fs::create_dir_all(out)?;
    let saved_config = out.join("recall.txt");
    fs::copy(config_path, &saved_config)?;
    let mut trace = File::create(out.join("trace.tsv"))?;
    writeln!(
        trace,
        "scenario\tframe\top\tchannel\tkey_or_cc\tvalue\tnote_id"
    )?;
    for (label, events) in cases {
        let config = Config::read(if label == "reopen" {
            &saved_config
        } else {
            config_path
        })?;
        let mut synth = Synthesizer::new(&font, &config.settings)?;
        config.apply(&mut synth);
        let end = events.last().ok_or("empty scenario")?;
        if end.op != "end" {
            return Err("scenario needs final end event".into());
        }
        let mut left = vec![0.0; end.frame];
        let mut right = vec![0.0; end.frame];
        let mut cursor = 0;
        for event in &events {
            if event.frame < cursor {
                return Err("events are not ordered".into());
            }
            synth.render(
                &mut left[cursor..event.frame],
                &mut right[cursor..event.frame],
            );
            cursor = event.frame;
            // IDs remain trace metadata. This backend accepts only channel/pitch.
            match event.op.as_str() {
                "on" => synth.note_on(event.channel, event.key, event.value),
                "off" => synth.note_off(event.channel, event.key),
                "cc" => synth.process_midi_message(event.channel, 0xB0, event.key, event.value),
                "program" => synth.process_midi_message(event.channel, 0xC0, event.key, 0),
                "panic" => synth.note_off_all(true),
                "reset" => {
                    synth.reset();
                    config.apply(&mut synth);
                }
                "end" => (),
                _ => return Err("unknown event operation".into()),
            }
            writeln!(
                trace,
                "{label}\t{}\t{}\t{}\t{}\t{}\t{}",
                event.frame, event.op, event.channel, event.key, event.value, event.id
            )?;
        }
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: config.settings.sample_rate as u32,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(out.join(format!("{label}.wav")), spec)?;
        for (l, r) in left.into_iter().zip(right) {
            for sample in [l, r] {
                if !sample.is_finite() || sample.abs() >= 1.0 {
                    return Err(format!("non-finite/clipping output in {label}").into());
                }
                wav.write_sample((sample * 32767.0).round() as i16)?;
            }
        }
        wav.finalize()?;
        println!(
            "{label}: {} frames, maximum_polyphony={}",
            end.frame,
            synth.get_maximum_polyphony()
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: probe <soundfont.sf2> <config.txt> <events.tsv> <output-dir>".into());
    }
    run(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
    )
}
