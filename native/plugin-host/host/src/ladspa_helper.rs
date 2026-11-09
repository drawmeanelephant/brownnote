//! The LADSPA side of the offline render protocol: one allow-listed plugin
//! instance, opened from an `O` message, fed blocks until `E`.
//!
//! Identity: backend `ladspa`, ID `UniqueID:Label`, ABI `ladspa 1.1`, the
//! SHA-256 of the loaded library file, and version `unversioned` (LADSPA
//! has no version field, so the library hash pins it). Parameter IDs are
//! control input port indices.
use crate::ladspa::{Control, Direction, Instance, Kind, LadspaError, Library, PROPERTY_REALTIME};
use crate::protocol::{self, FromHelper, Open, ParamInfo, Plugin, PluginInfo, ToHelper};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub const ABI: &str = "ladspa 1.1";
const VERSION: &str = "unversioned";

/// The plugins this helper loads unless the command line adds more with
/// `--allow UniqueID:Label`. Loading a library runs its code, so nothing
/// outside this list is opened. CAPS Plate is the target #97 selected.
pub const ALLOWED: &[&str] = &["1779:Plate"];

/// How a render's channels reach the plugin's audio inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputMap {
    Identity,
    /// A mono render feeds both plugin inputs.
    Duplicate,
    /// A stereo render is averaged into the plugin's one input.
    Average,
}

struct Session {
    instance: Instance,
    channels: usize,
    inputs: usize,
    map: InputMap,
}

fn sha256_file(path: &Path) -> Result<String, LadspaError> {
    let read_error = |error: io::Error| LadspaError::Dlopen(format!("{}: {error}", path.display()));
    let mut file = File::open(path).map_err(read_error)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(read_error)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn parse_id(id: &str) -> Result<(u64, &str), LadspaError> {
    id.split_once(':')
        .and_then(|(unique_id, label)| {
            let digits = !unique_id.is_empty() && unique_id.bytes().all(|b| b.is_ascii_digit());
            let unique_id = unique_id.parse().ok().filter(|_| digits)?;
            (!label.is_empty()).then_some((unique_id, label))
        })
        .ok_or_else(|| LadspaError::UnknownPlugin(format!("{id:?} is not a LADSPA UniqueID:Label")))
}

/// The directories searched for a bare library name: `LADSPA_PATH`
/// (colon separated) or, when it is unset, the user and system LADSPA
/// folders macOS hosts such as Audacity use.
fn search_dirs() -> Vec<PathBuf> {
    match std::env::var_os("LADSPA_PATH") {
        Some(value) => std::env::split_paths(&value)
            .filter(|dir| !dir.as_os_str().is_empty())
            .collect(),
        None => {
            let mut dirs = Vec::new();
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(Path::new(&home).join("Library/Audio/Plug-Ins/LADSPA"));
            }
            dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/LADSPA"));
            dirs
        }
    }
}

/// A location containing `/` is a path. A bare file name such as `caps.so`
/// is looked up in [`search_dirs`].
fn resolve(location: &str) -> Result<PathBuf, LadspaError> {
    if location.contains('/') {
        let path = PathBuf::from(location);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(LadspaError::Missing(format!("no library at {location}")))
        };
    }
    let dirs = search_dirs();
    dirs.iter()
        .map(|dir| dir.join(location))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            let searched: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
            LadspaError::Missing(format!("{location} is not in [{}]", searched.join(", ")))
        })
}

fn open(
    open: &Open,
    state: Option<&[u8]>,
    allowed: &[String],
) -> Result<(Session, PluginInfo), LadspaError> {
    if open.protocol != protocol::VERSION {
        return Err(LadspaError::Unsupported(format!(
            "protocol {} (this helper speaks {})",
            open.protocol,
            protocol::VERSION
        )));
    }
    if open.backend != "ladspa" {
        return Err(LadspaError::Unsupported(format!(
            "backend {}",
            open.backend
        )));
    }
    let channels = usize::from(open.channels);
    if !(1..=2).contains(&channels) || open.max_block_frames == 0 || open.rate == 0 {
        return Err(LadspaError::Unsupported(
            "channels, rate or block size".into(),
        ));
    }
    let (unique_id, label) = parse_id(&open.plugin_id)?;
    if !allowed.contains(&open.plugin_id) {
        return Err(LadspaError::Unsupported(format!(
            "{} is not on the LADSPA allow-list ({})",
            open.plugin_id,
            allowed.join(", ")
        )));
    }
    if state.is_some() {
        return Err(LadspaError::Unsupported(
            "LADSPA plugins have no state to restore".into(),
        ));
    }
    let path = resolve(&open.location)?;
    let artifact = sha256_file(&path)?;
    let library = Arc::new(Library::open(&path)?);
    let plugin = library.find(unique_id, label)?;
    if plugin.properties & PROPERTY_REALTIME != 0 {
        return Err(LadspaError::Unsupported(format!(
            "{} declares LADSPA_PROPERTY_REALTIME, so an offline render would not be reproducible",
            plugin.name
        )));
    }
    let mut controls: Vec<(usize, &str, Control, f64)> = Vec::new();
    for (index, port) in plugin.ports.iter().enumerate() {
        if port.kind == Kind::Control && port.direction == Direction::Input {
            let control = port.control(open.rate)?;
            controls.push((index, &port.name, control, control.default));
        }
    }
    let inputs = plugin.audio(Direction::Input).len();
    let outputs = plugin.audio(Direction::Output).len();
    let map = match (inputs, channels) {
        (i, c) if i == c => InputMap::Identity,
        (2, 1) => InputMap::Duplicate,
        (1, 2) => InputMap::Average,
        _ => InputMap::Identity,
    };
    if !(1..=2).contains(&inputs) || outputs != channels {
        return Err(LadspaError::Unsupported(format!(
            "{} has {inputs} audio inputs and {outputs} outputs; a {channels}-channel render needs 1 or 2 inputs and exactly {channels} outputs",
            plugin.name
        )));
    }

    for param in &open.params {
        let Some((_, name, control, value)) = controls
            .iter_mut()
            .find(|(index, ..)| *index as u64 == u64::from(param.id))
        else {
            return Err(LadspaError::ParamRejected(format!(
                "port {} is not a control input",
                param.id
            )));
        };
        if !control.admits(param.value) {
            let (lower, upper) = control.range();
            return Err(LadspaError::ParamRejected(format!(
                "{name} ({}) takes {lower}..={upper}{}, not {}",
                param.id,
                if control.toggled {
                    " (0 or 1)"
                } else if control.integer {
                    " (whole numbers)"
                } else {
                    ""
                },
                param.value
            )));
        }
        *value = param.value;
    }
    let values: Vec<(usize, f32)> = controls
        .iter()
        .map(|(index, _, _, value)| (*index, *value as f32))
        .collect();
    let params = controls
        .iter()
        .map(|(index, name, control, _)| {
            let (min, max) = control.range();
            ParamInfo {
                id: *index as u32,
                name: (*name).to_owned(),
                min,
                max,
                default: control.default,
                stepped: control.toggled || control.integer,
            }
        })
        .collect();
    let max_block = open.max_block_frames as usize;
    let instance = plugin.instantiate(open.rate, max_block, &values)?;
    let info = PluginInfo {
        protocol: protocol::VERSION,
        plugin: Plugin {
            backend: "ladspa",
            id: open.plugin_id.clone(),
            artifact,
            abi: ABI.into(),
            version: VERSION.into(),
        },
        name: plugin.name.clone(),
        // The LADSPA 1.1 base API declares neither latency nor tail. The
        // renderer measures the tail.
        latency_frames: 0,
        tail_frames: None,
        params,
    };
    Ok((
        Session {
            instance,
            channels,
            inputs,
            map,
        },
        info,
    ))
}

