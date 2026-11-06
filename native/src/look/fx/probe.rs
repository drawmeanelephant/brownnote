//! Frame timing for measurement runs: CPU per frame (Rune's update and this compositor's encode),
//! GPU per frame from timestamps on two passes that bracket the frame, and the frames on which
//! rune-fx compiled a pipeline. The GPU bracket adds one clear pass and one load/store pass to
//! every timed frame, so compare probed runs with probed runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const CAP: usize = 50_000;

#[derive(Clone, Copy, Debug)]
struct Sample {
    update_ms: f64,
    encode_ms: f64,
    drawn: u32,
    pixels: u64,
    live: bool,
}

pub struct Probe {
    label: String,
    timer: Option<Timer>,
    tried_timer: bool,
    frames: Vec<Sample>,
    gpu: Vec<f64>,
    /// (frame, CPU ms that frame, pipelines compiled).
    compiles: Vec<(usize, f64, usize)>,
    print_bursts: bool,
    burst_start: usize,
    gpu_burst_start: usize,
    was_live: bool,
}

impl Probe {
    pub fn new(label: &str) -> Probe {
        Probe {
            label: label.to_owned(),
            timer: None,
            tried_timer: false,
            frames: Vec::with_capacity(4096),
            gpu: Vec::with_capacity(4096),
            compiles: Vec::new(),
            print_bursts: !rune::is_headless(),
            burst_start: 0,
            gpu_burst_start: 0,
            was_live: false,
        }
    }

    pub(super) fn begin(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clear: [f32; 4],
    ) {
        if !self.tried_timer {
            self.tried_timer = true;
            self.timer = Timer::new(device, queue);
        }
        let Some(timer) = self.timer.as_mut() else {
            return;
        };
        let _ = device.poll(wgpu::PollType::Poll);
        if let Some(ms) = timer.begin(encoder)
            && self.gpu.len() < CAP
        {
            self.gpu.push(f64::from(ms));
        }
        if timer.recording {
            let writes = wgpu::RenderPassTimestampWrites {
                query_set: &timer.queries,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: None,
            };
            let load = wgpu::LoadOp::Clear(wgpu::Color {
                r: f64::from(clear[0]),
                g: f64::from(clear[1]),
                b: f64::from(clear[2]),
                a: f64::from(clear[3]),
            });
            drop(pass(encoder, target, load, Some(writes)));
        }
    }

    pub(super) fn end(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let Some(timer) = self.timer.as_mut() else {
            return;
        };
        if timer.recording {
            let writes = wgpu::RenderPassTimestampWrites {
                query_set: &timer.queries,
                beginning_of_pass_write_index: None,
                end_of_pass_write_index: Some(1),
            };
            drop(pass(encoder, target, wgpu::LoadOp::Load, Some(writes)));
            timer.state.store(WRITTEN, Ordering::Release);
        }
    }

    pub(super) fn frame(
        &mut self,
        update_ms: f64,
        encode_ms: f64,
        drawn: u32,
        pixels: u64,
        compiled: usize,
        live: bool,
    ) {
        if self.frames.len() >= CAP {
            return;
        }
        if compiled > 0 {
            self.compiles
                .push((self.frames.len(), update_ms + encode_ms, compiled));
        }
        self.frames.push(Sample {
            update_ms,
            encode_ms,
            drawn,
            pixels,
            live,
        });
        if self.print_bursts && self.was_live && !live {
            eprintln!("{}", self.report(self.burst_start, self.gpu_burst_start));
            self.burst_start = self.frames.len();
            self.gpu_burst_start = self.gpu.len();
        }
        self.was_live = live;
    }

    pub fn summary(&self) -> String {
        self.report(0, 0)
    }

