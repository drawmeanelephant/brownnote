//! Per-hit float32 stereo WAVs and origin-aligned fallbacks, with no cache or
//! plugin access. Source and retained encoded bytes are copied unchanged too.
//!
//! Pad playback follows the native mixer: stereo sources downmix to mono,
//! linear interpolation, rate * 2^(semitones/12), linear velocity/127 * gain,
//! constant-power pan, and a 5 ms choke fade shared by source identity.
//! Ordinary pads are one-shots; a captured key release does not trim their
//! tails. Frozen per-event renders are already audible stereo, including
//! pitch/choke/processing/tails, and are never processed a second time.
//!
//! Absolute onsets round once to nearest output frame, ties later (<= 0.5
//! frame). Every stem and the reference starts at frame zero of TimeOrigin
//! and has the same length, including tails. Stems are unclamped float audio;
//! the reference applies master gain then the native final [-1,1] clamp,
//! without normalization/tanh. Hit entries belong on separate DAW tracks.

use super::{
    fixture::FixtureExport,
    manifest::{
        AssetEntry, AssetId, AssetKind, AudioEntry, AudioRole, EventId, LossRecord, Manifest,
        PartId, PartKind,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Cursor,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PadSettings {
    pub gain: f32,
    pub pan: f32,
    pub semitones: f32,
}
impl Default for PadSettings {
    fn default() -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
            semitones: 0.0,
        }
    }
}

/// Authoritative project media, not a cache path. All hit-local processing
/// must already be baked; `dry` is optional only when it cannot be recovered.
#[derive(Clone, Debug, PartialEq)]
pub struct FrozenHit {
    pub wet: AssetId,
    pub dry: Option<AssetId>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioSettings {
    pub pads: BTreeMap<u8, PadSettings>,
    pub frozen: BTreeMap<EventId, FrozenHit>,
    pub master_gain: f32,
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            pads: BTreeMap::new(),
            frozen: BTreeMap::new(),
            master_gain: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioError {
    InvalidSettings,
    InvalidWav(AssetId),
    MissingAsset(AssetId),
    MissingHit(EventId),
    UnknownFrozenEvent(EventId),
    NotRetained(AssetId),
    TooLong,
    Encode,
}
impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "export audio: {self:?}")
    }
}
impl std::error::Error for AudioError {}

struct Decoded {
    rate: u32,
    frames: Vec<[f32; 2]>,
}

fn decode(id: AssetId, bytes: &[u8]) -> Result<Decoded, AudioError> {
    let invalid = || AudioError::InvalidWav(id);
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|_| invalid())?;
    let spec = reader.spec();
    if !matches!(spec.channels, 1 | 2)
        || !(8_000..=192_000).contains(&spec.sample_rate)
        || !matches!(
            (spec.sample_format, spec.bits_per_sample),
            (hound::SampleFormat::Int, 16) | (hound::SampleFormat::Float, 32)
        )
    {
        return Err(invalid());
    }
    let values: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|s| f32::from(s) / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|_| invalid())?,
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|_| invalid())?,
    };
    if values.is_empty()
        || !values.len().is_multiple_of(usize::from(spec.channels))
        || values.iter().any(|v| !v.is_finite())
        || values.len() as u64 != u64::from(reader.duration()) * u64::from(spec.channels)
    {
        return Err(invalid());
    }
    let frames = values
        .chunks_exact(usize::from(spec.channels))
        .map(|f| [f[0], f[usize::from(spec.channels) - 1]])
        .collect();
    Ok(Decoded {
        rate: spec.sample_rate,
        frames,
    })
}

