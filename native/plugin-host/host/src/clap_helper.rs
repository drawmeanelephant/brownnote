//! The CLAP side of the offline render protocol: one plugin instance, opened
//! from an `O` message, fed blocks until `E`.
use crate::clap::{self, HostError, Instance, Library};
use crate::protocol::{self, FromHelper, Open, ParamInfo, Plugin, PluginInfo, ToHelper};
use clap_sys::ext::params::CLAP_PARAM_IS_STEPPED;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::Path,
};

struct Session {
    // Declared before `_library` so the instance is destroyed first.
    instance: Instance,
    _library: Library,
    channels: usize,
    max_block: usize,
    /// `[port][channel][frame]`
    input: Vec<Vec<Vec<f32>>>,
    output: Vec<Vec<Vec<f32>>>,
}

fn sha256_file(path: &Path) -> Result<String, HostError> {
    let mut file = File::open(path).map_err(|e| HostError::Dlopen(e.to_string()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| HostError::Dlopen(e.to_string()))?;
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

fn open(open: &Open, state: Option<&[u8]>) -> Result<(Session, PluginInfo), HostError> {
    if open.protocol != protocol::VERSION {
        return Err(HostError::Unsupported(format!(
            "protocol {} (this helper speaks {})",
            open.protocol,
            protocol::VERSION
        )));
    }
    if open.backend != "clap" {
        return Err(HostError::Unsupported(format!("backend {}", open.backend)));
    }
    let channels = usize::from(open.channels);
    if !(1..=2).contains(&channels) || open.max_block_frames == 0 || open.rate == 0 {
        return Err(HostError::Unsupported(
            "channels, rate or block size".into(),
        ));
    }
    let bundle = Path::new(&open.location);
    let executable = clap::executable_path(bundle)?;
    let artifact = sha256_file(&executable)?;
    let library = Library::open(bundle, &executable)?;
    let descriptor = library
        .descriptors()
        .into_iter()
        .find(|d| d.id == open.plugin_id)
        .ok_or_else(|| HostError::UnknownPlugin(open.plugin_id.clone()))?;
    let mut instance = library.instantiate(&open.plugin_id)?;

    let inputs = instance.audio_ports(true);
    let outputs = instance.audio_ports(false);
    let main = |ports: &[u32]| ports.first().map(|c| *c as usize);
    if main(&inputs) != Some(channels) || main(&outputs) != Some(channels) {
        return Err(HostError::Unsupported(format!(
            "the plugin's main ports are {inputs:?} in and {outputs:?} out, the render needs {channels} channels"
        )));
    }
    if instance.hard_realtime() == Some(true) {
        return Err(HostError::Unsupported(
            "the plugin requires hard realtime processing".into(),
        ));
    }
    if instance.set_offline() == Some(false) {
        return Err(HostError::Unsupported(
            "the plugin refused offline render mode".into(),
        ));
    }
    if let Some(state) = state {
        instance.load_state(state)?;
    }
    let infos = instance.params();
    if !open.params.is_empty() {
        let mut values = Vec::with_capacity(open.params.len());
        for param in &open.params {
            let info = infos.iter().find(|i| i.id == param.id).ok_or_else(|| {
                HostError::ParamRejected(format!("no parameter with ID {}", param.id))
            })?;
            if !param.value.is_finite() || param.value < info.min || param.value > info.max {
                return Err(HostError::ParamRejected(format!(
                    "{} ({}) takes {}..={}, not {}",
                    info.name, info.id, info.min, info.max, param.value
                )));
            }
            values.push((param.id, param.value));
        }
        instance.flush_params(&values)?;
    }
    instance.activate(f64::from(open.rate), open.max_block_frames)?;
    let latency = instance.latency().unwrap_or(0);
    let tail = instance.tail();
    instance.start_processing()?;

    let max_block = open.max_block_frames as usize;
    let buffers = |ports: &[u32]| -> Vec<Vec<Vec<f32>>> {
        ports
            .iter()
            .map(|c| vec![vec![0.0; max_block]; *c as usize])
            .collect()
    };
    let (major, minor, revision) = library.clap_version;
    let version = if descriptor.version.trim().is_empty() {
        "unversioned".into()
    } else {
        descriptor.version
    };
    let info = PluginInfo {
        protocol: protocol::VERSION,
        plugin: Plugin {
            backend: "clap",
            id: descriptor.id,
            artifact,
            abi: format!("clap {major}.{minor}.{revision}"),
            version,
        },
        name: descriptor.name,
        latency_frames: latency,
        tail_frames: tail,
        params: infos
            .into_iter()
            .map(|i| ParamInfo {
                id: i.id,
                name: i.name,
                min: i.min,
                max: i.max,
                default: i.default,
                stepped: i.flags & CLAP_PARAM_IS_STEPPED != 0,
            })
            .collect(),
    };
    Ok((
        Session {
            input: buffers(&inputs),
            output: buffers(&outputs),
            instance,
            _library: library,
            channels,
            max_block,
        },
        info,
    ))
}

impl Session {
    fn process(&mut self, frames: u32, samples: &[f32]) -> Result<Vec<f32>, HostError> {
        let count = frames as usize;
        if count == 0 || count > self.max_block || samples.len() != count * self.channels {
            return Err(HostError::Unsupported(format!(
                "a block of {count} frames with {} samples",
                samples.len()
            )));
        }
        for (channel, buffer) in self.input[0].iter_mut().enumerate() {
            for (frame, value) in buffer[..count].iter_mut().enumerate() {
                *value = samples[frame * self.channels + channel];
            }
        }
        self.instance
            .process(frames, &mut self.input, &mut self.output)?;
        let main = &self.output[0];
        let mut out = Vec::with_capacity(samples.len());
        for frame in 0..count {
            for channel in main {
                out.push(channel[frame]);
            }
        }
        Ok(out)
    }

    fn finish(mut self) -> Result<Option<Vec<u8>>, HostError> {
        self.instance.stop_processing();
        let state = self.instance.save_state();
        self.instance.deactivate();
        state
    }
}

fn fail(out: &mut impl Write, error: &HostError) -> u8 {
    let _ = protocol::write(
        out,
        FromHelper::Error {
            code: error.code(),
            message: &error.to_string(),
        },
    );
    3
}

/// Serve one session. Returns the process exit code.
pub fn serve(input: impl Read, output: impl Write) -> u8 {
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
    let (mut session, info) = match open(&open_message, state.as_deref()) {
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
                let state = match session.finish() {
                    Ok(state) => state,
                    Err(error) => return fail(&mut out, &error),
                };
                return match protocol::write(&mut out, FromHelper::Done(state.as_deref())) {
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
