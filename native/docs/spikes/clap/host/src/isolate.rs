//! Failure boundary comparison: the same plugin hosted in-process (with a
//! watchdog) and in a helper process (with restart).
//!
//! The helper is this same binary run as `helper <bundle> <plugin-id>`. It
//! speaks a tiny length-free binary protocol over stdin/stdout, one request
//! and one reply per block. Pipes, not shared memory: this measures an upper
//! bound for IPC cost, not the cheapest possible transport.

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::clap::{HostError, Library};
use crate::json::Json;
use crate::scenario::Scenario;
use crate::wav;

const ACTIVATE: u8 = 1;
const PROCESS: u8 = 2;
const SAVE: u8 = 3;
const LOAD: u8 = 4;

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// Helper side. Exits when stdin closes. Any plugin fault kills this process,
/// which is the point.
pub fn helper(bundle: &Path, plugin_id: &str) -> Result<(), HostError> {
    let lib = Library::open(bundle)?;
    let mut inst = lib.instantiate(plugin_id)?;
    let mut rx = BufReader::new(std::io::stdin().lock());
    let mut tx = BufWriter::new(std::io::stdout().lock());
    let mut input = vec![vec![Vec::new(); 2]];
    let mut output = vec![vec![Vec::new(); 2]];
    // Reported once so the parent can time load+init separately from activation.
    tx.write_all(&[0]).and_then(|()| tx.flush()).ok();
    loop {
        let mut cmd = [0u8];
        if rx.read_exact(&mut cmd).is_err() {
            return Ok(());
        }
        match cmd[0] {
            ACTIVATE => {
                let sr = read_u32(&mut rx).expect("sr");
                let block = read_u32(&mut rx).expect("block");
                let ok = inst
                    .activate(f64::from(sr), 1, block)
                    .and_then(|()| inst.start_processing());
                tx.write_all(&[u8::from(ok.is_err())]).ok();
            }
            PROCESS => {
                let frames = read_u32(&mut rx).expect("frames") as usize;
                for ch in 0..2 {
                    let mut raw = vec![0u8; frames * 4];
                    rx.read_exact(&mut raw).expect("audio");
                    input[0][ch] = raw
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect();
                    output[0][ch] = vec![0.0; frames];
                }
                let r = inst.process(frames as u32, &mut input, &mut output, &[]);
                tx.write_all(&r.status.to_le_bytes()).ok();
                for chan in &output[0] {
                    for s in chan {
                        tx.write_all(&s.to_le_bytes()).ok();
                    }
                }
            }
            SAVE => {
                inst.stop_processing();
                let s = inst.save_state().unwrap_or_default();
                inst.start_processing().ok();
                tx.write_all(&(s.len() as u32).to_le_bytes()).ok();
                tx.write_all(&s).ok();
            }
            LOAD => {
                let n = read_u32(&mut rx).expect("len") as usize;
                let mut s = vec![0; n];
                rx.read_exact(&mut s).expect("state");
                inst.stop_processing();
                let ok = inst.load_state(&s).is_ok();
                inst.start_processing().ok();
                tx.write_all(&[u8::from(!ok)]).ok();
            }
            _ => return Ok(()),
        }
        tx.flush().ok();
    }
}

/// Parent-side handle to one helper process.
struct Helper {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Vec<u8>>,
    /// Bytes read past the end of the previous reply; pipe reads do not
    /// respect reply boundaries.
    pending: Vec<u8>,
}

enum Fail {
    /// Helper exited; `signal` is set when a signal killed it.
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// No reply within the deadline; the helper was killed.
    TimedOut,
}

