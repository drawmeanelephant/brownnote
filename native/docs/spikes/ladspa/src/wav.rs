//! Just enough RIFF/WAVE for the probe: read PCM16 or float32 input, write
//! float32 output. `verify.py` decodes the output with its own parser.

use std::path::Path;

pub struct Audio {
    pub rate: u32,
    /// Planar samples, one vector per channel.
    pub channels: Vec<Vec<f32>>,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

pub fn read(path: &Path) -> Result<Audio, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let bad = |why: &str| format!("unsupported WAV {}: {why}", path.display());
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(bad("not RIFF/WAVE"));
    }
    let mut at = 12;
    let mut format = None;
    let mut data = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(&bytes, at + 4).ok_or_else(|| bad("truncated chunk"))? as usize;
        let body = bytes
            .get(at + 8..at + 8 + size)
            .ok_or_else(|| bad("chunk past end of file"))?;
        match id {
            b"fmt " => {
                let tag = u16_at(body, 0).ok_or_else(|| bad("short fmt"))?;
                let channels = u16_at(body, 2).ok_or_else(|| bad("short fmt"))?;
                let rate = u32_at(body, 4).ok_or_else(|| bad("short fmt"))?;
                let bits = u16_at(body, 14).ok_or_else(|| bad("short fmt"))?;
                format = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    let (tag, channel_count, rate, bits) = format.ok_or_else(|| bad("no fmt chunk"))?;
    let data = data.ok_or_else(|| bad("no data chunk"))?;
    if !(1..=2).contains(&channel_count) {
        return Err(bad("only mono or stereo input"));
    }
    if !(8_000..=192_000).contains(&rate) {
        return Err(bad("sample rate outside 8-192 kHz"));
    }
    let width = match (tag, bits) {
        (1, 16) => 2,
        (3, 32) => 4,
        _ => return Err(bad("only PCM16 or float32")),
    };
    let channel_count = usize::from(channel_count);
    let frame = width * channel_count;
    let mut channels = vec![Vec::with_capacity(data.len() / frame); channel_count];
    for chunk in data.chunks_exact(frame) {
        for (channel, sample) in chunk.chunks_exact(width).enumerate() {
            let value = if width == 2 {
                f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0
            } else {
                f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]])
            };
            if !value.is_finite() {
                return Err(bad("non-finite input sample"));
            }
            channels[channel].push(value);
        }
    }
    Ok(Audio { rate, channels })
}

/// Writes IEEE float32 WAV with a `fact` chunk, as required for non-PCM data.
pub fn write_float(path: &Path, audio: &Audio) -> std::io::Result<()> {
    let channels = audio.channels.len();
    let frames = audio.channels.first().map_or(0, Vec::len);
    let data_len = (frames * channels * 4) as u32;
    let mut out = Vec::with_capacity(58 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(4 + 26 + 12 + 8 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&18u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&(channels as u16).to_le_bytes());
    out.extend_from_slice(&audio.rate.to_le_bytes());
    out.extend_from_slice(&(audio.rate * channels as u32 * 4).to_le_bytes());
    out.extend_from_slice(&(channels as u16 * 4).to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(frames as u32).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for frame in 0..frames {
        for channel in &audio.channels {
            out.extend_from_slice(&channel[frame].to_le_bytes());
        }
    }
    std::fs::write(path, out)
}