impl Session {
    fn process(&mut self, frames: u32, samples: &[f32]) -> Result<Vec<f32>, LadspaError> {
        let count = frames as usize;
        let channels = self.channels;
        if samples.len() != count * channels {
            return Err(LadspaError::Unsupported(format!(
                "a block of {count} frames with {} samples",
                samples.len()
            )));
        }
        for input in 0..self.inputs {
            let buffer = self.instance.input(input);
            if count > buffer.len() {
                return Err(LadspaError::Unsupported(format!(
                    "a block of {count} frames (at most {})",
                    buffer.len()
                )));
            }
            for (frame, value) in buffer[..count].iter_mut().enumerate() {
                let at = frame * channels;
                *value = match self.map {
                    InputMap::Identity => samples[at + input],
                    InputMap::Duplicate => samples[at],
                    InputMap::Average => 0.5 * (samples[at] + samples[at + 1]),
                };
            }
        }
        self.instance.run(count)?;
        let mut out = vec![0.0; count * channels];
        for channel in 0..channels {
            let buffer = self.instance.output(channel);
            for (frame, value) in buffer[..count].iter().enumerate() {
                out[frame * channels + channel] = *value;
            }
        }
        Ok(out)
    }
}

fn fail(out: &mut impl Write, error: &LadspaError) -> u8 {
    let _ = protocol::write(
        out,
        FromHelper::Error {
            code: error.code(),
            message: &error.to_string(),
        },
    );
    3
}

/// Serve one session for the plugins in `allowed`. Returns the process exit
/// code.
pub fn serve(input: impl Read, output: impl Write, allowed: &[String]) -> u8 {
    let mut input = BufReader::new(input);
    let mut out = BufWriter::new(output);
    let protocol_error = |error: io::Error| {
        eprintln!("brownnote-plugin-host: {error}");
        2
    };
    let (open_message, state) = match protocol::read(&mut input) {
        Ok(Some(ToHelper::Open(open, state))) => (open, state),
        Ok(None) => return 0,
        Ok(Some(other)) => {
            eprintln!("brownnote-plugin-host: expected open, got {other:?}");
            return 2;
        }
        Err(error) => return protocol_error(error),
    };
    let (mut session, info) = match open(&open_message, state.as_deref(), allowed) {
        Ok(opened) => opened,
        Err(error) => return fail(&mut out, &error),
    };
    if let Err(error) = protocol::write(&mut out, FromHelper::Ready(&info)) {
        return protocol_error(error);
    }
    loop {
        match protocol::read(&mut input) {
            Ok(Some(ToHelper::Process { frames, samples })) => {
                let block = match session.process(frames, &samples) {
                    Ok(block) => block,
                    Err(error) => return fail(&mut out, &error),
                };
                if let Err(error) = protocol::write(&mut out, FromHelper::Block(&block)) {
                    return protocol_error(error);
                }
            }
            Ok(Some(ToHelper::End)) => {
                // Deactivate and clean up before reporting completion.
                drop(session);
                return match protocol::write(&mut out, FromHelper::Done(None)) {
                    Ok(()) => 0,
                    Err(error) => protocol_error(error),
                };
            }
            // The renderer went away; nothing is waiting for a reply.
            Ok(None) => return 0,
            Ok(Some(other)) => {
                eprintln!("brownnote-plugin-host: unexpected {other:?}");
                return 2;
            }
            Err(error) => return protocol_error(error),
        }
    }
}
