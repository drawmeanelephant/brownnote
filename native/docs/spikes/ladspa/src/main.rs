//! Standalone offline LADSPA probe for brownnote#97. Not part of the app.
//!
//! `list <library>` prints every descriptor with its validation result.
//! `render <library> <label> <input.wav> <out-dir> [options]` renders one
//! file through one plugin and publishes `render.wav` plus `render.json`
//! only when the whole render succeeded.

mod ladspa;
mod wav;

use ladspa::{Dir, Kind, Plugin};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// -90 dBFS. Output below this for `HOLD_SECONDS` ends the tail.
const TAIL_THRESHOLD: f32 = 3.162_277_7e-5;
const HOLD_SECONDS: f64 = 0.05;
const DEFAULT_MAX_TAIL_SECONDS: f64 = 30.0;
const DEFAULT_BLOCK: usize = 256;
const MAX_BLOCK: usize = 65_536;

const USAGE: &str = "usage:
  brownnote-ladspa-probe list <library> [--rate HZ]
  brownnote-ladspa-probe render <library> <label> <input.wav> <out-dir>
      [--block N] [--max-tail SECONDS] [--run-adding] [--set NAME=VALUE]...";

struct RenderOptions {
    library: String,
    label: String,
    input: PathBuf,
    out: PathBuf,
    block: usize,
    max_tail_seconds: f64,
    run_adding: bool,
    overrides: Vec<(String, f64)>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("list") => parse_list(&args[1..]).and_then(|(lib, rate)| list(&lib, rate)),
        Some("render") => parse_render(&args[1..]).and_then(|options| render_command(&options)),
        _ => Err(Failure::Usage(USAGE.into())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        Err(Failure::Render(message)) => {
            eprintln!("error: {message}");
            ExitCode::from(1)
        }
    }
}

enum Failure {
    Usage(String),
    Render(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure::Render(message)
    }
}

fn parse_list(args: &[String]) -> Result<(String, u32), Failure> {
    match args {
        [lib] => Ok((lib.clone(), 48_000)),
        [lib, flag, rate] if flag == "--rate" => rate
            .parse()
            .map(|rate| (lib.clone(), rate))
            .map_err(|_| Failure::Usage(USAGE.into())),
        _ => Err(Failure::Usage(USAGE.into())),
    }
}

fn parse_render(args: &[String]) -> Result<RenderOptions, Failure> {
    let usage = || Failure::Usage(USAGE.into());
    let [library, label, input, out, rest @ ..] = args else {
        return Err(usage());
    };
    let mut options = RenderOptions {
        library: library.clone(),
        label: label.clone(),
        input: PathBuf::from(input),
        out: PathBuf::from(out),
        block: DEFAULT_BLOCK,
        max_tail_seconds: DEFAULT_MAX_TAIL_SECONDS,
        run_adding: false,
        overrides: Vec::new(),
    };
    let mut rest = rest.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--block" => {
                options.block = rest.next().and_then(|v| v.parse().ok()).ok_or_else(usage)?;
                if options.block == 0 || options.block > MAX_BLOCK {
                    return Err(usage());
                }
            }
            "--max-tail" => {
                options.max_tail_seconds =
                    rest.next().and_then(|v| v.parse().ok()).ok_or_else(usage)?;
                if !(options.max_tail_seconds > 0.0 && options.max_tail_seconds <= 600.0) {
                    return Err(usage());
                }
            }
            "--run-adding" => options.run_adding = true,
            "--set" => {
                let pair = rest.next().ok_or_else(usage)?;
                let (name, value) = pair.split_once('=').ok_or_else(usage)?;
                let value = value.parse().map_err(|_| usage())?;
                options.overrides.push((name.to_owned(), value));
            }
            _ => return Err(usage()),
        }
    }
    Ok(options)
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn strings(items: &[&str]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| quote(item)).collect();
    format!("[{}]", quoted.join(", "))
}

fn number(value: Option<f64>) -> String {
    value.map_or_else(|| "null".into(), |v| format!("{v:?}"))
}

