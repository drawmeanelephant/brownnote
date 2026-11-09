//! The wire format between the renderer and a plugin helper process.
//! `PROTOCOL.md` beside this file is the specification helpers implement.
use super::{Backend, Param, Plugin};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const VERSION: u32 = 1;
/// No single message may be larger than this.
pub const MAX_MESSAGE: u32 = 32 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub protocol: u32,
    pub backend: Backend,
    pub plugin_id: String,
    pub location: String,
    pub rate: u32,
    pub channels: u16,
    pub max_block_frames: u32,
    pub params: Vec<Param>,
    /// The state bytes follow the JSON in the same message.
    pub state: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub stepped: bool,
}

/// What a helper reports once the plugin is loaded, configured and active.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInfo {
    pub protocol: u32,
    pub plugin: Plugin,
    pub name: String,
    pub latency_frames: u32,
    /// `None` for no declared tail, `Some(u32::MAX)` for infinite.
    pub tail_frames: Option<u32>,
    pub params: Vec<ParamInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToHelper {
    Open(Open, Option<Vec<u8>>),
    Process { frames: u32, samples: Vec<f32> },
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FromHelper {
    Ready(PluginInfo),
    Block(Vec<f32>),
    Done(Option<Vec<u8>>),
    Error(HelperError),
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn write_frame(out: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|len| *len <= MAX_MESSAGE)
        .ok_or_else(|| invalid("message too large"))?;
    out.write_all(&[tag])?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(payload)
}

/// `Ok(None)` at a clean end of stream between messages.
fn read_frame(input: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut tag = [0];
    if input.read(&mut tag)? == 0 {
        return Ok(None);
    }
    let mut len = [0; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_MESSAGE {
        return Err(invalid(format!("message of {len} bytes is too large")));
    }
    let mut payload = vec![0; len as usize];
    input.read_exact(&mut payload)?;
    Ok(Some((tag[0], payload)))
}

fn floats(bytes: &[u8]) -> io::Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(invalid("audio is not whole f32 samples"));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

fn float_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> io::Result<T> {
    serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))
}

pub fn write_to_helper(out: &mut impl Write, message: &ToHelper) -> io::Result<()> {
    match message {
        ToHelper::Open(open, state) => {
            let header = to_json(open)?;
            let header_len =
                u32::try_from(header.len()).map_err(|_| invalid("header too large"))?;
            let mut payload = header_len.to_le_bytes().to_vec();
            payload.extend_from_slice(&header);
            if let Some(state) = state {
                payload.extend_from_slice(state);
            }
            write_frame(out, b'O', &payload)
        }
        ToHelper::Process { frames, samples } => {
            let mut payload = frames.to_le_bytes().to_vec();
            payload.extend_from_slice(&float_bytes(samples));
            write_frame(out, b'P', &payload)
        }
        ToHelper::End => write_frame(out, b'E', &[]),
    }
}

pub fn read_from_helper(input: &mut impl Read) -> io::Result<Option<FromHelper>> {
    let Some((tag, payload)) = read_frame(input)? else {
        return Ok(None);
    };
    Ok(Some(match tag {
        b'R' => FromHelper::Ready(json(&payload)?),
        b'B' => FromHelper::Block(floats(&payload)?),
        b'D' => match payload.split_first() {
            Some((0, [])) => FromHelper::Done(None),
            Some((1, state)) => FromHelper::Done(Some(state.to_vec())),
            _ => return Err(invalid("malformed done message")),
        },
        b'X' => FromHelper::Error(json(&payload)?),
        other => return Err(invalid(format!("unknown message tag {other:#04x}"))),
    }))
}

/// The helper side, for helpers and tests written in Rust.
pub fn read_to_helper(input: &mut impl Read) -> io::Result<Option<ToHelper>> {
    let Some((tag, payload)) = read_frame(input)? else {
        return Ok(None);
    };
    Ok(Some(match tag {
        b'O' => {
            let (len, rest) = payload
                .split_first_chunk::<4>()
                .ok_or_else(|| invalid("short open message"))?;
            let len = u32::from_le_bytes(*len) as usize;
            if len > rest.len() {
                return Err(invalid("open header overruns the message"));
            }
            let (header, state) = rest.split_at(len);
            let open: Open = json(header)?;
            let state = open.state.then(|| state.to_vec());
            ToHelper::Open(open, state)
        }
        b'P' => {
            let (frames, rest) = payload
                .split_first_chunk::<4>()
                .ok_or_else(|| invalid("short process message"))?;
            ToHelper::Process {
                frames: u32::from_le_bytes(*frames),
                samples: floats(rest)?,
            }
        }
        b'E' => ToHelper::End,
        other => return Err(invalid(format!("unknown message tag {other:#04x}"))),
    }))
}

pub fn write_from_helper(out: &mut impl Write, message: &FromHelper) -> io::Result<()> {
    match message {
        FromHelper::Ready(info) => write_frame(out, b'R', &to_json(info)?),
        FromHelper::Block(samples) => write_frame(out, b'B', &float_bytes(samples)),
        FromHelper::Done(state) => {
            let mut payload = vec![u8::from(state.is_some())];
            if let Some(state) = state {
                payload.extend_from_slice(state);
            }
            write_frame(out, b'D', &payload)
        }
        FromHelper::Error(error) => write_frame(out, b'X', &to_json(error)?),
    }
}

fn to_json(value: &impl Serialize) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|error| invalid(error.to_string()))
}
