//! Runs each chain stage in its own helper process with bounded waits.
use super::{
    Backend, ErrorKind, Layout, MAX_LATENCY_SECONDS, Phase, RenderError, Rendered, Request, Source,
    Stage, StageReport,
    protocol::{self, FromHelper, Open, PluginInfo, ToHelper},
};
use std::{
    ffi::OsString,
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// How often a wait wakes to check for cancellation.
const POLL: Duration = Duration::from_millis(10);
const STDERR_BYTES: u64 = 16 * 1024;
/// How long a helper whose output closed gets to report its exit status.
const EXIT_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// The helper executable for each backend. Machine configuration, never
/// part of a render's identity.
#[derive(Clone, Debug, Default)]
pub struct Helpers {
    commands: Vec<(Backend, HelperCommand)>,
}

impl Helpers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, backend: Backend, command: HelperCommand) -> Self {
        self.commands.retain(|(b, _)| *b != backend);
        self.commands.push((backend, command));
        self
    }

    pub fn get(&self, backend: Backend) -> Option<&HelperCommand> {
        self.commands
            .iter()
            .find(|(b, _)| *b == backend)
            .map(|(_, command)| command)
    }
}

/// Upper bounds on each wait. None of them changes the audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Start the helper, load, configure and activate the plugin.
    pub open: Duration,
    /// Process one block.
    pub block: Duration,
    /// Save state and shut down.
    pub close: Duration,
    /// The whole render, every stage included.
    pub total: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            open: Duration::from_secs(10),
            block: Duration::from_secs(2),
            close: Duration::from_secs(5),
            total: Duration::from_secs(120),
        }
    }
}

/// Stops a render from another thread. The running helper is killed within
/// about one poll interval and the render returns [`ErrorKind::Cancelled`].
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

struct Clock<'a> {
    deadline: Instant,
    cancel: &'a Cancel,
}

/// One helper process. Dropping it kills the process, so no error path can
/// leave a plugin running.
struct Helper {
    child: Child,
    to: Option<mpsc::Sender<Vec<u8>>>,
    from: mpsc::Receiver<io::Result<FromHelper>>,
    stderr: Option<thread::JoinHandle<String>>,
}

impl Helper {
    fn spawn(command: &HelperCommand) -> Result<Self, ErrorKind> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| ErrorKind::Spawn(format!("{}: {error}", command.program.display())))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ErrorKind::Spawn(
                "the helper's pipes were not created".into(),
            ));
        };
        // Writes happen on their own thread so a helper that stops reading
        // cannot block the renderer past its deadline.
        let (to, outgoing) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || write_loop(stdin, &outgoing));
        let (incoming, from) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match protocol::read_from_helper(&mut reader) {
                    Ok(Some(message)) => {
                        if incoming.send(Ok(message)).is_err() {
                            return;
                        }
                    }
                    // A crash mid-message looks like a short read: report the exit, not the framing.
                    Ok(None) => return,
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return,
                    Err(error) => {
                        let _ = incoming.send(Err(error));
                        return;
                    }
                }
            }
        });
        let stderr = thread::spawn(move || {
            let mut text = String::new();
            let mut limited = stderr.take(STDERR_BYTES);
            let _ = limited.read_to_string(&mut text);
            let _ = io::copy(&mut limited.into_inner(), &mut io::sink());
            text
        });
        Ok(Self {
            child,
            to: Some(to),
            from,
            stderr: Some(stderr),
        })
    }

    fn send(&self, message: &ToHelper) -> Result<(), ErrorKind> {
        let mut bytes = Vec::new();
        protocol::write_to_helper(&mut bytes, message)
            .map_err(|error| ErrorKind::Protocol(error.to_string()))?;
        if let Some(to) = &self.to {
            // A failed send means the helper is gone; the next receive reports how.
            let _ = to.send(bytes);
        }
        Ok(())
    }

    fn recv(
        &mut self,
        phase: Phase,
        limit: Duration,
        clock: &Clock,
    ) -> Result<FromHelper, ErrorKind> {
        let phase_deadline = Instant::now() + limit;
        loop {
            if clock.cancel.is_cancelled() {
                return Err(ErrorKind::Cancelled);
            }
            let now = Instant::now();
            if now >= clock.deadline {
                return Err(ErrorKind::Timeout(Phase::Total));
            }
            if now >= phase_deadline {
                return Err(ErrorKind::Timeout(phase));
            }
            let wait = POLL.min(phase_deadline - now).min(clock.deadline - now);
            match self.from.recv_timeout(wait) {
                Ok(Ok(FromHelper::Error(error))) => {
                    return Err(match phase {
                        Phase::Open if error.code == "unsupported" => {
                            ErrorKind::Unsupported(error.message)
                        }
                        Phase::Open => ErrorKind::Unavailable {
                            code: error.code,
                            message: error.message,
                        },
                        _ => ErrorKind::Failed {
                            code: error.code,
                            message: error.message,
                        },
                    });
                }
                Ok(Ok(message)) => return Ok(message),
                Ok(Err(error)) => return Err(ErrorKind::Protocol(error.to_string())),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(self.exited()),
            }
        }
    }

    fn exited(&mut self) -> ErrorKind {
        let deadline = Instant::now() + EXIT_GRACE;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                _ => break None,
            }
        };
        self.stop();
        let stderr = self
            .stderr
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        match status {
            Some(status) => ErrorKind::Exited {
                status: status.to_string(),
                stderr,
            },
            None => ErrorKind::Protocol("the helper closed its output but kept running".into()),
        }
    }

    /// Let a helper that sent its final message exit on its own.
    fn finish(mut self, grace: Duration) {
        self.to = None;
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(None) => thread::sleep(Duration::from_millis(2)),
                _ => return,
            }
        }
    }

    fn stop(&mut self) {
        self.to = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}

