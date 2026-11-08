//! Render a pad's source sample through a pedal chain with `brownnote`, and
//! cache the result on disk by content.
use super::catalog::{Catalog, MixLaw};
use crate::{
    engine::Sample,
    kit::{self, KitError},
};
use std::{
    fmt,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// Silence appended before rendering so echoes and reverb ring out.
pub const TAIL_SECONDS: f64 = 2.0;
/// The UI's limit. The bench would route more.
pub const MAX_STAGES: usize = 4;
const MAX_MIX: u8 = 120;
const CACHE_FILES: usize = 256;
const SILENCE: f32 = 1e-4;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stage {
    pub effect: String,
    /// Only the pots moved away from their default.
    pub pots: Vec<(String, u8)>,
    /// `None` until the user touches it; then the effect's own default mix.
    pub mix: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Chain {
    /// In signal order.
    pub stages: Vec<Stage>,
}

pub struct RenderJob {
    pub brownnote: PathBuf,
    pub cache: PathBuf,
    pub catalog: Arc<Catalog>,
    pub source: Sample,
    pub chain: Chain,
    pub timeout: Duration,
}

pub struct Rendered {
    pub sample: Sample,
    pub cached: bool,
    pub clipped: Option<u64>,
}

impl fmt::Debug for Rendered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rendered")
            .field("frames", &self.sample.data.len())
            .field("rate", &self.sample.rate)
            .field("cached", &self.cached)
            .field("clipped", &self.clipped)
            .finish()
    }
}

#[derive(Debug)]
pub enum RenderError {
    Invalid(String),
    NotFound(PathBuf),
    Path(crate::paths::PathError),
    NoBench(String),
    Usage(String),
    NotRoutable(String),
    Bench { code: i32, stderr: String },
    Timeout,
    Io(String),
    Output(KitError),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::NotFound(path) => write!(
                f,
                "Pedals need brownnote: run `make` or set --brownnote PATH / BROWNNOTE_BIN (looked for {})",
                path.display()
            ),
            Self::Path(error) => write!(f, "Pedals: {error}"),
            Self::NoBench(detail) => write!(
                f,
                "Pedals need the GuitarPedal bench: set BROWNNOTE_BENCH or run `make -C vendor/GuitarPedal/Validation bench`.{}{detail}",
                if detail.is_empty() { "" } else { " " }
            ),
            Self::Usage(message) => write!(f, "Pedals: brownnote rejected the command: {message}"),
            Self::NotRoutable(message) => write!(f, "Pedals: {message}"),
            Self::Bench { code, stderr } => {
                let first = stderr.lines().next().unwrap_or("no message");
                write!(f, "Pedals: the bench failed (exit {code}): {first}")
            }
            Self::Timeout => f.write_str("Pedals: the render did not finish in time"),
            Self::Io(message) => write!(f, "Pedals: {message}"),
            Self::Output(error) => write!(f, "Pedals: {error}"),
        }
    }
}
impl std::error::Error for RenderError {}

/// The `brownnote` arguments for a chain, in signal order.
pub fn argv(chain: &Chain) -> Vec<String> {
    let mut args = Vec::new();
    for stage in &chain.stages {
        args.push("--effect".into());
        args.push(stage.effect.clone());
        for (label, value) in &stage.pots {
            args.push("--pot".into());
            args.push(format!("{}:{label}={value}", stage.effect));
        }
        if let Some(mix) = stage.mix {
            args.push("--mix".into());
            args.push(format!("{}={mix}", stage.effect));
        }
    }
    args
}

/// Check a chain against the catalog before starting a process.
fn validate(chain: &Chain, catalog: &Catalog) -> Result<(), String> {
    if chain.stages.is_empty() {
        return Err("an empty chain has nothing to render".into());
    }
    if chain.stages.len() > MAX_STAGES {
        return Err(format!("a chain holds at most {MAX_STAGES} pedals"));
    }
    let mut seen = Vec::new();
    for stage in &chain.stages {
        if seen.contains(&stage.effect) {
            return Err(format!("{} appears twice in the chain", stage.effect));
        }
        seen.push(stage.effect.clone());
        let effect = catalog
            .effect(&stage.effect)
            .ok_or_else(|| format!("no pedal named {}", stage.effect))?;
        if !effect.routable() {
            return Err(format!("{} always runs and cannot be routed", effect.name));
        }
        for (label, value) in &stage.pots {
            let pot = effect
                .pots
                .iter()
                .find(|pot| &pot.label == label)
                .ok_or_else(|| format!("{} has no pot named {label}", effect.name))?;
            if *value > pot.max {
                return Err(format!(
                    "{} / {label} takes 0..{}, not {value}",
                    effect.name, pot.max
                ));
            }
        }
        if let Some(mix) = stage.mix {
            if mix > MAX_MIX {
                return Err(format!("mix takes 0..{MAX_MIX}, not {mix}"));
            }
            if effect.mix == MixLaw::None {
                return Err(format!("{} has no mix", effect.name));
            }
        }
    }
    Ok(())
}

