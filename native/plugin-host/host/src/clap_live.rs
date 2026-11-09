//! The helper side of the live instrument protocol
//! (`native/src/plugins/LIVE_PROTOCOL.md`): one CLAP instrument per process,
//! opened from an `O` message, then fed one block of timed note and parameter
//! events per `P` message until `E`.
//!
//! This process is never the app's audio callback. The app's live worker
//! thread talks to it; the callback only reads blocks the worker has already
//! received. The helper's own thread is an ordinary-priority thread.
use crate::clap::{self, HostError, InputEvent, Instance, Library, NoteEnd};
use crate::protocol::{self, FromHelper, Open, ParamInfo, Plugin, PluginInfo};
use clap_sys::ext::note_ports::CLAP_NOTE_DIALECT_CLAP;
use clap_sys::ext::params::CLAP_PARAM_IS_STEPPED;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::Path,
};

/// The live protocol version, independent of the offline protocol's.
pub const VERSION: u32 = 1;
const MAX_MESSAGE: u32 = 32 << 20;
/// Bytes per encoded input event.
pub const EVENT_BYTES: usize = 24;
pub const MAX_EVENTS: usize = 1024;

const KIND_NOTE_ON: u8 = 0;
const KIND_NOTE_OFF: u8 = 1;
const KIND_PARAM: u8 = 2;