fn write_loop(stdin: ChildStdin, outgoing: &mpsc::Receiver<Vec<u8>>) {
    let mut stdin = BufWriter::new(stdin);
    while let Ok(bytes) = outgoing.recv() {
        if stdin
            .write_all(&bytes)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn open(
    helpers: &Helpers,
    backend: Backend,
    plugin_id: &str,
    location: &Path,
    rate: u32,
    layout: Layout,
    max_block_frames: u32,
    params: &[super::Param],
    state: Option<&[u8]>,
    limits: &Limits,
    clock: &Clock,
) -> Result<(Helper, PluginInfo), ErrorKind> {
    let command = helpers.get(backend).ok_or(ErrorKind::NoHelper(backend))?;
    let location = location
        .to_str()
        .ok_or_else(|| ErrorKind::Invalid("the plugin location is not UTF-8".into()))?;
    if clock.cancel.is_cancelled() {
        return Err(ErrorKind::Cancelled);
    }
    let mut helper = Helper::spawn(command)?;
    helper.send(&ToHelper::Open(
        Open {
            protocol: protocol::VERSION,
            backend,
            plugin_id: plugin_id.into(),
            location: location.into(),
            rate,
            channels: layout.channels(),
            max_block_frames,
            params: params.to_vec(),
            state: state.is_some(),
        },
        state.map(<[u8]>::to_vec),
    ))?;
    let info = match helper.recv(Phase::Open, limits.open, clock)? {
        FromHelper::Ready(info) => info,
        other => return Err(unexpected("ready", &other)),
    };
    if info.protocol != protocol::VERSION {
        return Err(ErrorKind::Protocol(format!(
            "helper speaks protocol {}, not {}",
            info.protocol,
            protocol::VERSION
        )));
    }
    if info.plugin.backend != backend || info.plugin.id != plugin_id {
        return Err(ErrorKind::Protocol(format!(
            "asked for {backend} {plugin_id}, the helper loaded {} {}",
            info.plugin.backend, info.plugin.id
        )));
    }
    if info.latency_frames > MAX_LATENCY_SECONDS * rate {
        return Err(ErrorKind::Unsupported(format!(
            "latency of {} frames is longer than {MAX_LATENCY_SECONDS} s",
            info.latency_frames
        )));
    }
    Ok((helper, info))
}

fn unexpected(wanted: &str, got: &FromHelper) -> ErrorKind {
    let got = match got {
        FromHelper::Ready(_) => "ready",
        FromHelper::Block(_) => "block",
        FromHelper::Done(_) => "done",
        FromHelper::Error(_) => "error",
    };
    ErrorKind::Protocol(format!("expected {wanted}, got {got}"))
}

/// Load a plugin without rendering, to learn its identity, latency, declared
/// tail and parameters. The result's `plugin` is what a [`Stage`] names.
#[allow(clippy::too_many_arguments)]
pub fn probe(
    helpers: &Helpers,
    backend: Backend,
    plugin_id: &str,
    location: &Path,
    rate: u32,
    layout: Layout,
    limits: &Limits,
    cancel: &Cancel,
) -> Result<PluginInfo, RenderError> {
    let fail = |kind| RenderError {
        stage: None,
        plugin: None,
        kind,
    };
    limits.validate().map_err(|m| fail(ErrorKind::Invalid(m)))?;
    if !(super::MIN_RATE..=super::MAX_RATE).contains(&rate) {
        return Err(fail(ErrorKind::Invalid(format!(
            "sample rate {rate} is out of range"
        ))));
    }
    let clock = Clock {
        deadline: Instant::now() + limits.total,
        cancel,
    };
    let (mut helper, info) = open(
        helpers,
        backend,
        plugin_id,
        location,
        rate,
        layout,
        super::MAX_BLOCK,
        &[],
        None,
        limits,
        &clock,
    )
    .map_err(fail)?;
    helper.send(&ToHelper::End).map_err(fail)?;
    match helper
        .recv(Phase::Close, limits.close, &clock)
        .map_err(fail)?
    {
        FromHelper::Done(_) => helper.finish(limits.close),
        other => return Err(fail(unexpected("done", &other))),
    }
    Ok(info)
}

/// Render `request` offline. Blocking: call it off the UI and audio threads.
/// On any error nothing is returned, so the caller's previous audio stands.
pub fn render(
    request: &Request,
    helpers: &Helpers,
    limits: &Limits,
    cancel: &Cancel,
) -> Result<Rendered, RenderError> {
    let fail = |kind| RenderError {
        stage: None,
        plugin: None,
        kind,
    };
    request
        .validate()
        .map_err(|m| fail(ErrorKind::Invalid(m)))?;
    limits.validate().map_err(|m| fail(ErrorKind::Invalid(m)))?;
    if cancel.is_cancelled() {
        return Err(fail(ErrorKind::Cancelled));
    }
    let identity = request.identity();
    let clock = Clock {
        deadline: Instant::now() + limits.total,
        cancel,
    };
    let mut audio = upmix(&request.source, request.policy.layout);
    let mut stages = Vec::with_capacity(request.chain.len());
    for (index, stage) in request.chain.iter().enumerate() {
        let (output, report) =
            run_stage(stage, &audio, request, helpers, limits, &clock).map_err(|kind| {
                RenderError {
                    stage: Some(index),
                    plugin: Some(Box::new(stage.plugin.clone())),
                    kind,
                }
            })?;
        audio = output;
        stages.push(report);
    }
    Ok(Rendered {
        identity,
        layout: request.policy.layout,
        rate: request.source.rate,
        samples: audio.into(),
        source_frames: request.source.frames(),
        stages,
    })
}

fn upmix(source: &Source, layout: Layout) -> Vec<f32> {
    if source.channels == layout.channels() {
        return source.samples.to_vec();
    }
    source.samples.iter().flat_map(|&s| [s, s]).collect()
}

fn run_stage(
    stage: &Stage,
    input: &[f32],
    request: &Request,
    helpers: &Helpers,
    limits: &Limits,
    clock: &Clock,
) -> Result<(Vec<f32>, StageReport), ErrorKind> {
    let policy = &request.policy;
    let rate = request.source.rate;
    let channels = usize::from(policy.layout.channels());
    let (mut helper, info) = open(
        helpers,
        stage.plugin.backend,
        &stage.plugin.id,
        &stage.location,
        rate,
        policy.layout,
        policy.block_frames,
        &stage.params,
        stage.state.as_deref(),
        limits,
        clock,
    )?;
    if info.plugin != stage.plugin {
        return Err(ErrorKind::ArtifactChanged {
            expected: Box::new(stage.plugin.clone()),
            found: Box::new(info.plugin),
        });
    }
    let latency = if policy.compensate_latency {
        info.latency_frames as usize
    } else {
        0
    };
    let input_frames = input.len() / channels;
    let input_end = latency + input_frames;
    let cap = input_end + policy.tail.max_frames as usize;
    let hold = policy.tail.hold_frames as usize;
    let threshold = 10f64.powf(policy.tail.threshold_dbfs / 20.0) as f32;
    let block = policy.block_frames as usize;

    let mut output: Vec<f32> = Vec::with_capacity(input_end * channels);
    let mut produced = 0;
    let mut last_loud: Option<usize> = None;
    let mut capped = false;
    let mut index = 0;
    loop {
        if produced >= input_end {
            let quiet_from = input_end.max(last_loud.map_or(0, |frame| frame + 1));
            if produced - quiet_from >= hold {
                break;
            }
            if produced >= cap {
                capped = true;
                break;
            }
        }
        let frames = block.min(cap - produced);
        let mut samples = Vec::with_capacity(frames * channels);
        for frame in produced..produced + frames {
            if frame < input_frames {
                samples.extend_from_slice(&input[frame * channels..(frame + 1) * channels]);
            } else {
                samples.resize(samples.len() + channels, 0.0);
            }
        }
        helper.send(&ToHelper::Process {
            frames: frames as u32,
            samples,
        })?;
        let samples = match helper.recv(Phase::Block(index), limits.block, clock)? {
            FromHelper::Block(samples) => samples,
            other => return Err(unexpected("block", &other)),
        };
        if samples.len() != frames * channels {
            return Err(ErrorKind::Protocol(format!(
                "block {index} has {} samples, expected {}",
                samples.len(),
                frames * channels
            )));
        }
        if let Some(position) = samples.iter().position(|s| !s.is_finite()) {
            return Err(ErrorKind::NonFinite {
                frame: produced + position / channels,
            });
        }
        for (offset, frame) in samples.chunks_exact(channels).enumerate() {
            if frame.iter().any(|s| s.abs() >= threshold) {
                last_loud = Some(produced + offset);
            }
        }
        output.extend_from_slice(&samples);
        produced += frames;
        index += 1;
    }
    helper.send(&ToHelper::End)?;
    let state = match helper.recv(Phase::Close, limits.close, clock)? {
        FromHelper::Done(state) => state,
        other => return Err(unexpected("done", &other)),
    };
    helper.finish(limits.close);

    let end = input_end
        .max(last_loud.map_or(0, |frame| frame + 1))
        .min(produced);
    output.truncate(end * channels);
    output.drain(..latency * channels);
    Ok((
        output,
        StageReport {
            plugin: stage.plugin.clone(),
            name: info.name,
            latency_frames: info.latency_frames,
            declared_tail_frames: info.tail_frames,
            tail_frames: end - input_end,
            tail_capped: capped,
            state: state.map(Into::into),
        },
    ))
}