impl Helper {
    /// Spawns and waits for the "loaded" byte. Every reply is read by a
    /// thread so the parent can wait with a deadline.
    fn spawn(bundle: &Path, plugin_id: &str, timeout: Duration) -> Result<Self, Fail> {
        let mut child = Command::new(std::env::current_exe().expect("current exe"))
            .args(["helper", &bundle.to_string_lossy(), plugin_id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn helper");
        let stdin = child.stdin.take().expect("stdin");
        let mut stdout = child.stdout.take().expect("stdout");
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut h = Self {
            child,
            stdin,
            replies,
            pending: Vec::new(),
        };
        h.recv(1, timeout)?;
        Ok(h)
    }

    fn recv(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>, Fail> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::with_capacity(n);
        let take = n.min(self.pending.len());
        out.extend(self.pending.drain(..take));
        while out.len() < n {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok(chunk) => self.pending.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => {
                    self.child.kill().ok();
                    self.child.wait().ok();
                    return Err(Fail::TimedOut);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let st = self.child.wait().expect("wait helper");
                    return Err(Fail::Exited {
                        code: st.code(),
                        signal: st.signal(),
                    });
                }
            }
            let take = (n - out.len()).min(self.pending.len());
            out.extend(self.pending.drain(..take));
        }
        Ok(out)
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), Fail> {
        if self
            .stdin
            .write_all(bytes)
            .and_then(|()| self.stdin.flush())
            .is_err()
        {
            let st = self.child.wait().expect("wait helper");
            return Err(Fail::Exited {
                code: st.code(),
                signal: st.signal(),
            });
        }
        Ok(())
    }

    fn activate(&mut self, sr: u32, block: u32, timeout: Duration) -> Result<bool, Fail> {
        let mut m = vec![ACTIVATE];
        m.extend_from_slice(&sr.to_le_bytes());
        m.extend_from_slice(&block.to_le_bytes());
        self.send(&m)?;
        Ok(self.recv(1, timeout)?[0] == 0)
    }

    fn process(&mut self, input: &[Vec<f32>; 2], timeout: Duration) -> Result<[Vec<f32>; 2], Fail> {
        let frames = input[0].len();
        let mut m = Vec::with_capacity(5 + frames * 8);
        m.push(PROCESS);
        m.extend_from_slice(&(frames as u32).to_le_bytes());
        for ch in input {
            for s in ch {
                m.extend_from_slice(&s.to_le_bytes());
            }
        }
        self.send(&m)?;
        let r = self.recv(4 + frames * 8, timeout)?;
        let samples: Vec<f32> = r[4..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        Ok([samples[..frames].to_vec(), samples[frames..].to_vec()])
    }

    fn save(&mut self, timeout: Duration) -> Result<Vec<u8>, Fail> {
        self.send(&[SAVE])?;
        let n = u32::from_le_bytes(self.recv(4, timeout)?.try_into().unwrap()) as usize;
        if n == 0 {
            Ok(Vec::new())
        } else {
            self.recv(n, timeout)
        }
    }

    fn load(&mut self, state: &[u8], timeout: Duration) -> Result<bool, Fail> {
        let mut m = vec![LOAD];
        m.extend_from_slice(&(state.len() as u32).to_le_bytes());
        m.extend_from_slice(state);
        self.send(&m)?;
        Ok(self.recv(1, timeout)?[0] == 0)
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn input_block(sc: &Scenario, start: u32, n: u32) -> [Vec<f32>; 2] {
    let ch = |c| (start..start + n).map(|f| sc.input_sample(f, c)).collect();
    [ch(0), ch(1)]
}

fn fail_json(block: u32, f: &Fail, detect: Duration) -> Json {
    let (kind, code, signal) = match f {
        Fail::Exited { code, signal } => ("helper-exited", *code, *signal),
        Fail::TimedOut => ("helper-timed-out", None, None),
    };
    Json::obj([
        ("block", Json::num(block)),
        ("kind", Json::str(kind)),
        (
            "exit_code",
            code.map_or(Json::Null, |c| Json::Num(c.to_string())),
        ),
        (
            "signal",
            signal.map_or(Json::Null, |s| Json::Num(s.to_string())),
        ),
        ("detect_ms", Json::f(detect.as_secs_f64() * 1e3)),
    ])
}

/// Renders a scenario through a helper process. On a crash or timeout it
/// substitutes the dry input for the lost block, restarts the helper, and
/// restores the last state saved from it. After `max_restarts` it disables
/// the plugin (dry pass-through) for the rest of the render. Non-finite
/// output also disables it: such a block is replaced with dry input.
pub fn isolate(args: &[String]) -> Result<(), HostError> {
    let (bundle, plugin_id, scenario, out, name) = (
        Path::new(&args[0]),
        args[1].as_str(),
        Path::new(&args[2]),
        Path::new(&args[3]),
        &args[4],
    );
    let mut timeout = Duration::from_millis(200);
    let mut max_restarts = 2u32;
    let mut block = None;
    let mut it = args[5..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--timeout-ms" => {
                timeout = Duration::from_millis(it.next().and_then(|v| v.parse().ok()).expect("ms"))
            }
            "--max-restarts" => max_restarts = it.next().and_then(|v| v.parse().ok()).expect("n"),
            "--block" => block = it.next().and_then(|v| v.parse().ok()),
            other => panic!("unknown option {other}"),
        }
    }
    let mut sc = Scenario::load(scenario);
    if let Some(b) = block {
        sc.block = b;
    }
    let start_timeout = Duration::from_secs(10);
    let mut audio = [Vec::new(), Vec::new()];
    let mut failures = Vec::new();
    let mut restarts_ms = Vec::new();
    let mut block_ns = Vec::new();
    let mut disabled: Option<String> = None;
    let mut substituted_blocks = 0u32;
    let mut restored_states = 0u32;

    let t0 = Instant::now();
    let mut helper = start(bundle, plugin_id, &sc, start_timeout);
    let first_start_ms = t0.elapsed().as_secs_f64() * 1e3;
    let mut state = match &mut helper {
        Ok(h) => h.save(timeout).ok(),
        Err(e) => {
            disabled = Some(format!("start: {e}"));
            None
        }
    };
    let state_bytes = state.as_ref().map(Vec::len);
    let mut restarts = 0u32;
    let mut index = 0u32;
    let mut start_frame = 0;
    while start_frame < sc.frames {
        let n = sc.block.min(sc.frames - start_frame);
        let dry = input_block(&sc, start_frame, n);
        let mut wet = None;
        if disabled.is_none() {
            let h = helper.as_mut().expect("helper running while enabled");
            let t = Instant::now();
            match h.process(&dry, timeout) {
                Ok(o) if o.iter().flatten().all(|s| s.is_finite()) => {
                    block_ns.push(t.elapsed().as_nanos() as u64);
                    wet = Some(o);
                }
                Ok(_) => {
                    failures.push(Json::obj([
                        ("block", Json::num(index)),
                        ("kind", Json::str("nonfinite-output")),
                        ("detect_ms", Json::f(t.elapsed().as_secs_f64() * 1e3)),
                    ]));
                    disabled = Some("nonfinite-output".into());
                }
                Err(f) => {
                    failures.push(fail_json(index, &f, t.elapsed()));
                    if restarts < max_restarts {
                        restarts += 1;
                        let r0 = Instant::now();
                        helper = start(bundle, plugin_id, &sc, start_timeout);
                        if let (Ok(h), Some(s)) = (&mut helper, &state)
                            && h.load(s, timeout).unwrap_or(false)
                        {
                            restored_states += 1;
                        }
                        restarts_ms.push(r0.elapsed().as_secs_f64() * 1e3);
                        if helper.is_err() {
                            disabled = Some("restart-failed".into());
                        }
                    } else {
                        disabled =
                            Some(format!("{} failures, restart budget spent", failures.len()));
                    }
                }
            }
        }
        let block = wet.unwrap_or_else(|| {
            substituted_blocks += 1;
            dry
        });
        for ch in 0..2 {
            audio[ch].extend_from_slice(&block[ch]);
        }
        start_frame += n;
        index += 1;
    }
    if let Ok(h) = &mut helper
        && disabled.is_none()
    {
        state = h.save(timeout).ok();
    }
    std::fs::create_dir_all(out).expect("create out dir");
    wav::write_f32(&out.join(format!("{name}.wav")), sc.sample_rate, &audio).expect("write wav");
    block_ns.sort_unstable();
    let pct = |p: f64| {
        block_ns
            .get(((block_ns.len().max(1) - 1) as f64 * p) as usize)
            .copied()
            .unwrap_or(0)
    };
    let summary = Json::obj([
        ("plugin_id", Json::str(plugin_id)),
        ("boundary", Json::str("helper-process")),
        ("frames", Json::num(sc.frames)),
        ("block", Json::num(sc.block)),
        ("blocks", Json::num(index)),
        ("timeout_ms", Json::num(timeout.as_millis() as u64)),
        ("host_survived", Json::Bool(true)),
        ("first_start_ms", Json::f(first_start_ms)),
        ("failures", Json::Arr(failures)),
        ("restarts", Json::num(restarts)),
        (
            "restart_ms",
            Json::Arr(restarts_ms.into_iter().map(Json::f).collect()),
        ),
        ("states_restored_after_restart", Json::num(restored_states)),
        (
            "initial_state_bytes",
            Json::opt_num(state_bytes.map(|n| n as u64)),
        ),
        (
            "final_state_bytes",
            Json::opt_num(state.map(|s| s.len() as u64)),
        ),
        ("disabled", disabled.map_or(Json::Null, Json::str)),
        ("dry_substituted_blocks", Json::num(substituted_blocks)),
        ("roundtrip_ns_p50", Json::num(pct(0.5))),
        ("roundtrip_ns_p99", Json::num(pct(0.99))),
        ("roundtrip_ns_max", Json::num(pct(1.0))),
    ]);
    std::fs::write(out.join(format!("{name}.json")), summary.to_string() + "\n")
        .expect("write json");
    Ok(())
}

fn start(
    bundle: &Path,
    plugin_id: &str,
    sc: &Scenario,
    timeout: Duration,
) -> Result<Helper, String> {
    let mut h = Helper::spawn(bundle, plugin_id, timeout)
        .map_err(|_| "load/init failed in helper".to_string())?;
    match h.activate(sc.sample_rate, sc.block, timeout) {
        Ok(true) => Ok(h),
        Ok(false) => Err("activate failed in helper".into()),
        Err(_) => Err("helper died during activate".into()),
    }
}

/// In-process hosting with a watchdog. The audio thread publishes a block
/// counter and appends one line per finished block to `<name>.progress`, so
/// whatever happens to this process, the last completed block is on disk.
/// A hang is detected, but the stuck thread and the plugin instance cannot
/// be reclaimed: the probe reports and exits without joining or destroying.
pub fn inproc(args: &[String]) -> Result<(), HostError> {
    let (bundle, plugin_id, scenario, out, name) = (
        Path::new(&args[0]),
        args[1].clone(),
        Path::new(&args[2]),
        Path::new(&args[3]),
        args[4].clone(),
    );
    let timeout = Duration::from_millis(match args.get(5).map(String::as_str) {
        Some("--timeout-ms") => args[6].parse().expect("ms"),
        _ => 200,
    });
    let sc = Scenario::load(scenario);
    std::fs::create_dir_all(out).expect("create out dir");
    let progress_path = out.join(format!("{name}.progress"));
    let mut progress = std::fs::File::create(&progress_path).expect("progress file");
    let lib = Library::open(bundle)?;
    let mut inst = lib.instantiate(&plugin_id)?;
    inst.activate(f64::from(sc.sample_rate), 1, sc.block)?;
    let done = Arc::new(AtomicU32::new(0));
    let finished = Arc::new(AtomicU32::new(0));
    let (d, fin) = (done.clone(), finished.clone());
    let sc2 = sc.clone();
    let audio_thread = std::thread::spawn(move || {
        let mut inst = inst;
        inst.start_processing().expect("start");
        let mut input = vec![vec![Vec::new(); 2]];
        let mut output = vec![vec![Vec::new(); 2]];
        let mut nonfinite = 0u64;
        let mut start = 0;
        let mut index = 0u32;
        while start < sc2.frames {
            let n = sc2.block.min(sc2.frames - start);
            let [l, r] = input_block(&sc2, start, n);
            input[0] = vec![l, r];
            output[0] = vec![vec![0.0; n as usize]; 2];
            inst.process(n, &mut input, &mut output, &[]);
            nonfinite += output[0]
                .iter()
                .flatten()
                .filter(|s| !s.is_finite())
                .count() as u64;
            index += 1;
            writeln!(progress, "{index}\t{nonfinite}").ok();
            progress.sync_data().ok();
            d.store(index, Ordering::Release);
            start += n;
        }
        inst.stop_processing();
        fin.store(1, Ordering::Release);
        (inst, nonfinite)
    });
    let mut last = 0;
    let mut last_change = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(5));
        if finished.load(Ordering::Acquire) == 1 {
            break;
        }
        let now = done.load(Ordering::Acquire);
        if now != last {
            last = now;
            last_change = Instant::now();
        } else if last_change.elapsed() > timeout {
            let report = Json::obj([
                ("plugin_id", Json::str(plugin_id)),
                ("boundary", Json::str("in-process")),
                ("outcome", Json::str("hang-detected")),
                ("blocks_completed", Json::num(last)),
                (
                    "detect_ms",
                    Json::f(last_change.elapsed().as_secs_f64() * 1e3),
                ),
                ("audio_thread_reclaimed", Json::Bool(false)),
                ("plugin_destroyed", Json::Bool(false)),
            ]);
            std::fs::write(out.join(format!("{name}.json")), report.to_string() + "\n")
                .expect("write json");
            // Returning would run destructors that call into the stuck
            // plugin. Exiting is the only clean option left in-process.
            std::process::exit(4);
        }
    }
    let (inst, nonfinite) = audio_thread.join().expect("audio thread");
    drop(inst);
    let report = Json::obj([
        ("plugin_id", Json::str(plugin_id)),
        ("boundary", Json::str("in-process")),
        ("outcome", Json::str("completed")),
        ("blocks_completed", Json::num(done.load(Ordering::Acquire))),
        ("nonfinite_samples", Json::num(nonfinite)),
    ]);
    std::fs::write(out.join(format!("{name}.json")), report.to_string() + "\n")
        .expect("write json");
    Ok(())
}