/// FNV-1a 64. `DefaultHasher` is not stable across Rust releases; cache keys
/// must be.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub fn cache_key(job: &RenderJob, argv: &[String]) -> u64 {
    let mut hash = Fnv::new();
    hash.write(b"brownnote-native-pedal-render-v1");
    hash.write(job.catalog.source.as_bytes());
    hash.write(&job.source.rate.to_le_bytes());
    for value in job.source.data.iter() {
        hash.write(&value.to_bits().to_le_bytes());
    }
    hash.write(&TAIL_SECONDS.to_bits().to_le_bytes());
    for arg in argv {
        hash.write(arg.as_bytes());
        hash.write(&[0]);
    }
    hash.finish()
}

fn clipped(stderr: &str) -> Option<u64> {
    for line in stderr.lines() {
        let Some((_, rest)) = line.split_once("clipped ") else {
            continue;
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(count) = digits.parse() {
            return Some(count);
        }
    }
    None
}

fn write_input(path: &Path, source: &Sample) -> Result<(), RenderError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: source.rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let io = |error: hound::Error| RenderError::Io(error.to_string());
    let mut writer = hound::WavWriter::create(path, spec).map_err(io)?;
    for &value in source.data.iter() {
        // brownnote reads 16-bit PCM only; float sources above unity clip here.
        let sample = (value.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        writer.write_sample(sample).map_err(io)?;
    }
    for _ in 0..tail_frames(source.rate) {
        writer.write_sample(0_i16).map_err(io)?;
    }
    writer.finalize().map_err(io)
}

fn tail_frames(rate: u32) -> usize {
    (TAIL_SECONDS * f64::from(rate)).round() as usize
}

fn trim(sample: Sample, floor: usize) -> Sample {
    let mut end = sample.data.len();
    while end > floor && sample.data[end - 1].abs() < SILENCE {
        end -= 1;
    }
    if end == sample.data.len() {
        return sample;
    }
    Sample {
        data: sample.data[..end].to_vec().into(),
        rate: sample.rate,
    }
}

fn classify(code: i32, stderr: &str) -> RenderError {
    let stderr = stderr.trim().to_string();
    if code == 2 {
        return if stderr.contains("no bench binary found") {
            RenderError::NoBench(stderr)
        } else {
            RenderError::Usage(stderr)
        };
    }
    if code == 1 && stderr.contains("not routable") {
        return RenderError::NotRoutable(stderr);
    }
    RenderError::Bench { code, stderr }
}

fn evict(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !is_cache_file(&path) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    if files.len() <= CACHE_FILES {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    for (_, path) in files.iter().take(files.len() - CACHE_FILES) {
        remove_entry(path);
    }
}

/// `--pedal-cache DIR` takes any directory, so eviction may only touch entries
/// this cache wrote: `<16 hex digits>.wav` with its optional `.clip` beside it.
/// A directory of 300 unrelated recordings looks nothing like that and is left
/// alone.
fn is_cache_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    ext == "wav" && stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit())
}

fn clip_file(cache_file: &Path) -> PathBuf {
    cache_file.with_extension("clip")
}

fn remove_entry(cache_file: &Path) {
    let _ = fs::remove_file(cache_file);
    let _ = fs::remove_file(clip_file(cache_file));
}

/// The clipping count has to survive a cache hit, or the editor shows CLIPPED
/// for a fresh render and nothing for the same chain served from disk.
fn read_clip(cache_file: &Path) -> Option<u64> {
    let text = fs::read_to_string(clip_file(cache_file)).ok()?;
    text.trim().parse().ok()
}

