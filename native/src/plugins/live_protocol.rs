//! The app side of the live instrument helper protocol (`LIVE_PROTOCOL.md`).
//!
//! Only the live worker thread uses this module. It owns the helper process,
//! its pipes and every wait; the audio callback never touches it.
use crate::pedals::offline::{
    HelperCommand,
    protocol::{HelperError, MAX_MESSAGE, Open, PluginInfo},
};
use std::{
    io::{self, BufReader, Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

pub const VERSION: u32 = 1;
pub const EVENT_BYTES: usize = 24;
pub const MAX_EVENTS: usize = 1024;
const STDERR_BYTES: usize = 16 * 1024;
/// How long a helper whose output closed gets to report its exit status.
const EXIT_GRACE: Duration = Duration::from_secs(1);

const KIND_NOTE_ON: u8 = 0;
const KIND_NOTE_OFF: u8 = 1;
const KIND_PARAM: u8 = 2;

/// One timed event inside a helper block. `time` is the frame offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WireEvent {
    NoteOn {
        time: u32,
        note_id: i32,
        channel: u8,
        key: u8,
        velocity: f64,
    },
    NoteOff {
        time: u32,
        note_id: i32,
        channel: u8,
        key: u8,
    },
    Param {
        time: u32,
        id: u32,
        value: f64,
    },
}

/// A note the plugin reported as finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEnd {
    pub time: u32,
    pub note_id: i32,
    pub channel: i16,
    pub key: i16,
}

#[derive(Debug)]
enum Reply {
    Ready(PluginInfo),
    Block {
        frames: u32,
        samples: Vec<f32>,
        ends: Vec<NoteEnd>,
    },
    State(Option<Vec<u8>>),
    Done(Option<Vec<u8>>),
    Error(HelperError),
}

/// Why the helper process could not answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperFault {
    Spawn(String),
    /// No reply within the deadline. The helper was killed.
    Timeout,
    /// The helper exited (or closed its output) without the expected reply.
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
        stderr: String,
    },
    Protocol(String),
}

impl std::fmt::Display for HelperFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(message) => write!(f, "could not start the helper: {message}"),
            Self::Timeout => f.write_str("the helper missed its deadline and was killed"),
            Self::Exited {
                code,
                signal,
                stderr,
            } => write!(
                f,
                "the helper exited (code {code:?}, signal {signal:?}): {}",
                stderr.trim()
            ),
            Self::Protocol(message) => write!(f, "helper protocol error: {message}"),
        }
    }
}

/// A reply the helper sent, or the typed error it reported instead.
pub type Answer<T> = Result<Result<T, HelperError>, HelperFault>;