fn plugin_json(plugin: &Plugin, rate: u32, indent: &str) -> String {
    let mut ports = Vec::new();
    for (index, port) in plugin.ports.iter().enumerate() {
        let dir = if port.dir == Dir::Input {
            "input"
        } else {
            "output"
        };
        let kind = if port.kind == Kind::Audio {
            "audio"
        } else {
            "control"
        };
        let mut entry = format!(
            "{{\"index\": {index}, \"name\": {}, \"direction\": \"{dir}\", \"kind\": \"{kind}\", \"extra_descriptor_bits\": {}, \"hints\": {}, \"raw_lower\": {:?}, \"raw_upper\": {:?}",
            quote(&port.name),
            port.extra_bits,
            strings(&ladspa::hint_names(port.hint)),
            port.lower,
            port.upper,
        );
        if port.kind == Kind::Control && port.dir == Dir::Input {
            match ladspa::resolve_control(port, rate) {
                Ok(control) => {
                    let _ = write!(
                        entry,
                        ", \"resolved_lower\": {}, \"resolved_upper\": {}, \"resolved_default\": {:?}",
                        number(control.lower),
                        number(control.upper),
                        control.default
                    );
                }
                Err(error) => {
                    let _ = write!(entry, ", \"hint_error\": {}", quote(&error));
                }
            }
        }
        entry.push('}');
        ports.push(format!("{indent}    {entry}"));
    }
    format!(
        "{{\n{indent}  \"unique_id\": {},\n{indent}  \"label\": {},\n{indent}  \"name\": {},\n{indent}  \"maker\": {},\n{indent}  \"copyright\": {},\n{indent}  \"properties\": {},\n{indent}  \"run_adding\": {},\n{indent}  \"activate\": {},\n{indent}  \"deactivate\": {},\n{indent}  \"hint_rate\": {rate},\n{indent}  \"ports\": [\n{}\n{indent}  ]\n{indent}}}",
        plugin.unique_id,
        quote(&plugin.label),
        quote(&plugin.name),
        quote(&plugin.maker),
        quote(&plugin.copyright),
        strings(&plugin.property_names()),
        plugin.raw.run_adding.is_some(),
        plugin.raw.activate.is_some(),
        plugin.raw.deactivate.is_some(),
        ports.join(",\n"),
    )
}

fn list(library: &str, rate: u32) -> Result<(), Failure> {
    let (path, _) = ladspa::resolve(library)?;
    let lib = ladspa::Library::open(&path)?;
    let mut entries = Vec::new();
    for raw in lib.descriptors()? {
        match ladspa::validate(raw) {
            Ok(plugin) => entries.push(format!("    {}", plugin_json(&plugin, rate, "    "))),
            Err(error) => entries.push(format!(
                "    {{\"label\": {}, \"error\": {}}}",
                quote(&ladspa::label_of(raw)),
                quote(&error)
            )),
        }
    }
    println!(
        "{{\n  \"library\": {},\n  \"descriptors\": [\n{}\n  ]\n}}",
        quote(&path.display().to_string()),
        entries.join(",\n")
    );
    Ok(())
}

/// Owns one plugin instance and always deactivates and cleans it up.
struct Instance<'a> {
    raw: &'a ladspa::Descriptor,
    handle: ladspa::Handle,
    active: bool,
}

impl Drop for Instance<'_> {
    fn drop(&mut self) {
        // SAFETY: handle came from instantiate and is released exactly once;
        // validate() guaranteed cleanup is present.
        unsafe {
            if self.active
                && let Some(deactivate) = self.raw.deactivate
            {
                deactivate(self.handle);
            }
            if let Some(cleanup) = self.raw.cleanup {
                cleanup(self.handle);
            }
        }
    }
}