    fn report(&self, from: usize, gpu_from: usize) -> String {
        let frames = &self.frames[from.min(self.frames.len())..];
        let gpu = &self.gpu[gpu_from.min(self.gpu.len())..];
        let col = |f: fn(&Sample) -> f64| -> Vec<f64> { frames.iter().map(f).collect() };
        let update = col(|s| s.update_ms);
        let encode = col(|s| s.encode_ms);
        let cpu = col(|s| s.update_ms + s.encode_ms);
        let live = frames.iter().filter(|s| s.live).count();
        let drawn = frames.iter().map(|s| s.drawn).max().unwrap_or(0);
        let pixels = frames.iter().map(|s| s.pixels).max().unwrap_or(0);
        let compiles: Vec<String> = self
            .compiles
            .iter()
            .filter(|(i, ..)| *i >= from)
            .map(|(i, ms, n)| format!("#{i}:{ms:.1}ms(+{n})"))
            .collect();
        format!(
            "look[{}] frames={} live={} | cpu p50/p95/max {} | update {} | encode {} | gpu {} n={} | fx layers max {} px max {} | pipeline compiles [{}]",
            self.label,
            frames.len(),
            live,
            stats(&cpu),
            stats(&update),
            stats(&encode),
            if self.timer.is_some() {
                stats(gpu)
            } else {
                "unavailable".into()
            },
            gpu.len(),
            drawn,
            pixels,
            compiles.join(" "),
        )
    }
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn stats(values: &[f64]) -> String {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    format!(
        "{:.3}/{:.3}/{:.3}",
        percentile(&v, 0.5),
        percentile(&v, 0.95),
        v.last().copied().unwrap_or(f64::NAN)
    )
}

fn pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    view: &'e wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
    timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'e>>,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("look probe"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

const IDLE: u8 = 0;
const COPIED: u8 = 1;
const MAPPING: u8 = 2;
const MAPPED: u8 = 3;
const WRITTEN: u8 = 4;

/// Two timestamps per timed frame, resolved a command buffer later and mapped without stalling
/// (Metal can run a same-encoder resolve before the last pass's end sample lands).
struct Timer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    state: Arc<AtomicU8>,
    period: f32,
    recording: bool,
}

impl Timer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Timer> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("look probe"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(Timer {
            queries,
            resolve: buffer(
                "look probe resolve",
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            ),
            read: buffer(
                "look probe read",
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            ),
            state: Arc::new(AtomicU8::new(IDLE)),
            period: queue.get_timestamp_period(),
            recording: false,
        })
    }

    fn begin(&mut self, encoder: &mut wgpu::CommandEncoder) -> Option<f32> {
        let mut out = None;
        match self.state.load(Ordering::Acquire) {
            WRITTEN => {
                encoder.resolve_query_set(&self.queries, 0..2, &self.resolve, 0);
                encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
                self.state.store(COPIED, Ordering::Release);
            }
            COPIED => {
                self.state.store(MAPPING, Ordering::Release);
                let state = self.state.clone();
                self.read
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        state.store(if r.is_ok() { MAPPED } else { IDLE }, Ordering::Release);
                    });
            }
            MAPPED => {
                if let Ok(data) = self.read.slice(..).get_mapped_range() {
                    let word = |i: usize| {
                        data.get(i * 8..i * 8 + 8)
                            .and_then(|b| b.try_into().ok())
                            .map_or(0, u64::from_le_bytes)
                    };
                    let (start, end) = (word(0), word(1));
                    out = (end > start).then(|| (end - start) as f32 * self.period / 1e6);
                }
                self.read.unmap();
                self.state.store(IDLE, Ordering::Release);
            }
            _ => {}
        }
        self.recording = self.state.load(Ordering::Acquire) == IDLE;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_reports_cover_empty_and_live_bursts() {
        assert!(percentile(&[], 0.5).is_nan());
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 0.5), 3.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 0.95), 5.0);
        let mut probe = Probe::new("t");
        probe.print_bursts = false;
        probe.frame(1.0, 2.0, 3, 400, 0, true);
        probe.frame(0.5, 0.5, 0, 0, 2, false);
        let report = probe.summary();
        assert!(report.contains("frames=2 live=1"), "{report}");
        assert!(report.contains("#1:1.0ms(+2)"), "{report}");
        assert!(report.contains("gpu unavailable"), "{report}");
    }
}
