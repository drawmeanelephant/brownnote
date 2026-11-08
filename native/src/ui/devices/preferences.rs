//! Machine-local output preference. The caller supplies the machine root;
//! this module never discovers a home directory or touches project documents.
use brownnote_native::output::OutputSelection;
use cpal::BufferSize;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

const FILE: &str = "audio-output.json";
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    version: u32,
    device_id: Option<String>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    buffer_frames: Option<u32>,
}

pub fn load(root: &Path) -> Result<OutputSelection, String> {
    let file = match File::open(root.join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OutputSelection::default());
        }
        Err(_) => return Err("Cannot read machine output preferences".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read machine output preferences")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Machine output preferences are too large".into());
    }
    let saved: Preference = serde_json::from_slice(&bytes)
        .map_err(|_| "Corrupt machine output preferences; choose an output explicitly")?;
    if saved.version != 1 {
        return Err("Unsupported machine output preference version".into());
    }
    let device_id = saved
        .device_id
        .map(|id| {
            if id.split_once(':').is_none_or(|(_, value)| value.is_empty()) {
                return Err("Invalid saved output device ID");
            }
            id.parse().map_err(|_| "Invalid saved output device ID")
        })
        .transpose()?;
    if saved.sample_rate == Some(0) || saved.channels == Some(0) || saved.buffer_frames == Some(0) {
        return Err("Invalid saved output configuration".into());
    }
    Ok(OutputSelection {
        device_id,
        sample_rate: saved.sample_rate,
        channels: saved.channels,
        buffer_size: saved
            .buffer_frames
            .map_or(BufferSize::Default, BufferSize::Fixed),
    })
}

pub fn save(root: &Path, selection: &OutputSelection) -> Result<(), String> {
    let saved = Preference {
        version: 1,
        device_id: selection.device_id.as_ref().map(ToString::to_string),
        sample_rate: selection.sample_rate,
        channels: selection.channels,
        buffer_frames: match selection.buffer_size {
            BufferSize::Default => None,
            BufferSize::Fixed(frames) => Some(frames),
        },
    };
    let bytes = serde_json::to_vec_pretty(&saved)
        .map_err(|_| "Cannot encode machine output preferences")?;
    // One app-side writer. A failed write leaves the last complete file intact.
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(root)?;
        let temporary = root.join("audio-output.json.tmp");
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(temporary, root.join(FILE))?;
        File::open(root)?.sync_all()
    };
    write().map_err(|_| "Output choice is active in memory but could not be saved".into())
}