struct Rendered {
    audio: wav::Audio,
    input_frames: usize,
    tail_frames: usize,
    channel_map: &'static str,
    controls: Vec<(String, f64, &'static str)>,
    control_outputs: Vec<(String, f32)>,
}

fn render(
    plugin: &Plugin,
    input: &wav::Audio,
    options: &RenderOptions,
) -> Result<Rendered, String> {
    let ins = plugin.audio(Dir::Input);
    let outs = plugin.audio(Dir::Output);
    if !(1..=2).contains(&ins.len()) || !(1..=2).contains(&outs.len()) {
        return Err(format!(
            "plugin {}: unsupported channel layout {} in / {} out (supported: 1-2 in, 1-2 out)",
            plugin.label,
            ins.len(),
            outs.len()
        ));
    }
    let channel_map = match (input.channels.len(), ins.len()) {
        (a, b) if a == b => "identity",
        (1, 2) => "mono input duplicated to both plugin inputs",
        _ => "stereo input averaged into one plugin input",
    };
    if options.run_adding && plugin.raw.run_adding.is_none() {
        return Err(format!("plugin {} has no run_adding", plugin.label));
    }

    let rate = input.rate;
    let mut controls = vec![0.0f32; plugin.ports.len()];
    let mut resolved = Vec::new();
    for (index, port) in plugin.ports.iter().enumerate() {
        if port.kind != Kind::Control || port.dir != Dir::Input {
            continue;
        }
        let control = ladspa::resolve_control(port, rate)?;
        let (value, source) = match options
            .overrides
            .iter()
            .find(|(name, _)| *name == port.name)
        {
            Some(&(_, value)) => {
                if !control.admits(value) {
                    return Err(format!(
                        "control {} = {value} is outside its hints (lower {}, upper {})",
                        port.name,
                        number(control.lower),
                        number(control.upper)
                    ));
                }
                (value, "override")
            }
            None => (control.default, "default"),
        };
        controls[index] = value as f32;
        resolved.push((port.name.clone(), value, source));
    }
    for (name, _) in &options.overrides {
        let known = plugin
            .ports
            .iter()
            .any(|p| p.kind == Kind::Control && p.dir == Dir::Input && p.name == *name);
        if !known {
            return Err(format!(
                "plugin {} has no control input {name}",
                plugin.label
            ));
        }
    }

    let block = options.block;
    let mut buffers = vec![vec![0.0f32; block]; plugin.ports.len()];
    let pointers: Vec<*mut f32> = buffers.iter_mut().map(|b| b.as_mut_ptr()).collect();
    let controls_ptr = controls.as_mut_ptr();
    let raw = plugin.raw;
    let (Some(instantiate), Some(connect), Some(run)) =
        (raw.instantiate, raw.connect_port, raw.run)
    else {
        return Err("descriptor lost a required function".into());
    };
    // SAFETY: raw is a validated descriptor from a loaded library.
    let handle = unsafe { instantiate(raw, std::ffi::c_ulong::from(rate)) };
    if handle.is_null() {
        return Err(format!(
            "plugin {} failed to instantiate at {rate} Hz",
            plugin.label
        ));
    }
    let mut instance = Instance {
        raw,
        handle,
        active: false,
    };
    for (index, port) in plugin.ports.iter().enumerate() {
        let location = if port.kind == Kind::Audio {
            pointers[index]
        } else {
            // SAFETY: index < ports.len() == controls.len().
            unsafe { controls_ptr.add(index) }
        };
        // SAFETY: every port gets storage that outlives the instance.
        unsafe { connect(handle, index as std::ffi::c_ulong, location) };
    }
    if let Some(activate) = raw.activate {
        // SAFETY: all ports are connected.
        unsafe { activate(handle) };
    }
    instance.active = true;
    if options.run_adding
        && let Some(gain) = raw.set_run_adding_gain
    {
        // SAFETY: instance is live.
        unsafe { gain(handle, 1.0) };
    }

    let input_frames = input.channels.first().map_or(0, Vec::len);
    let hold = (HOLD_SECONDS * f64::from(rate)).ceil() as usize;
    let max_tail = (options.max_tail_seconds * f64::from(rate)).ceil() as usize;
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(input_frames + hold); outs.len()];
    let mut position = 0usize;
    let mut quiet = 0usize;
    let mut last_loud: Option<usize> = None;
    loop {
        if position >= input_frames + hold && quiet >= hold {
            break;
        }
        if position > input_frames + max_tail + hold {
            return Err(format!(
                "plugin {}: tail did not fall below -90 dBFS within {} s",
                plugin.label, options.max_tail_seconds
            ));
        }
        let frames = block;
        for (slot, &port) in ins.iter().enumerate() {
            // SAFETY: each audio buffer holds `block` frames.
            let dest = unsafe { std::slice::from_raw_parts_mut(pointers[port], block) };
            for (offset, sample) in dest.iter_mut().enumerate() {
                let frame = position + offset;
                *sample = if frame >= input_frames {
                    0.0
                } else if input.channels.len() == ins.len() {
                    input.channels[slot][frame]
                } else if input.channels.len() == 1 {
                    input.channels[0][frame]
                } else {
                    0.5 * (input.channels[0][frame] + input.channels[1][frame])
                };
            }
        }
        let count = frames as std::ffi::c_ulong;
        if options.run_adding {
            for &port in &outs {
                // SAFETY: as above.
                unsafe { std::slice::from_raw_parts_mut(pointers[port], block) }.fill(0.0);
            }
            if let Some(run_adding) = raw.run_adding {
                // SAFETY: instance is active and every port is connected.
                unsafe { run_adding(handle, count) };
            }
        } else {
            // SAFETY: instance is active and every port is connected.
            unsafe { run(handle, count) };
        }
        for offset in 0..frames {
            let mut loud = false;
            for (slot, &port) in outs.iter().enumerate() {
                // SAFETY: offset < block.
                let value = unsafe { *pointers[port].add(offset) };
                if !value.is_finite() {
                    return Err(format!(
                        "plugin {}: non-finite output at frame {} on {}",
                        plugin.label,
                        position + offset,
                        plugin.ports[port].name
                    ));
                }
                loud |= value.abs() >= TAIL_THRESHOLD;
                out[slot].push(value);
            }
            if loud {
                last_loud = Some(position + offset);
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        position += frames;
    }
    let control_outputs = plugin
        .ports
        .iter()
        .enumerate()
        .filter(|(_, p)| p.kind == Kind::Control && p.dir == Dir::Output)
        // SAFETY: index < controls.len(); the plugin no longer runs.
        .map(|(index, p)| (p.name.clone(), unsafe { *controls_ptr.add(index) }))
        .collect();
    drop(instance);
    drop(buffers);

    let length = input_frames.max(last_loud.map_or(0, |frame| frame + 1));
    for channel in &mut out {
        channel.truncate(length);
    }
    Ok(Rendered {
        audio: wav::Audio {
            rate,
            channels: out,
        },
        input_frames,
        tail_frames: length - input_frames,
        channel_map,
        controls: resolved,
        control_outputs,
    })
}

fn render_command(options: &RenderOptions) -> Result<(), Failure> {
    if options.out.exists() {
        return Err(format!("output already exists: {}", options.out.display()).into());
    }
    let (path, search) = ladspa::resolve(&options.library)?;
    let library = ladspa::Library::open(&path)?;
    let raws: Vec<_> = library
        .descriptors()?
        .into_iter()
        .filter(|raw| ladspa::label_of(raw) == options.label)
        .collect();
    let raw = match raws.as_slice() {
        [raw] => *raw,
        [] => {
            return Err(format!(
                "plugin label {} not found in {}",
                options.label,
                path.display()
            )
            .into());
        }
        _ => return Err(format!("plugin label {} is not unique", options.label).into()),
    };
    let plugin = ladspa::validate(raw)?;
    let input = wav::read(&options.input)?;
    let rendered = render(&plugin, &input, options)?;

    let rate = rendered.audio.rate;
    let peaks: Vec<String> = rendered
        .audio
        .channels
        .iter()
        .map(|c| format!("{:?}", c.iter().fold(0.0f32, |m, v| m.max(v.abs()))))
        .collect();
    let controls: Vec<String> = rendered
        .controls
        .iter()
        .map(|(name, value, source)| {
            format!(
                "    {{\"name\": {}, \"value\": {value:?}, \"source\": \"{source}\"}}",
                quote(name)
            )
        })
        .collect();
    let control_outputs: Vec<String> = rendered
        .control_outputs
        .iter()
        .map(|(name, value)| format!("    {}: {value:?}", quote(name)))
        .collect();
    let dirs: Vec<String> = search
        .dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect();
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let record = format!(
        "{{\n  \"library\": {},\n  \"ladspa_path\": {},\n  \"search_dirs\": {},\n  \"host\": \"{}-{}\",\n  \"plugin\": {},\n  \"input\": {{\"path\": {}, \"channels\": {}, \"frames\": {}, \"rate\": {rate}}},\n  \"channel_map\": \"{}\",\n  \"block_frames\": {},\n  \"process_call\": \"{}\",\n  \"buffers\": \"separate input and output buffers (in-place never used)\",\n  \"controls\": [\n{}\n  ],\n  \"control_outputs\": {{\n{}\n  }},\n  \"tail\": {{\"threshold_dbfs\": -90.0, \"hold_seconds\": {HOLD_SECONDS:?}, \"max_seconds\": {:?}, \"frames\": {}, \"seconds\": {:?}}},\n  \"output\": {{\"file\": \"render.wav\", \"format\": \"float32\", \"channels\": {}, \"frames\": {}, \"rate\": {rate}, \"peaks\": [{}]}}\n}}\n",
        quote(&path.display().to_string()),
        search
            .ladspa_path
            .as_deref()
            .map_or_else(|| "null".into(), quote),
        strings(&dirs),
        std::env::consts::ARCH,
        std::env::consts::OS,
        plugin_json(&plugin, rate, "  "),
        quote(&options.input.display().to_string()),
        input.channels.len(),
        rendered.input_frames,
        rendered.channel_map,
        options.block,
        if options.run_adding {
            "run_adding"
        } else {
            "run"
        },
        controls.join(",\n"),
        control_outputs.join(",\n"),
        options.max_tail_seconds,
        rendered.tail_frames,
        rendered.tail_frames as f64 / f64::from(rate),
        rendered.audio.channels.len(),
        rendered.input_frames + rendered.tail_frames,
        peaks.join(", "),
    );
    publish(&options.out, &rendered.audio, &record)
        .map_err(|error| Failure::Render(format!("cannot publish render: {error}")))
}

/// Writes into a sibling staging directory and renames it into place, so a
/// failed write never leaves a partial `out` directory behind.
fn publish(out: &Path, audio: &wav::Audio, record: &str) -> std::io::Result<()> {
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = out
        .file_name()
        .map_or_else(|| "render".into(), |n| n.to_string_lossy().into_owned());
    let staging = parent.join(format!(".{name}.partial-{}", std::process::id()));
    std::fs::create_dir(&staging)?;
    let result = (|| {
        wav::write_float(&staging.join("render.wav"), audio)?;
        std::fs::write(staging.join("render.json"), record)?;
        std::fs::rename(&staging, out)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}
