//! The helper side of the offline render protocol. This mirrors
//! `native/src/pedals/offline/protocol.rs`; the native crate's
//! `clap_offline` tests drive this binary through that implementation.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const VERSION: u32 = 1;
const MAX_MESSAGE: u32 = 32 << 20;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub id: u32,
    pub value: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub protocol: u32,
    pub backend: String,
    pub plugin_id: String,
    pub location: String,
    pub rate: u32,
    pub channels: u16,
    pub max_block_frames: u32,
    pub params: Vec<Param>,
    pub state: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plugin {
    pub backend: &'static str,
    pub id: String,
    pub artifact: String,
    pub abi: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub stepped: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginInfo {
    pub protocol: u32,
    pub plugin: Plugin,
    pub name: String,
    pub latency_frames: u32,
    pub tail_frames: Option<u32>,
    pub params: Vec<ParamInfo>,
}

#[derive(Debug)]
pub enum ToHelper {
    Open(Open, Option<Vec<u8>>),
    Process { frames: u32, samples: Vec<f32> },
    End,
}

pub enum FromHelper<'a> {
    Ready(&'a PluginInfo),
    Block(&'a [f32]),
    Done(Option<&'a [u8]>),
    Error { code: &'a str, message: &'a str },
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn read(input: &mut impl Read) -> io::Result<Option<ToHelper>> {
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
            let (len, rest) = payload
                .split_first_chunk::<4>()
                .ok_or_else(|| invalid("short open message"))?;
            let len = u32::from_le_bytes(*len) as usize;
            if len > rest.len() {
                return Err(invalid("open header overruns the message"));
            }
            let (header, state) = rest.split_at(len);
            let open: Open =
                serde_json::from_slice(header).map_err(|error| invalid(error.to_string()))?;
            let state = open.state.then(|| state.to_vec());
            ToHelper::Open(open, state)
        }
        b'P' => {
            let (frames, rest) = payload
                .split_first_chunk::<4>()
                .ok_or_else(|| invalid("short process message"))?;
            let (samples, remainder) = rest.as_chunks::<4>();
            if !remainder.is_empty() {
                return Err(invalid("audio is not whole f32 samples"));
            }
            ToHelper::Process {
                frames: u32::from_le_bytes(*frames),
                samples: samples.iter().map(|b| f32::from_le_bytes(*b)).collect(),
            }
        }
        b'E' => ToHelper::End,
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

pub fn write(out: &mut impl Write, message: FromHelper) -> io::Result<()> {
    match message {
        FromHelper::Ready(info) => frame(out, b'R', &to_json(info)?),
        FromHelper::Block(samples) => {
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            frame(out, b'B', &bytes)
        }
        FromHelper::Done(state) => {
            let mut payload = vec![u8::from(state.is_some())];
            payload.extend_from_slice(state.unwrap_or_default());
            frame(out, b'D', &payload)
        }
        FromHelper::Error { code, message } => {
            #[derive(Serialize)]
            struct Error<'a> {
                code: &'a str,
                message: &'a str,
            }
            frame(out, b'X', &to_json(&Error { code, message })?)
        }
    }
}

fn to_json(value: &impl Serialize) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|error| invalid(error.to_string()))
}