fn write_clip(cache_file: &Path, clip: Option<u64>) {
    let path = clip_file(cache_file);
    match clip {
        Some(count) => {
            let _ = fs::write(path, format!("{count}\n"));
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

static TEMP: AtomicU64 = AtomicU64::new(0);

/// Render `job.chain` on `job.source`. Blocking; the UI calls this off its
/// thread. Identical jobs hit the cache instead of running brownnote.
pub fn render(job: &RenderJob) -> Result<Rendered, RenderError> {
    validate(&job.chain, &job.catalog).map_err(RenderError::Invalid)?;
    let cache = crate::paths::Resolver::current()
        .and_then(|paths| paths.resolve(crate::paths::Resource::Cache, Some(&job.cache)))
        .map_err(RenderError::Path)?;
    let args = argv(&job.chain);
    let key = cache_key(job, &args);
    let cache_file = cache.join(format!("{key:016x}.wav"));
    // The bench writes the source plus the tail, which is longer than the
    // 30-second kit limit once a source is long in its own right.
    let limit = job.source.data.len() + tail_frames(job.source.rate);

    // A cache entry that fails to load is a miss; the atomic replace fixes it.
    if cache_file.is_file()
        && let Ok(sample) = kit::load_wav_max(&cache_file, |_| limit as u64)
    {
        return Ok(Rendered {
            sample: trim(sample, job.source.data.len()),
            cached: true,
            clipped: read_clip(&cache_file),
        });
    }

    let unique = TEMP.fetch_add(1, Relaxed);
    let stem = format!("{key:016x}.{}.{unique}", std::process::id());
    let input = cache.join(format!("{stem}.in.wav"));
    let temp = cache.join(format!("{stem}.tmp"));

    let result = run(job, &args, &input, &temp);
    let _ = fs::remove_file(&input);
    let (sample, clipped) = match result {
        Ok(rendered) => rendered,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
    };
    fs::rename(&temp, &cache_file).map_err(|error| {
        let _ = fs::remove_file(&temp);
        RenderError::Io(format!("{}: {error}", cache_file.display()))
    })?;
    write_clip(&cache_file, clipped);
    evict(&cache);
    Ok(Rendered {
        sample,
        cached: false,
        clipped,
    })
}

fn run(
    job: &RenderJob,
    args: &[String],
    input: &Path,
    temp: &Path,
) -> Result<(Sample, Option<u64>), RenderError> {
    write_input(input, &job.source)?;
    let stdin = File::open(input)
        .map_err(|error| RenderError::Io(format!("{}: {error}", input.display())))?;
    let stdout = File::create(temp)
        .map_err(|error| RenderError::Io(format!("{}: {error}", temp.display())))?;
    let mut command = crate::paths::Resolver::current()
        .and_then(|paths| paths.pedal_command(Some(&job.brownnote), None))
        .map_err(|error| {
            if error.resource == crate::paths::Resource::Brownnote
                && error.kind == crate::paths::PathErrorKind::Missing
            {
                RenderError::NotFound(error.path.clone().unwrap_or_else(|| job.brownnote.clone()))
            } else {
                RenderError::Path(error)
            }
        })?;
    let mut child = command
        .args(args)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            RenderError::Io(format!(
                "{}: {error}",
                command.get_program().to_string_lossy()
            ))
        })?;
    let deadline = Instant::now() + job.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                terminate(&mut child);
                return Err(RenderError::Timeout);
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                terminate(&mut child);
                return Err(RenderError::Io(error.to_string()));
            }
        }
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let code = status.code().unwrap_or(-1);
    if code != 0 {
        return Err(classify(code, &stderr));
    }
    let limit = job.source.data.len() + tail_frames(job.source.rate);
    let output = kit::load_wav_max(temp, |_| limit as u64).map_err(RenderError::Output)?;
    let mut rendered = trim(output, job.source.data.len());
    rendered.rate = job.source.rate;
    Ok((rendered, clipped(&stderr)))
}

/// How long brownnote gets to act on SIGTERM before it is killed outright.
/// Its handler gives the bench two seconds of grace, then kills it.
const TERM_GRACE: Duration = Duration::from_millis(2500);

/// Stop a render. SIGTERM goes to brownnote, whose handler forwards it to the
/// bench and waits for it to leave; SIGKILL alone would bypass that and leave
/// the bench computing with nobody to read its output.
fn terminate(child: &mut std::process::Child) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