fn silence(frames: usize) -> Result<Vec<[f32; 2]>, AudioError> {
    // RIFF's data chunk and containing file must both fit 32-bit sizes.
    if frames as u64 * 8 > u64::from(u32::MAX) - 128 {
        return Err(AudioError::TooLong);
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(frames)
        .map_err(|_| AudioError::TooLong)?;
    result.resize(frames, [0.0; 2]);
    Ok(result)
}

fn wav(frames: &[[f32; 2]], rate: u32) -> Result<Vec<u8>, AudioError> {
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
        hound::WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .map_err(|_| AudioError::Encode)?;
    for value in frames.iter().flatten() {
        if !value.is_finite() {
            return Err(AudioError::Encode);
        }
        writer
            .write_sample(*value)
            .map_err(|_| AudioError::Encode)?;
    }
    writer.finalize().map_err(|_| AudioError::Encode)?;
    Ok(bytes.into_inner())
}

pub fn frame_at(nanos: u64, rate: u32) -> Result<u64, AudioError> {
    u64::try_from((u128::from(nanos) * u128::from(rate) + 500_000_000) / 1_000_000_000)
        .map_err(|_| AudioError::TooLong)
}

fn render_hit(
    sample: &Decoded,
    rate: u32,
    settings: PadSettings,
    velocity: f32,
    choke: Option<u64>,
    frozen: bool,
) -> Result<Vec<[f32; 2]>, AudioError> {
    let step = f64::from(sample.rate) / f64::from(rate)
        * 2.0_f64.powf(f64::from(settings.semitones) / 12.0);
    let natural = (sample.frames.len() as f64 / step).ceil() as u64;
    let fade = (f64::from(rate) * 0.005).ceil() as u64;
    let length = choke.map_or(natural, |at| natural.min(at.saturating_add(fade)));
    let mut out = silence(usize::try_from(length).map_err(|_| AudioError::TooLong)?)?;
    let theta = (settings.pan + 1.0) * std::f32::consts::FRAC_PI_4;
    let gains = [
        theta.cos() * velocity * settings.gain,
        theta.sin() * velocity * settings.gain,
    ];
    let mut position = 0.0_f64;
    for (i, frame) in out.iter_mut().enumerate() {
        let index = (position as usize).min(sample.frames.len() - 1);
        let next = (index + 1).min(sample.frames.len() - 1);
        let envelope = choke
            .filter(|at| i as u64 >= *at)
            .map_or(1.0, |at| (fade - (i as u64 - at)) as f32 / fade as f32);
        for channel in 0..2 {
            let value = if frozen {
                let a = f64::from(sample.frames[index][channel]);
                a + (f64::from(sample.frames[next][channel]) - a) * position.fract()
            } else {
                let a = sample.frames[index][0] / 2.0 + sample.frames[index][1] / 2.0;
                let b = sample.frames[next][0] / 2.0 + sample.frames[next][1] / 2.0;
                f64::from(a) + (f64::from(b) - f64::from(a)) * position.fract()
            };
            frame[channel] = (value
                * f64::from(if frozen { 1.0 } else { gains[channel] })
                * f64::from(envelope)) as f32;
        }
        position += step;
    }
    Ok(out)
}

struct Hit {
    id: EventId,
    part: PartId,
    start: usize,
    wet: Vec<[f32; 2]>,
    dry: Option<Vec<[f32; 2]>>,
    detail: String,
}

fn add(
    manifest: &mut Manifest,
    generated: &mut BTreeMap<AssetId, Vec<u8>>,
    frames: &[[f32; 2]],
    role: AudioRole,
    part: Option<PartId>,
    event: Option<EventId>,
    start_frame: u64,
) -> Result<AssetId, AudioError> {
    let bytes = wav(frames, manifest.timing.sample_rate_hz)?;
    let id = AssetId::of(&bytes);
    if manifest.asset(id).is_none() {
        manifest.assets.push(AssetEntry {
            id,
            kind: AssetKind::RetainedRender,
            bytes: bytes.len() as u64,
            path: id.relative_path(),
        });
        generated.insert(id, bytes);
    }
    manifest.audio.push(AudioEntry {
        role,
        part,
        event,
        asset: id,
        start_frame,
        frames: frames.len() as u64,
    });
    Ok(id)
}

/// Build render bytes and manifest joins before the transaction can commit.
/// `media` contains only verified project-owned bytes, never render-cache data.
pub fn render(
    manifest: &mut Manifest,
    export: &FixtureExport,
    media: &BTreeMap<AssetId, Vec<u8>>,
    settings: &AudioSettings,
) -> Result<BTreeMap<AssetId, Vec<u8>>, AudioError> {
    let rate = manifest.timing.sample_rate_hz;
    if !(8_000..=192_000).contains(&rate)
        || !settings.master_gain.is_finite()
        || !(0.0..=1.0).contains(&settings.master_gain)
        || settings.pads.values().any(|p| {
            !p.gain.is_finite()
                || !(0.0..=1.5).contains(&p.gain)
                || !p.pan.is_finite()
                || !(-1.0..=1.0).contains(&p.pan)
                || !p.semitones.is_finite()
                || !(-12.0..=12.0).contains(&p.semitones)
        })
    {
        return Err(AudioError::InvalidSettings);
    }
    for (id, frozen) in &settings.frozen {
        if manifest.event(*id).is_none() {
            return Err(AudioError::UnknownFrozenEvent(*id));
        }
        for asset in [Some(frozen.wet), frozen.dry].into_iter().flatten() {
            if manifest
                .asset(asset)
                .is_none_or(|a| a.kind != AssetKind::RetainedRender)
            {
                return Err(AudioError::NotRetained(asset));
            }
        }
    }
    let decoded = media
        .iter()
        .map(|(id, bytes)| Ok((*id, decode(*id, bytes)?)))
        .collect::<Result<BTreeMap<_, _>, AudioError>>()?;
    let per_hit: BTreeSet<_> = export
        .takes
        .iter()
        .flat_map(|take| &take.events)
        .filter(|event| event.asset.is_some())
        .map(|event| event.id)
        .collect();
    // Choke follows source identity, including pads sharing an assignment.
    let group = |event: &super::manifest::EventEntry| {
        event
            .pad
            .and_then(|pad| export.pads.iter().find(|p| p.id == pad))
            .filter(|pad| pad.choke)
            .and_then(|pad| pad.source)
    };
    let mut ordered: Vec<_> = manifest.events.iter().collect();
    ordered.sort_by_key(|event| (event.start_nanos, event.id));
    let mut next_by_group = BTreeMap::new();
    let mut chokes = BTreeMap::new();
    for event in ordered.into_iter().rev() {
        if let Some(group) = group(event) {
            let start = frame_at(event.start_nanos, rate)?;
            if let Some(next) = next_by_group.insert(group, start) {
                chokes.insert(event.id, next - start);
            }
        }
    }
    let mut hits = Vec::new();
    let mut total = 1_usize;
    for event in &manifest.events {
        let start =
            usize::try_from(frame_at(event.start_nanos, rate)?).map_err(|_| AudioError::TooLong)?;
        let end = event
            .start_nanos
            .checked_add(event.duration_nanos)
            .ok_or(AudioError::TooLong)?;
        total = total.max(usize::try_from(frame_at(end, rate)?).map_err(|_| AudioError::TooLong)?);
        let frozen = settings.frozen.get(&event.id);
        let instrument = manifest
            .part(event.part)
            .is_some_and(|p| matches!(p.kind, PartKind::InstrumentNotes { .. }));
        // A live take's notes have no per-hit render by design: the take's own
        // retained audio, exported as its clip part, is the audio for this
        // performance. Nothing is lost, so nothing is recorded as a loss.
        if manifest
            .part(event.part)
            .is_some_and(|p| matches!(p.kind, PartKind::LiveInstrumentNotes { .. }))
        {
            continue;
        }
        if instrument && frozen.is_none() && event.asset.is_none() {
            manifest.losses.push(LossRecord::UnsupportedMapping {
                event: event.id,
                reason: "instrument has editable MIDI only; no frozen project render, omitted from audio fallbacks".into(),
            });
            continue;
        }
        let wet_id = frozen
            .map(|f| f.wet)
            .or(event.asset)
            .ok_or(AudioError::MissingHit(event.id))?;
        let authoritative = frozen.is_some() || per_hit.contains(&event.id);
        if authoritative
            && manifest
                .asset(wet_id)
                .is_none_or(|a| a.kind != AssetKind::RetainedRender)
        {
            return Err(AudioError::NotRetained(wet_id));
        }
        let dry_id = if let Some(frozen) = frozen {
            frozen.dry
        } else if authoritative {
            None
        } else {
            event.source
        };
        let pad_settings = event
            .pad
            .and_then(|pad| settings.pads.get(&pad))
            .copied()
            .unwrap_or_default();
        let velocity = f32::from(event.velocity.unwrap_or(127)) / 127.0;
        let choke = if authoritative {
            None
        } else {
            chokes.get(&event.id).copied()
        };
        let render = |id| {
            render_hit(
                decoded.get(&id).ok_or(AudioError::MissingAsset(id))?,
                rate,
                if authoritative {
                    PadSettings::default()
                } else {
                    pad_settings
                },
                velocity,
                choke,
                authoritative,
            )
        };
        let wet = render(wet_id)?;
        let dry = dry_id.map(render).transpose()?;
        total = total.max(start.checked_add(wet.len()).ok_or(AudioError::TooLong)?);
        if let Some(dry) = &dry {
            total = total.max(start.checked_add(dry.len()).ok_or(AudioError::TooLong)?);
        } else {
            manifest.losses.push(LossRecord::UnsupportedMapping {
                event: event.id,
                reason: "no recoverable dry render; dry stem omits this hit".into(),
            });
        }
        let detail = if authoritative {
            format!(
                "authoritative per-hit render; only resampled to {rate} Hz, \
                no second pitch/gain/pan/velocity/choke pass; hit/stems pre-master, \
                reference master {} and final clamp",
                settings.master_gain
            )
        } else {
            format!(
                "native one-shot: pitch {} semitones, gain {}, pan {}, \
                velocity {}, choke starts at local frame {:?} with {}-frame fade; \
                linear interpolation to {rate} Hz; hit/stems pre-master, reference master {} and final clamp",
                pad_settings.semitones,
                pad_settings.gain,
                pad_settings.pan,
                event.velocity.unwrap_or(127),
                choke,
                (f64::from(rate) * 0.005).ceil(),
                settings.master_gain
            )
        };
        hits.push(Hit {
            id: event.id,
            part: event.part,
            start,
            wet,
            dry,
            detail,
        });
        manifest.losses.push(LossRecord::ConversionLoss {
            event: event.id,
            detail: "float32 stereo hit; nearest output-frame onset (ties later, <= 0.5 frame); native linear interpolation, gain/velocity/pan and source-identity 5 ms choke baked, or authoritative frozen hit resampled without reprocessing; offline mix preserves all hits rather than live finite-voice-pool stealing".into(),
        });
        if let Some(frozen) = frozen {
            manifest.losses.push(LossRecord::BakedProcessing {
                part: event.part,
                asset: frozen.wet,
                detail: frozen.detail.clone(),
            });
        }
    }
    let mut generated = BTreeMap::new();
    let mut reference = silence(total)?;
    for hit in &hits {
        let wet_id = add(
            manifest,
            &mut generated,
            &hit.wet,
            AudioRole::HitWet,
            Some(hit.part),
            Some(hit.id),
            hit.start as u64,
        )?;
        if let Some(event) = manifest.events.iter_mut().find(|event| event.id == hit.id) {
            event.asset = Some(wet_id);
        }
        manifest.losses.push(LossRecord::BakedProcessing {
            part: hit.part,
            asset: wet_id,
            detail: hit.detail.clone(),
        });
        if let Some(dry) = &hit.dry {
            add(
                manifest,
                &mut generated,
                dry,
                AudioRole::HitDry,
                Some(hit.part),
                Some(hit.id),
                hit.start as u64,
            )?;
        }
    }
    let parts: Vec<_> = manifest.parts.iter().map(|p| p.id).collect();
    for part in parts {
        let part_hits: Vec<_> = hits.iter().filter(|h| h.part == part).collect();
        if part_hits.is_empty() {
            continue;
        }
        let mut wet = silence(total)?;
        let mut dry = silence(total)?;
        for hit in part_hits {
            mix(&mut wet, &hit.wet, hit.start);
            if let Some(frames) = &hit.dry {
                mix(&mut dry, frames, hit.start);
            }
        }
        mix(&mut reference, &wet, 0);
        add(
            manifest,
            &mut generated,
            &wet,
            AudioRole::StemWet,
            Some(part),
            None,
            0,
        )?;
        add(
            manifest,
            &mut generated,
            &dry,
            AudioRole::StemDry,
            Some(part),
            None,
            0,
        )?;
    }
    for value in reference.iter_mut().flatten() {
        *value = (*value * settings.master_gain).clamp(-1.0, 1.0);
    }
    add(
        manifest,
        &mut generated,
        &reference,
        AudioRole::Reference,
        None,
        None,
        0,
    )?;
    manifest.assets.sort_by_key(|a| a.id);
    Ok(generated)
}

fn mix(destination: &mut [[f32; 2]], source: &[[f32; 2]], start: usize) {
    for (out, frame) in destination[start..].iter_mut().zip(source) {
        for channel in 0..2 {
            out[channel] += frame[channel];
        }
    }
}