#[cfg(unix)]
fn signal_of(status: ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}
#[cfg(not(unix))]
fn signal_of(_status: ExitStatus) -> Option<i32> {
    None
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn frame(tag: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|len| *len <= MAX_MESSAGE)
        .ok_or_else(|| invalid("message too large"))?;
    let mut bytes = Vec::with_capacity(5 + payload.len());
    bytes.push(tag);
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

pub fn encode_open(open: &Open, state: Option<&[u8]>) -> io::Result<Vec<u8>> {
    let header = serde_json::to_vec(open).map_err(|error| invalid(error.to_string()))?;
    let header_len = u32::try_from(header.len()).map_err(|_| invalid("open header too large"))?;
    let mut payload = Vec::with_capacity(4 + header.len() + state.map_or(0, <[u8]>::len));
    payload.extend_from_slice(&header_len.to_le_bytes());
    payload.extend_from_slice(&header);
    payload.extend_from_slice(state.unwrap_or_default());
    frame(b'O', &payload)
}

pub fn encode_process(frames: u32, events: &[WireEvent]) -> io::Result<Vec<u8>> {
    if events.len() > MAX_EVENTS {
        return Err(invalid("too many events in one block"));
    }
    let mut payload = Vec::with_capacity(4 + events.len() * EVENT_BYTES);
    payload.extend_from_slice(&frames.to_le_bytes());
    for event in events {
        let (time, kind, channel, key, id, value) = match *event {
            WireEvent::NoteOn {
                time,
                note_id,
                channel,
                key,
                velocity,
            } => (time, KIND_NOTE_ON, channel, key, note_id as u32, velocity),
            WireEvent::NoteOff {
                time,
                note_id,
                channel,
                key,
            } => (time, KIND_NOTE_OFF, channel, key, note_id as u32, 0.0),
            WireEvent::Param { time, id, value } => (time, KIND_PARAM, 0, 0, id, value),
        };
        payload.extend_from_slice(&time.to_le_bytes());
        payload.extend_from_slice(&[kind, 0]);
        payload.extend_from_slice(&i16::from(channel).to_le_bytes());
        payload.extend_from_slice(&i16::from(key).to_le_bytes());
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(&id.to_le_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
    }
    frame(b'P', &payload)
}

fn read_reply(input: &mut impl Read) -> io::Result<Option<Reply>> {
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
    let state = |payload: &[u8]| -> io::Result<Option<Vec<u8>>> {
        match payload.split_first() {
            Some((0, [])) => Ok(None),
            Some((1, rest)) => Ok(Some(rest.to_vec())),
            _ => Err(invalid("malformed state reply")),
        }
    };
    Ok(Some(match tag[0] {
        b'R' => Reply::Ready(
            serde_json::from_slice(&payload).map_err(|error| invalid(error.to_string()))?,
        ),
        b'L' => {
            let (head, rest) = payload
                .split_first_chunk::<8>()
                .ok_or_else(|| invalid("short block"))?;
            let frames = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
            let ends = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
            let end_bytes = ends
                .checked_mul(12)
                .filter(|bytes| *bytes <= rest.len())
                .ok_or_else(|| invalid("note ends overrun the block"))?;
            let (samples, end_data) = rest.split_at(rest.len() - end_bytes);
            let (samples, remainder) = samples.as_chunks::<4>();
            if !remainder.is_empty() {
                return Err(invalid("audio is not whole f32 samples"));
            }
            Reply::Block {
                frames,
                samples: samples.iter().map(|b| f32::from_le_bytes(*b)).collect(),
                ends: end_data
                    .as_chunks::<12>()
                    .0
                    .iter()
                    .map(|e| NoteEnd {
                        time: u32::from_le_bytes([e[0], e[1], e[2], e[3]]),
                        note_id: i32::from_le_bytes([e[4], e[5], e[6], e[7]]),
                        channel: i16::from_le_bytes([e[8], e[9]]),
                        key: i16::from_le_bytes([e[10], e[11]]),
                    })
                    .collect(),
            }
        }
        b'T' => Reply::State(state(&payload)?),
        b'D' => Reply::Done(state(&payload)?),
        b'X' => Reply::Error(
            serde_json::from_slice(&payload).map_err(|error| invalid(error.to_string()))?,
        ),
        other => return Err(invalid(format!("unknown reply tag {other:#04x}"))),
    }))
}

/// One running helper process. Writes go through their own thread and replies
/// arrive through a reader thread, so every wait here has a deadline.
pub struct Helper {
    child: Child,
    requests: Option<mpsc::Sender<Vec<u8>>>,
    replies: mpsc::Receiver<io::Result<Reply>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    channels: usize,
}

impl Helper {
    pub fn spawn(command: &HelperCommand, channels: usize) -> Result<Self, HelperFault> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                HelperFault::Spawn(format!("{}: {error}", command.program.display()))
            })?;
        let (Some(mut stdin), Some(stdout), Some(mut stderr_pipe)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(HelperFault::Spawn("the helper's pipes are missing".into()));
        };
        let (requests, outgoing) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("live-helper-write".into())
            .spawn(move || {
                for bytes in outgoing {
                    if stdin
                        .write_all(&bytes)
                        .and_then(|()| stdin.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| HelperFault::Spawn(error.to_string()))?;
        let (send, replies) = mpsc::channel();
        thread::Builder::new()
            .name("live-helper-read".into())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let reply = read_reply(&mut stdout);
                    let stop = !matches!(reply, Ok(Some(_)));
                    let message = match reply {
                        Ok(Some(reply)) => Ok(reply),
                        Ok(None) => Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                        Err(error) => Err(error),
                    };
                    if send.send(message).is_err() || stop {
                        break;
                    }
                }
            })
            .map_err(|error| HelperFault::Spawn(error.to_string()))?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = stderr.clone();
        thread::Builder::new()
            .name("live-helper-stderr".into())
            .spawn(move || {
                let mut chunk = [0; 1024];
                while let Ok(count) = stderr_pipe.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    if let Ok(mut kept) = sink.lock() {
                        let room = STDERR_BYTES.saturating_sub(kept.len());
                        kept.extend_from_slice(&chunk[..count.min(room)]);
                    }
                }
            })
            .map_err(|error| HelperFault::Spawn(error.to_string()))?;
        Ok(Self {
            child,
            requests: Some(requests),
            replies,
            stderr,
            channels,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn send(&mut self, bytes: io::Result<Vec<u8>>) -> Result<(), HelperFault> {
        let bytes = bytes.map_err(|error| HelperFault::Protocol(error.to_string()))?;
        match &self.requests {
            Some(requests) if requests.send(bytes).is_ok() => Ok(()),
            _ => Err(self.exited()),
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Reply, HelperFault> {
        match self.replies.recv_timeout(timeout) {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
                self.kill();
                Err(HelperFault::Protocol(error.to_string()))
            }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.exited()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.kill();
                Err(HelperFault::Timeout)
            }
        }
    }

    fn exited(&mut self) -> HelperFault {
        let deadline = Instant::now() + EXIT_GRACE;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                _ => break None,
            }
        };
        if status.is_none() {
            self.kill();
        }
        // The stderr reader finishes once the process is gone.
        thread::sleep(Duration::from_millis(5));
        let stderr = self
            .stderr
            .lock()
            .map(|kept| String::from_utf8_lossy(&kept).into_owned())
            .unwrap_or_default();
        HelperFault::Exited {
            code: status.and_then(|s| s.code()),
            signal: status.and_then(signal_of),
            stderr,
        }
    }

    pub fn kill(&mut self) {
        self.requests = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn unexpected(&mut self, wanted: &str, got: &Reply) -> HelperFault {
        self.kill();
        let got = match got {
            Reply::Ready(_) => "ready",
            Reply::Block { .. } => "block",
            Reply::State(_) => "state",
            Reply::Done(_) => "done",
            Reply::Error(_) => "error",
        };
        HelperFault::Protocol(format!("expected {wanted}, got {got}"))
    }

    pub fn open(
        &mut self,
        open: &Open,
        state: Option<&[u8]>,
        timeout: Duration,
    ) -> Answer<PluginInfo> {
        self.send(encode_open(open, state))?;
        match self.recv(timeout)? {
            Reply::Ready(info) => Ok(Ok(info)),
            Reply::Error(error) => Ok(Err(error)),
            other => Err(self.unexpected("ready", &other)),
        }
    }

    /// One block: interleaved samples in the opened channel count, plus the
    /// notes the plugin ended during it.
    pub fn process(
        &mut self,
        frames: u32,
        events: &[WireEvent],
        timeout: Duration,
    ) -> Answer<(Vec<f32>, Vec<NoteEnd>)> {
        self.send(encode_process(frames, events))?;
        match self.recv(timeout)? {
            Reply::Block {
                frames: got,
                samples,
                ends,
            } => {
                if got != frames || samples.len() != frames as usize * self.channels {
                    self.kill();
                    return Err(HelperFault::Protocol(format!(
                        "asked for {frames} frames, got {got} frames and {} samples",
                        samples.len()
                    )));
                }
                Ok(Ok((samples, ends)))
            }
            Reply::Error(error) => Ok(Err(error)),
            other => Err(self.unexpected("block", &other)),
        }
    }

    pub fn save_state(&mut self, timeout: Duration) -> Answer<Option<Vec<u8>>> {
        self.send(frame(b'S', &[]))?;
        match self.recv(timeout)? {
            Reply::State(state) => Ok(Ok(state)),
            Reply::Error(error) => Ok(Err(error)),
            other => Err(self.unexpected("state", &other)),
        }
    }

    pub fn end(&mut self, timeout: Duration) -> Answer<Option<Vec<u8>>> {
        self.send(frame(b'E', &[]))?;
        let answer = match self.recv(timeout)? {
            Reply::Done(state) => Ok(Ok(state)),
            Reply::Error(error) => Ok(Err(error)),
            other => Err(self.unexpected("done", &other)),
        };
        self.kill();
        answer
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.kill();
    }
}
