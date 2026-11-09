use super::{Error, Result, invalid};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const LIMIT: u64 = 16 * 1024 * 1024;

fn capture(reader: impl Read + Send + 'static) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.take(LIMIT + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn wait(child: &mut Child, timeout: Duration) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(invalid("SoundCraft subprocess timed out"));
            }
        }
    }
}

fn run(mut command: Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = capture(
        child
            .stdout
            .take()
            .ok_or_else(|| invalid("missing stdout"))?,
    );
    let stderr = capture(
        child
            .stderr
            .take()
            .ok_or_else(|| invalid("missing stderr"))?,
    );
    let status = match wait(&mut child, timeout) {
        Ok(status) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let out = stdout
        .join()
        .map_err(|_| invalid("stdout reader failed"))??;
    let err = stderr
        .join()
        .map_err(|_| invalid("stderr reader failed"))??;
    if out.len() as u64 > LIMIT || err.len() as u64 > LIMIT {
        return Err(invalid("SoundCraft reply exceeds 16 MiB"));
    }
    if !status.success() {
        return Err(Error::Process {
            code: status.code(),
            detail: String::from_utf8_lossy(&err).trim().to_owned(),
        });
    }
    Ok(out)
}

pub(super) fn version(path: &Path, name: &str, timeout: Duration) -> Result<()> {
    let mut command = Command::new(path);
    command.arg("--version");
    let bytes = run(command, timeout)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut words = text.split_whitespace();
    if words.next() != Some(name) || words.next() != Some(super::VERSION) {
        return Err(invalid(format!(
            "expected {name} {}, got {} (pin {})",
            super::VERSION,
            text.trim(),
            super::COMMIT
        )));
    }
    Ok(())
}

pub(super) struct App {
    child: Child,
    cli: std::path::PathBuf,
    port: u16,
    timeout: Duration,
}

impl App {
    pub(super) fn start(app: &Path, cli: &Path, profile: &Path, timeout: Duration) -> Result<Self> {
        let mut child = Command::new(app)
            .args(["--no-audio", "--control", "0"])
            .env("SOUNDCRAFT_NO_PREFS", "1")
            .env("SOUNDCRAFT_CONFIG_DIR", profile)
            .env_remove("SOUNDCRAFT_CONTROL_PORT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (send, receive) = mpsc::sync_channel(2);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| invalid("missing app stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| invalid("missing app stderr"))?;
        for reader in [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)] {
            let send = send.clone();
            thread::spawn(move || {
                // Drain both streams; keep no unbounded app log in memory.
                let mut reader = reader;
                let mut buffer = [0u8; 4096];
                let mut line = Vec::new();
                while let Ok(count) = reader.read(&mut buffer) {
                    if count == 0 {
                        break;
                    }
                    for byte in &buffer[..count] {
                        if *byte == b'\n' {
                            if let Some(port) = line.strip_prefix(b"SOUNDCRAFT_CONTROL_PORT=")
                                && let Ok(text) = std::str::from_utf8(port)
                                && let Ok(port) = text.trim().parse::<u16>()
                            {
                                let _ = send.try_send(port);
                            }
                            line.clear();
                        } else if line.len() < 4096 {
                            line.push(*byte);
                        }
                    }
                }
            });
        }
        drop(send);
        let mut instance = Self {
            child,
            cli: cli.into(),
            port: 0,
            timeout,
        };
        instance.port = receive
            .recv_timeout(timeout)
            .map_err(|_| invalid("app did not announce its isolated control port"))?;
        if instance.port == 0 {
            return Err(invalid("app announced invalid control port"));
        }
        instance.call("ui.key", json!({"key": "Escape"}))?;
        let ui = instance.call("ui.inspect", json!({}))?;
        if !ui["audio_device"].is_null()
            || ui["playing"] != false
            || !ui["edit_layout"]["timeline"][2]
                .as_f64()
                .is_some_and(|width| width > 0.0)
        {
            return Err(invalid("app has no stopped, no-audio GUI layout"));
        }
        Ok(instance)
    }

    pub(super) fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let mut command = Command::new(&self.cli);
        command.args([
            "app",
            "--port",
            &self.port.to_string(),
            method,
            &params.to_string(),
        ]);
        let bytes = run(command, self.timeout)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("{method}: invalid reply: {error}")))?;
        if value.get("ok").is_some() {
            if value["ok"] != true || value.get("result").is_none() {
                return Err(invalid(format!("{method}: {}", value["error"])));
            }
            Ok(value["result"].clone())
        } else {
            Ok(value)
        }
    }

    pub(super) fn finish(mut self) -> Result<()> {
        self.call("app.quit", json!({}))?;
        let status = wait(&mut self.child, self.timeout)?;
        if !status.success() {
            return Err(invalid("SoundCraft app exited unsuccessfully"));
        }
        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