enum Message {
    Open(Open, Option<Vec<u8>>),
    Process {
        frames: u32,
        events: Vec<InputEvent>,
    },
    SaveState,
    End,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn le_u32(bytes: &[u8]) -> u32 {
    let mut b = [0; 4];
    b.copy_from_slice(&bytes[..4]);
    u32::from_le_bytes(b)
}

fn le_i16(bytes: &[u8]) -> i16 {
    i16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_f64(bytes: &[u8]) -> f64 {
    let mut b = [0; 8];
    b.copy_from_slice(&bytes[..8]);
    f64::from_le_bytes(b)
}

fn decode_event(bytes: &[u8]) -> io::Result<InputEvent> {
    let time = le_u32(&bytes[0..]);
    let kind = bytes[4];
    let channel = le_i16(&bytes[6..]);
    let key = le_i16(&bytes[8..]);
    let id = le_u32(&bytes[12..]);
    let value = le_f64(&bytes[16..]);
    Ok(match kind {
        KIND_NOTE_ON => InputEvent::NoteOn {
            time,
            note_id: id as i32,
            channel,
            key,
            velocity: value,
        },
        KIND_NOTE_OFF => InputEvent::NoteOff {
            time,
            note_id: id as i32,
            channel,
            key,
            velocity: value,
        },
        KIND_PARAM => InputEvent::Param { time, id, value },
        other => return Err(invalid(format!("unknown event kind {other}"))),
    })
}

fn read(input: &mut impl Read) -> io::Result<Option<Message>> {
    let mut tag = [0];
    if input.read(&mut tag)? == 0 {
        return Ok(None);
    }
    let mut len = [0; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_MESSAGE {
        return Err(invalid("message too large"));
    }
    let mut payload = vec![0; len as usize];
    input.read_exact(&mut payload)?;
    Ok(Some(match tag[0] {
        b'O' => {
            if payload.len() < 4 {
                return Err(invalid("short open message"));
            }
            let header = le_u32(&payload) as usize;
            let rest = &payload[4..];
            if header > rest.len() {
                return Err(invalid("open header overruns the message"));
            }
            let (header, state) = rest.split_at(header);
            let open: Open =
                serde_json::from_slice(header).map_err(|error| invalid(error.to_string()))?;
            let state = open.state.then(|| state.to_vec());
            Message::Open(open, state)
        }
        b'P' => {
            if payload.len() < 4 || !(payload.len() - 4).is_multiple_of(EVENT_BYTES) {
                return Err(invalid("process message is not whole events"));
            }
            let frames = le_u32(&payload);
            let events = payload[4..]
                .as_chunks::<EVENT_BYTES>()
                .0
                .iter()
                .map(|bytes| decode_event(bytes))
                .collect::<io::Result<Vec<_>>>()?;
            if events.len() > MAX_EVENTS {
                return Err(invalid("too many events in one block"));
            }
            Message::Process { frames, events }
        }
        b'S' => Message::SaveState,
        b'E' => Message::End,
        other => return Err(invalid(format!("unknown message tag {other:#04x}"))),
    }))
}

fn frame(out: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|len| *len <= MAX_MESSAGE)
        .ok_or_else(|| invalid("message too large"))?;
    out.write_all(&[tag])?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

fn write_block(
    out: &mut impl Write,
    frames: u32,
    samples: &[f32],
    ends: &[NoteEnd],
) -> io::Result<()> {
    let mut payload = Vec::with_capacity(8 + samples.len() * 4 + ends.len() * 12);
    payload.extend_from_slice(&frames.to_le_bytes());
    payload.extend_from_slice(&(ends.len() as u32).to_le_bytes());
    for sample in samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }
    for end in ends {
        payload.extend_from_slice(&end.time.to_le_bytes());
        payload.extend_from_slice(&end.note_id.to_le_bytes());
        payload.extend_from_slice(&end.channel.to_le_bytes());
        payload.extend_from_slice(&end.key.to_le_bytes());
    }
    frame(out, b'L', &payload)
}

fn write_state(out: &mut impl Write, tag: u8, state: Option<&[u8]>) -> io::Result<()> {
    let mut payload = vec![u8::from(state.is_some())];
    payload.extend_from_slice(state.unwrap_or_default());
    frame(out, tag, &payload)
}

struct Session {
    // Declared before `_library` so the instance is destroyed first.
    instance: Instance,
    _library: Library,
    channels: usize,
    max_block: usize,
    /// `[port][channel][frame]`
    input: Vec<Vec<Vec<f32>>>,
    output: Vec<Vec<Vec<f32>>>,
    ends: Vec<NoteEnd>,
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
    if open.protocol != VERSION {
        return Err(HostError::Unsupported(format!(
            "live protocol {} (this helper speaks {VERSION})",
            open.protocol
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

    if !instance
        .note_ports(true)
        .iter()
        .any(|(supported, _)| supported & CLAP_NOTE_DIALECT_CLAP != 0)
    {
        return Err(HostError::Unsupported(
            "the plugin has no note input port that accepts CLAP note events".into(),
        ));
    }
    let inputs = instance.audio_ports(true);
    let outputs = instance.audio_ports(false);
    match outputs.first().map(|c| *c as usize) {
        Some(main) if main == channels || main == 1 => {}
        _ => {
            return Err(HostError::Unsupported(format!(
                "the plugin's output ports are {outputs:?}, the host needs {channels} channels or mono"
            )));
        }
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
        protocol: VERSION,
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
            ends: Vec::with_capacity(64),
        },
        info,
    ))
}

impl Session {
    fn process(&mut self, frames: u32, events: &[InputEvent]) -> Result<Vec<f32>, HostError> {
        let count = frames as usize;
        if count == 0 || count > self.max_block {
            return Err(HostError::Unsupported(format!("a block of {count} frames")));
        }
        let mut last = 0;
        for event in events {
            let time = match *event {
                InputEvent::NoteOn { time, .. }
                | InputEvent::NoteOff { time, .. }
                | InputEvent::Param { time, .. } => time,
            };
            if time < last || time >= frames {
                return Err(HostError::Unsupported(format!(
                    "event at frame {time} in a block of {frames} after frame {last}"
                )));
            }
            last = time;
        }
        self.ends.clear();
        self.instance.process_events(
            frames,
            events,
            &mut self.ends,
            &mut self.input,
            &mut self.output,
        )?;
        // A mono instrument feeds every requested channel.
        let main = &self.output[0];
        let sources: Vec<&[f32]> = (0..self.channels)
            .map(|channel| &main[channel.min(main.len() - 1)][..count])
            .collect();
        let mut out = Vec::with_capacity(count * self.channels);
        for frame in 0..count {
            out.extend(sources.iter().map(|source| source[frame]));
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

/// Serve one live session. Returns the process exit code.
pub fn serve(input: impl Read, output: impl Write) -> u8 {
    let mut input = BufReader::new(input);
    let mut out = BufWriter::new(output);
    let protocol_error = |error: io::Error| {
        eprintln!("brownnote-clap-live: {error}");
        2
    };
    let (open_message, state) = match read(&mut input) {
        Ok(Some(Message::Open(open, state))) => (open, state),
        Ok(None) => return 0,
        Ok(Some(_)) => {
            eprintln!("brownnote-clap-live: expected open");
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
        match read(&mut input) {
            Ok(Some(Message::Process { frames, events })) => {
                let block = match session.process(frames, &events) {
                    Ok(block) => block,
                    Err(error) => return fail(&mut out, &error),
                };
                if let Err(error) = write_block(&mut out, frames, &block, &session.ends) {
                    return protocol_error(error);
                }
            }
            Ok(Some(Message::SaveState)) => {
                let state = match session.instance.save_state() {
                    Ok(state) => state,
                    Err(error) => return fail(&mut out, &error),
                };
                if let Err(error) = write_state(&mut out, b'T', state.as_deref()) {
                    return protocol_error(error);
                }
            }
            Ok(Some(Message::End)) => {
                let state = match session.finish() {
                    Ok(state) => state,
                    Err(error) => return fail(&mut out, &error),
                };
                return match write_state(&mut out, b'D', state.as_deref()) {
                    Ok(()) => 0,
                    Err(error) => protocol_error(error),
                };
            }
            // The worker went away; nothing is waiting for a reply.
            Ok(None) => return 0,
            Ok(Some(Message::Open(..))) => {
                eprintln!("brownnote-clap-live: unexpected second open");
                return 2;
            }
            Err(error) => return protocol_error(error),
        }
    }
}
