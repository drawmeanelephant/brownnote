//! IEEE float WAV writer. The checker decodes these with its own reader.

use std::io::Write;
use std::path::Path;

pub fn write_f32(path: &Path, sample_rate: u32, channels: &[Vec<f32>]) -> std::io::Result<()> {
    let ch = channels.len() as u16;
    let frames = channels.first().map_or(0, Vec::len);
    let data_len = (frames * channels.len() * 4) as u32;
    let mut b = Vec::with_capacity(data_len as usize + 44);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    b.extend_from_slice(&ch.to_le_bytes());
    b.extend_from_slice(&sample_rate.to_le_bytes());
    b.extend_from_slice(&(sample_rate * u32::from(ch) * 4).to_le_bytes());
    b.extend_from_slice(&(ch * 4).to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for c in channels {
            b.extend_from_slice(&c[i].to_le_bytes());
        }
    }
    std::fs::File::create(path)?.write_all(&b)
}
