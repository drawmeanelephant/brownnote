//! Pinned, subprocess-only handoff from manifest v1. No private session JSON.
//! Audio is authoritative; editable sampler/MIDI tracks start muted.

mod process;

use crate::export::manifest::{
    AssetId, AudioRole, EventId, LossRecord, MANIFEST_NAME, Manifest, PartEntry, PartId, PartKind,
};
use crate::export::midi;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

pub const VERSION: &str = "0.19.0";
pub const COMMIT: &str = "04878f8fda885c95f8be5893bbfe4ea4e062a814";
pub const PIN: &str = include_str!("soundcraft/pin.json");
const RATE: u64 = 48_000;
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(String),
    Process { code: Option<i32>, detail: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "SoundCraft adapter: {error}"),
            Self::Invalid(detail) => write!(f, "SoundCraft adapter: {detail}"),
            Self::Process { code, detail } => {
                write!(f, "SoundCraft subprocess failed ({code:?}): {detail}")
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
fn invalid(detail: impl Into<String>) -> Error {
    Error::Invalid(detail.into())
}

/// Optional target-local intent, not an extension or inference of manifest v1.
/// A retained event render is never guessed to be an instrument's source sample.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SamplerMapping {
    pub part: PartId,
    pub asset: AssetId,
    pub root_note: u8,
    pub key_low: u8,
    pub key_high: u8,
    pub key_track: bool,
    pub one_shot: bool,
    #[serde(default)]
    pub choke_group: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub bundle: PathBuf,
    pub app: PathBuf,
    pub cli: PathBuf,
    /// A new directory. Move the entire directory, not just session.scraft.
    pub output: PathBuf,
    pub mappings: Vec<SamplerMapping>,
    pub timeout: Duration,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    pub event: Option<EventId>,
    pub part: Option<PartId>,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AudioClip {
    pub event: EventId,
    pub track: u64,
    pub clip: u64,
    /// Deterministic pad/part, take and hit identity assigned with the target's
    /// public `track.rename`/`clip.rename`, at most 64 Unicode scalar values.
    pub name: String,
    pub start: i64,
    pub length: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MidiPart {
    pub part: PartId,
    pub track: u64,
    pub clip: u64,
    pub name: String,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Note {
    pub start: i64,
    pub pitch: u8,
    pub channel: u8,
    pub length: i64,
    pub velocity: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub soundcraft_version: String,
    pub soundcraft_commit: String,
    pub target_validation: String,
    pub session: PathBuf,
    pub audio: Vec<AudioClip>,
    pub midi: Vec<MidiPart>,
    pub bundle_losses: Vec<LossRecord>,
    pub records: Vec<Record>,
}

impl Report {
    fn record(
        &mut self,
        event: Option<EventId>,
        part: Option<PartId>,
        kind: &str,
        detail: impl Into<String>,
    ) {
        self.records.push(Record {
            event,
            part,
            kind: kind.into(),
            detail: detail.into(),
        });
    }
}

struct Media {
    bytes: Vec<u8>,
    frames: i64,
}
struct Input {
    manifest: Manifest,
    bytes: Vec<u8>,
    media: BTreeMap<AssetId, Media>,
    /// The bundle's own SMF, kept so the retained manifest copy stays complete.
    midi: Option<(PathBuf, Vec<u8>)>,
}

fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > limit {
        return Err(invalid(format!(
            "not a bounded regular file: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("input grew beyond its size limit"));
    }
    Ok(bytes)
}

fn rounded(nanos: u64, numerator: u64, denominator: u64) -> Result<(i64, bool)> {
    let value = u128::from(nanos) * u128::from(numerator);
    let divisor = u128::from(denominator);
    let rounded = (value + divisor / 2) / divisor;
    Ok((
        i64::try_from(rounded).map_err(|_| invalid("target time overflows i64"))?,
        value % divisor != 0,
    ))
}

/// Reproduce the bundle writer's SMF gate exactly: absolute onset and absolute
/// end round independently to the nearest encoded tempo tick, ties later, and
/// every gate gets at least one tick. The returned bool reports any rounding
/// or minimum-gate change.
fn midi_gate(
    start_nanos: u64,
    duration_nanos: u64,
    tempo_milli_bpm: u64,
) -> Result<(i64, i64, bool)> {
    let micros =
        midi::tempo_micros(tempo_milli_bpm).map_err(|_| invalid("invalid bundle tempo"))?;
    let start =
        midi::tick_at(start_nanos, micros).map_err(|_| invalid("target tick overflows i64"))?;
    let end_nanos = start_nanos
        .checked_add(duration_nanos)
        .ok_or_else(|| invalid("event end overflows"))?;
    let rounded_end =
        midi::tick_at(end_nanos, micros).map_err(|_| invalid("target tick overflows i64"))?;
    let end = rounded_end.max(
        start
            .checked_add(1)
            .ok_or_else(|| invalid("target tick overflows i64"))?,
    );
    let loss = u128::from(start_nanos) * u128::from(midi::PPQ)
        != u128::from(start) * u128::from(micros) * 1_000
        || u128::from(end_nanos) * u128::from(midi::PPQ)
            != u128::from(rounded_end) * u128::from(micros) * 1_000
        || end != rounded_end;
    let start_value = i64::try_from(start).map_err(|_| invalid("target tick overflows i64"))?;
    let length = i64::try_from(end - start).map_err(|_| invalid("target tick overflows i64"))?;
    Ok((start_value, length, loss))
}

fn note_bounds(event: &crate::export::manifest::EventEntry) -> Result<()> {
    if !event.pitch.is_some_and(|x| x <= 127)
        || !event.channel.is_some_and(|x| x <= 15)
        || !event.velocity.is_some_and(|x| (1..=127).contains(&x))
    {
        return Err(invalid(
            "MIDI note needs pitch 0..127, channel 0..15 and velocity 1..127",
        ));
    }
    Ok(())
}

fn load(bundle: &Path, mappings: &[SamplerMapping]) -> Result<Input> {
    let root = fs::canonicalize(bundle)?;
    let bytes = read_regular(&root.join(MANIFEST_NAME), 16 * 1024 * 1024)?;
    let manifest = Manifest::decode(&bytes).map_err(|error| invalid(error.to_string()))?;
    if manifest.timing.tempo_milli_bpm == 0
        || !(8_000..=192_000).contains(&manifest.timing.sample_rate_hz)
    {
        return Err(invalid("invalid bundle tempo or sample rate"));
    }
    // Only media the target actually loads must satisfy its proven import
    // slice. Source-only provenance is retained and verified, but the target
    // never decodes it.
    let imported: BTreeSet<AssetId> = manifest
        .events
        .iter()
        .filter_map(|event| event.asset)
        .chain(mappings.iter().map(|mapping| mapping.asset))
        .collect();
    let mut media = BTreeMap::new();
    for asset in &manifest.assets {
        let path = root.join(&asset.path);
        if !fs::canonicalize(&path)?.starts_with(&root) {
            return Err(invalid("media escapes bundle"));
        }
        let bytes = read_regular(&path, 256 * 1024 * 1024)?;
        if bytes.len() as u64 != asset.bytes || AssetId::of(&bytes) != asset.id {
            return Err(invalid(format!("media size/hash mismatch: {}", asset.id)));
        }
        let reader = hound::WavReader::new(io::Cursor::new(&bytes))
            .map_err(|error| invalid(format!("{}: {error}", asset.id)))?;
        let spec = reader.spec();
        if reader.duration() == 0
            || !(1..=2).contains(&spec.channels)
            || !(8_000..=192_000).contains(&spec.sample_rate)
            || !matches!(
                (spec.sample_format, spec.bits_per_sample),
                (hound::SampleFormat::Int, 16) | (hound::SampleFormat::Float, 32)
            )
        {
            return Err(invalid(
                "media outside native PCM16/float32 mono/stereo 8-192k WAV slice",
            ));
        }
        if imported.contains(&asset.id) && ![44_100, 48_000].contains(&spec.sample_rate) {
            return Err(invalid(
                "media outside proven PCM16/float32 mono/stereo 44.1/48k WAV slice",
            ));
        }
        let frames = (u64::from(reader.duration()) * RATE).div_ceil(u64::from(spec.sample_rate));
        media.insert(
            asset.id,
            Media {
                bytes,
                frames: frames as i64,
            },
        );
    }
    let midi = match &manifest.midi {
        Some(entry) => {
            if entry.path.components().count() != 1
                || !matches!(
                    entry.path.components().next(),
                    Some(std::path::Component::Normal(_))
                )
            {
                return Err(invalid("bundle MIDI path must be a plain file name"));
            }
            let path = root.join(&entry.path);
            if !fs::canonicalize(&path)?.starts_with(&root) {
                return Err(invalid("MIDI escapes bundle"));
            }
            Some((entry.path.clone(), read_regular(&path, 16 * 1024 * 1024)?))
        }
        None => None,
    };
    for event in &manifest.events {
        event
            .start_nanos
            .checked_add(event.duration_nanos)
            .ok_or_else(|| invalid("event end overflows"))?;
        let part = manifest
            .part(event.part)
            .ok_or_else(|| invalid("missing part"))?;
        match part.kind {
            PartKind::PadClips { pad } if event.pad != Some(pad) || event.asset.is_none() => {
                return Err(invalid(
                    "pad hit needs a matching pad and retained/source media",
                ));
            }
            // Instrument notes follow the writer's documented SMF contract:
            // absolute endpoints round independently and a legal minimum gate
            // is one tick, including zero-duration notes. Live-instrument notes
            // are not imported as MIDI, so their gate must never block the
            // authoritative retained take.
            PartKind::InstrumentNotes { .. } => {
                note_bounds(event)?;
                midi_gate(
                    event.start_nanos,
                    event.duration_nanos,
                    u64::from(manifest.timing.tempo_milli_bpm),
                )?;
            }
            PartKind::LiveInstrumentNotes { .. } => note_bounds(event)?,
            _ => {}
        }
    }
    Ok(Input {
        manifest,
        bytes,
        media,
        midi,
    })
}

struct Staging {
    path: PathBuf,
    published: bool,
}
impl Drop for Staging {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Owns a fresh no-audio GUI process and its isolated profile. Never connects to
/// an existing app. A failure never publishes an output session directory.
pub fn convert(request: &Request) -> Result<Report> {
    let input = load(&request.bundle, &request.mappings)?;
    if request.timeout.is_zero() {
        return Err(invalid("timeout must be positive"));
    }
    let app_path = fs::canonicalize(&request.app)?;
    let cli_path = fs::canonicalize(&request.cli)?;
    process::version(&app_path, "soundcraft", request.timeout)?;
    process::version(&cli_path, "soundcraft-cli", request.timeout)?;
    let output = std::path::absolute(&request.output)?;
    let parent = output
        .parent()
        .ok_or_else(|| invalid("output needs a parent"))?;
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(&output).is_ok() {
        return Err(invalid("output already exists"));
    }
    let name = output
        .file_name()
        .and_then(|x| x.to_str())
        .ok_or_else(|| invalid("output needs a Unicode name"))?;
    let staging_path = parent.join(format!(".{name}.soundcraft-staging"));
    // Never adopt or delete another run's leftovers.
    fs::create_dir(&staging_path)?;
    let mut staging = Staging {
        path: staging_path,
        published: false,
    };
    let stage = fs::canonicalize(&staging.path)?;
    if stage.to_str().is_none() {
        return Err(invalid("output path must be Unicode"));
    }
    fs::create_dir(stage.join("media"))?;
    fs::write(stage.join(MANIFEST_NAME), &input.bytes)?;
    for (id, media) in &input.media {
        fs::write(stage.join(id.relative_path()), &media.bytes)?;
    }
    if let Some((path, bytes)) = &input.midi {
        fs::write(stage.join(path), bytes)?;
    }
    let mut report = Report {
        soundcraft_version: VERSION.into(), soundcraft_commit: COMMIT.into(),
        target_validation: "v0.19.0 GUI checked through its control protocol (no audio device) on 2026-11-09: #95 recipe plus adapter edit/save/reopen/relocation. Maintainer listening and hand-driven UI edits are still outstanding.".into(),
        session: "session.scraft".into(), audio: Vec::new(), midi: Vec::new(),
        bundle_losses: input.manifest.losses.clone(), records: Vec::new(),
    };
    report.record(None, None, "boundary", "Full source manifest/media retained. Session uses 48 kHz and 960 PPQ. MIDI/sampler tracks are muted editable approximations, never sound-equivalent replacements.");
    if input.manifest.timing.sample_rate_hz != RATE as u32 {
        report.record(
            None,
            None,
            "conversion",
            "Session runs at 48 kHz; source media is resampled by SoundCraft.",
        );
    }
    let mut app = process::App::start(
        &app_path,
        &cli_path,
        &stage.join("profile"),
        request.timeout,
    )?;
    build(&mut app, &stage, &input, &request.mappings, &mut report)?;
    app.call(
        "session.save_as",
        json!({"path": stage.join("session.scraft")}),
    )?;
    app.finish()?;
    let mut reopened = process::App::start(
        &app_path,
        &cli_path,
        &stage.join("profile"),
        request.timeout,
    )?;
    let opened = reopened.call(
        "session.open",
        json!({"path": stage.join("session.scraft")}),
    )?;
    if opened["missing"] != json!([]) {
        return Err(invalid("saved session has missing media"));
    }
    verify(&mut reopened, &stage, &input.manifest, &report)?;
    reopened.finish()?;
    if !stage.join("session.scraft").is_file() {
        return Err(invalid("target did not save session.scraft"));
    }
    fs::write(
        stage.join("adapter-report.json"),
        serde_json::to_vec_pretty(&report).map_err(|error| invalid(error.to_string()))?,
    )?;
    fs::write(stage.join("soundcraft-pin.json"), PIN)?;
    // A profile is machine state, not session media.
    if stage.join("profile").exists() {
        fs::remove_dir_all(stage.join("profile"))?;
    }
    if fs::symlink_metadata(&output).is_ok() {
        return Err(invalid("output appeared during conversion"));
    }
    fs::rename(&stage, &output)?;
    staging.published = true;
    Ok(report)
}

fn number(value: &Value, context: &str) -> Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| invalid(format!("{context}: missing numeric ID")))
}

/// Target track gain for the manifest's static master. Hit assets are
/// pre-master, so this is applied exactly once to each imported clip track.
fn master_db(gain: f32) -> f64 {
    if gain <= 0.0 {
        // The target's documented fader floor; the reference at master 0 is
        // mathematically silent, which a finite fader cannot be.
        -144.0
    } else {
        (20.0 * f64::from(gain).log10()).clamp(-144.0, 12.0)
    }
}

/// `track.rename` and `clip.rename` trim and truncate names to 64 Unicode
/// scalar values, so every identity is bounded here before it is sent.
const MAX_NAME_CHARS: usize = 64;

/// A readable identity for one imported event. Pad/part text is variable, so
/// it is truncated before the ` · Take <k> · Hit <n>` suffix that makes the
/// name unique: the suffix is never cut and long labels cannot collide.
fn identity_name(prefix: &str, take: u64, hit: u64) -> Result<String> {
    let suffix = format!(" · Take {take} · Hit {hit}");
    let room = MAX_NAME_CHARS
        .checked_sub(suffix.chars().count())
        .ok_or_else(|| invalid("identity suffix exceeds the target's name limit"))?;
    Ok(format!(
        "{}{suffix}",
        prefix.chars().take(room).collect::<String>()
    ))
}

fn bounded_name(text: &str) -> String {
    text.chars().take(MAX_NAME_CHARS).collect()
}

fn part_label(part: &PartEntry) -> String {
    match &part.kind {
        PartKind::PadClips { pad } => format!("Pad {pad:02}"),
        PartKind::InstrumentNotes { .. } => format!("Part {} note", part.id.0),
        PartKind::SampleClips { .. } => format!("Part {} take audio", part.id.0),
        PartKind::LiveInstrumentNotes { .. } => format!("Part {} live note", part.id.0),
    }
}

/// Names for the whole manifest. Takes are numbered by first appearance and
/// hits by manifest order, so the suffix stays unique even when a bundle
/// reuses a 128-bit take identity or the same pad appears in separate parts.
fn identity_names(manifest: &Manifest) -> Result<BTreeMap<EventId, String>> {
    let mut take_ordinals: BTreeMap<u128, u64> = BTreeMap::new();
    let mut hit_count: BTreeMap<u128, u64> = BTreeMap::new();
    let mut names = BTreeMap::new();
    for event in &manifest.events {
        let take = match take_ordinals.get(&event.id.take) {
            Some(ordinal) => *ordinal,
            None => {
                let ordinal = take_ordinals.len() as u64 + 1;
                take_ordinals.insert(event.id.take, ordinal);
                ordinal
            }
        };
        let hit = hit_count.entry(event.id.take).or_insert(0);
        *hit += 1;
        let part = manifest
            .part(event.part)
            .ok_or_else(|| invalid("missing part"))?;
        names.insert(event.id, identity_name(&part_label(part), take, *hit)?);
    }
    Ok(names)
}

fn build(
    app: &mut process::App,
    stage: &Path,
    input: &Input,
    mappings: &[SamplerMapping],
    report: &mut Report,
) -> Result<()> {
    let manifest = &input.manifest;
    let names = identity_names(manifest)?;
    app.call(
        "session.new",
        json!({"name": "Brownnote handoff", "sample_rate": RATE}),
    )?;
    app.call(
        "event.tempo",
        json!({"bpm": f64::from(manifest.timing.tempo_milli_bpm) / 1000.0, "at": 0}),
    )?;
    if manifest.master_gain != 1.0 {
        let db = master_db(manifest.master_gain);
        let floor = if manifest.master_gain <= 0.0 {
            " The reference is mathematically silent; the target's lowest fader setting is -144 dB."
        } else {
            ""
        };
        report.record(
            None,
            None,
            "conversion",
            format!(
                "Static master gain {} ({} dB) applied once to every imported pre-master clip track; \
                 hit assets and per-hit gain/velocity/pan stay untouched, so master is not applied \
                 twice.{}",
                manifest.master_gain, db, floor
            ),
        );
    }
    for event in &manifest.events {
        if let Some(asset) = event.asset {
            let (start, lost) = rounded(event.start_nanos, RATE, 1_000_000_000)?;
            let length = input.media[&asset].frames;
            start
                .checked_add(length)
                .ok_or_else(|| invalid("audio end overflows target time"))?;
            let reply = app.call(
                "file.import_audio",
                json!({"path": stage.join(asset.relative_path()), "at": start, "new_tracks": true}),
            )?;
            let imported = &reply["imported"][0];
            let track = number(&imported["track"], "audio import")?;
            let clip = number(&imported["clip"], "audio import")?;
            if imported["frames"].as_i64() != Some(length) {
                return Err(invalid("audio import changed expected frame count"));
            }
            if manifest.master_gain != 1.0 {
                app.call(
                    "mix.volume",
                    json!({"tracks": [track], "db": master_db(manifest.master_gain)}),
                )?;
            }
            let name = names
                .get(&event.id)
                .ok_or_else(|| invalid("missing imported event identity"))?
                .clone();
            app.call("track.rename", json!({"track": track, "name": name}))?;
            app.call("clip.rename", json!({"clip": clip, "name": name}))?;
            aligned_with_bundle(manifest, event.id, asset, start)?;
            report.audio.push(AudioClip {
                event: event.id,
                track,
                clip,
                name,
                start,
                length,
            });
            if lost {
                report.record(
                    Some(event.id),
                    Some(event.part),
                    "conversion",
                    "Audio onset rounded to nearest 48 kHz sample.",
                );
            }
            let (requested_length, _) = rounded(event.duration_nanos, RATE, 1_000_000_000)?;
            if requested_length != length {
                report.record(Some(event.id), Some(event.part), "unsupported", "Full media length retained, not trimmed to event duration; bake choke/envelope/tails into per-hit assets upstream.");
            }
            report.record(Some(event.id), Some(event.part), "fallback", "Independent full-length audio clip retained; no sampler, plugin, choke, gain, pan or pitch parity inferred.");
        }
    }
    if manifest
        .audio
        .iter()
        .any(|entry| entry.role != AudioRole::HitWet)
    {
        report.record(None, None, "fallback", "Origin-aligned dry hits, stems and the reference mix stay in the retained bundle media/ for comparison; they are not imported as tracks.");
    }
    let mut seen = BTreeSet::new();
    for mapping in mappings {
        if !seen.insert(mapping.part) {
            return Err(invalid("duplicate sampler mapping for part"));
        }
        if !matches!(
            manifest.part(mapping.part).map(|p| &p.kind),
            Some(PartKind::InstrumentNotes { .. })
        ) || !input.media.contains_key(&mapping.asset)
        {
            return Err(invalid(
                "sampler mapping needs an instrument part and a bundle asset",
            ));
        }
    }
    for part in &manifest.parts {
        if let PartKind::LiveInstrumentNotes { plugin } = &part.kind {
            // Never a silent drop and never a sampler guess: this target cannot
            // load a plugin from a bundle or carry its identity into a track.
            let notes = manifest
                .events
                .iter()
                .filter(|event| event.part == part.id)
                .count();
            report.record(
                None,
                Some(part.id),
                "unsupported",
                format!(
                    "{notes} live instrument note(s) kept in the manifest for {}; this target \
                     cannot load that plugin or carry its identity into a track, so the notes are \
                     not imported as MIDI. The take's retained audio is the fallback.",
                    plugin.describe()
                ),
            );
            continue;
        }
        if !matches!(part.kind, PartKind::InstrumentNotes { .. }) {
            continue;
        }
        let events: Vec<_> = manifest
            .events
            .iter()
            .filter(|event| event.part == part.id)
            .collect();
        if events.is_empty() {
            continue;
        }
        let mut notes = Vec::new();
        for event in events {
            let (start, length, loss) = midi_gate(
                event.start_nanos,
                event.duration_nanos,
                u64::from(manifest.timing.tempo_milli_bpm),
            )?;
            notes.push(Note {
                start,
                length,
                pitch: event.pitch.ok_or_else(|| invalid("missing pitch"))?,
                channel: event.channel.ok_or_else(|| invalid("missing channel"))?,
                velocity: event.velocity.ok_or_else(|| invalid("missing velocity"))?,
            });
            if loss {
                report.record(
                    Some(event.id),
                    Some(part.id),
                    "conversion",
                    "MIDI onset/end rounded to nearest 960 PPQ tick with a one-tick minimum gate, matching notes.mid.",
                );
            }
            if event.asset.is_none() {
                report.record(Some(event.id), Some(part.id), "unsupported", "No retained audio for this instrument note; editable MIDI alone does not preserve its sound.");
            }
        }
        notes.sort();
        let end = notes.iter().try_fold(0i64, |end, note| {
            note.start
                .checked_add(note.length)
                .map(|x| end.max(x))
                .ok_or_else(|| invalid("MIDI end overflows"))
        })?;
        let samples = (u128::try_from(end).map_err(|_| invalid("negative MIDI end"))?
            * 60_000
            * u128::from(RATE))
        .div_ceil(u128::from(manifest.timing.tempo_milli_bpm) * u128::from(midi::PPQ));
        let samples =
            i64::try_from(samples + 1).map_err(|_| invalid("MIDI clip length overflows"))?;
        let midi_name = bounded_name(&format!("Part {} · MIDI · Editable, muted", part.id.0));
        let reply = app.call(
            "track.new",
            json!({"kind": "instrument", "name": midi_name}),
        )?;
        let track = number(&reply["tracks"][0], "MIDI track")?;
        app.call("mix.mute", json!({"tracks": [track], "value": true}))?;
        let reply = app.call(
            "midi.new_clip",
            json!({"track": track, "start": 0, "length": samples}),
        )?;
        let clip = number(&reply["clip"], "MIDI clip")?;
        for note in &notes {
            app.call("midi.note_add", json!({"clip": clip, "pitch": note.pitch, "channel": note.channel, "velocity": note.velocity, "start_ticks": note.start, "length_ticks": note.length}))?;
        }
        if let Some(mapping) = mappings.iter().find(|mapping| mapping.part == part.id) {
            if mapping.choke_group.is_some()
                || mapping.key_low > mapping.key_high
                || mapping.root_note > 127
                || mapping.key_high > 127
            {
                report.record(None, Some(part.id), "unsupported", "Sampler mapping not applied: no choke_group support, or invalid MIDI root/key range. Retained audio and muted MIDI remain.");
            } else {
                app.call("mix.sampler_load", json!({
                    "track": track, "path": stage.join(mapping.asset.relative_path()),
                    "params": {"root_note": mapping.root_note, "key_low": mapping.key_low, "key_high": mapping.key_high,
                    "key_track": u8::from(mapping.key_track), "play_mode": u8::from(mapping.one_shot)}
                }))?;
                report.record(None, Some(part.id), "approximation", "Explicit single-sample mapping applied; muted to avoid doubling fallback clips. Sampler envelopes/interpolation/retriggers are not Brownnote-equivalent.");
            }
        } else {
            report.record(None, Some(part.id), "unsupported", "Manifest v1 has no sampler root/range/play-mode or plugin state. No mapping inferred; target default instrument stays muted.");
        }
        report.midi.push(MidiPart {
            part: part.id,
            track,
            clip,
            name: midi_name,
            notes,
        });
    }
    Ok(())
}

/// The #109 writer already aligned this hit; disagreeing would silently move it.
fn aligned_with_bundle(
    manifest: &Manifest,
    event: EventId,
    asset: AssetId,
    start: i64,
) -> Result<()> {
    let Some(hit) = manifest
        .audio
        .iter()
        .find(|entry| entry.role == AudioRole::HitWet && entry.event == Some(event))
    else {
        return Ok(());
    };
    let rate = u128::from(manifest.timing.sample_rate_hz);
    let expected = (u128::from(hit.start_frame) * u128::from(RATE) + rate / 2) / rate;
    let expected = i64::try_from(expected).map_err(|_| invalid("bundle hit overflows"))?;
    if hit.asset != asset || (expected - start).abs() > 1 {
        return Err(invalid(format!(
            "bundle HitWet entry for event {}:{} disagrees with its event asset/onset",
            event.take, event.sequence
        )));
    }
    Ok(())
}

fn verify(
    app: &mut process::App,
    stage: &Path,
    manifest: &Manifest,
    report: &Report,
) -> Result<()> {
    let state = app.call("session.inspect", json!({"detail": "full"}))?;
    if state["sample_rate"] != RATE
        || state["tempo"]
            != json!([{"bpm": f64::from(manifest.timing.tempo_milli_bpm) / 1000.0, "tick": 0}])
    {
        return Err(invalid("saved tempo/sample rate differs"));
    }
    let tracks = state["tracks"]
        .as_array()
        .ok_or_else(|| invalid("inspection missing tracks"))?;
    if tracks.len() != report.audio.len() + report.midi.len() {
        return Err(invalid("saved track count differs"));
    }
    let mut audio_tracks = BTreeSet::new();
    for audio in &report.audio {
        if !audio_tracks.insert(audio.track) {
            return Err(invalid("audio hits share a destructive track"));
        }
        let track = tracks
            .iter()
            .find(|t| t["id"] == audio.track)
            .ok_or_else(|| invalid("missing audio track"))?;
        if track["name"].as_str() != Some(audio.name.as_str()) {
            return Err(invalid("saved audio track lost its imported name"));
        }
        let clips = track["clips"]
            .as_array()
            .ok_or_else(|| invalid("missing audio clips"))?;
        if clips.len() != 1
            || clips[0]["id"] != audio.clip
            || clips[0]["name"].as_str() != Some(audio.name.as_str())
            || clips[0]["start"] != audio.start
            || clips[0]["length"] != audio.length
        {
            return Err(invalid(
                "saved audio clip count/identity/position/length differs",
            ));
        }
        let expected_db = if manifest.master_gain == 1.0 {
            0.0
        } else {
            master_db(manifest.master_gain)
        };
        let actual_db = track["volume_db"]
            .as_f64()
            .ok_or_else(|| invalid("saved audio track has no volume"))?;
        if (actual_db - expected_db).abs() > 1e-6 {
            return Err(invalid("saved audio track lost the static master gain"));
        }
    }
    for midi in &report.midi {
        let track = tracks
            .iter()
            .find(|t| t["id"] == midi.track)
            .ok_or_else(|| invalid("missing MIDI track"))?;
        let clips = track["clips"]
            .as_array()
            .ok_or_else(|| invalid("missing MIDI clips"))?;
        if track["mute"] != true
            || track["name"].as_str() != Some(midi.name.as_str())
            || clips.len() != 1
            || clips[0]["id"] != midi.clip
            || clips[0]["start"] != 0
        {
            return Err(invalid("saved MIDI clip differs"));
        }
        let reply = app.call("midi.notes", json!({"clip": midi.clip}))?;
        let mut notes: Vec<Note> = serde_json::from_value(reply["notes"].clone())
            .map_err(|error| invalid(error.to_string()))?;
        notes.sort();
        if notes != midi.notes {
            return Err(invalid("saved MIDI notes differ"));
        }
    }
    let sources = state["sources"]
        .as_array()
        .ok_or_else(|| invalid("inspection missing sources"))?;
    if !report.audio.is_empty() && sources.is_empty() {
        return Err(invalid("inspection lost audio sources"));
    }
    for source in sources {
        let path = source["path"]
            .as_str()
            .ok_or_else(|| invalid("source missing path"))?;
        let rel = Path::new(path);
        if source["loaded"] != true
            || !path.starts_with("Audio Files/")
            || rel
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || !stage.join(rel).is_file()
        {
            return Err(invalid(
                "source is unavailable or not relocatable Audio Files/ media",
            ));
        }
    }
    if app.call("session.audio_health", json!({}))?["healthy"] != true {
        return Err(invalid("saved audio health check failed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_name_truncates_long_prefixes_before_keeping_the_unique_suffix() {
        let prefix = "Pad 01 · a deliberately long part label ".repeat(4);
        let name = identity_name(&prefix, 12, 345).expect("naming");
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);
        assert!(name.ends_with(" · Take 12 · Hit 345"), "{name}");
        let bounded = identity_name("Pad 03", 1, 1).expect("naming");
        assert_eq!(bounded, "Pad 03 · Take 1 · Hit 1");
        assert!(
            identity_name("x", u64::MAX, u64::MAX)
                .expect("naming")
                .chars()
                .count()
                <= MAX_NAME_CHARS
        );
    }

    #[test]
    fn identity_names_are_unique_across_parts_takes_and_large_take_ids() {
        let mut manifest = Manifest::new(
            crate::export::manifest::TimeOrigin::default(),
            crate::export::manifest::Timing {
                tempo_milli_bpm: 120_000,
                sample_rate_hz: 48_000,
            },
        );
        manifest.parts = vec![
            PartEntry {
                id: PartId(1),
                kind: PartKind::PadClips { pad: 1 },
            },
            PartEntry {
                id: PartId(2),
                kind: PartKind::PadClips { pad: 1 },
            },
        ];
        for (take, sequence) in [(u128::from(u64::MAX) + 1, 0), (7, 0), (7, 1)] {
            manifest.events.push(crate::export::manifest::EventEntry {
                id: EventId { take, sequence },
                part: PartId(sequence + 1),
                start_nanos: 0,
                duration_nanos: 1,
                pad: Some(1),
                pitch: None,
                channel: None,
                velocity: None,
                asset: None,
                source: None,
                variant_of: None,
            });
        }
        let names = identity_names(&manifest).expect("naming");
        let unique: BTreeSet<_> = names.values().collect();
        assert_eq!(unique.len(), manifest.events.len());
        assert!(
            names
                .values()
                .all(|name| name.chars().count() <= MAX_NAME_CHARS)
        );
        assert_eq!(names[&manifest.events[0].id], "Pad 01 · Take 1 · Hit 1");
        assert_eq!(names[&manifest.events[1].id], "Pad 01 · Take 2 · Hit 1");
        assert_eq!(names[&manifest.events[2].id], "Pad 01 · Take 2 · Hit 2");
    }
}
