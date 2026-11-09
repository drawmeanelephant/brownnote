//! Standalone CLAP probe for #96. Not wired into the application.
//!
//! Subcommands:
//!   describe <bundle>
//!   render <bundle> <plugin-id> <scenario.tsv> <out-dir> <name> [options]
//!   state <bundle> <plugin-id> <scenario.tsv> <out-dir> <param-name>=<value>
//!
//! Render options: --block N, --mode offline|realtime, --live,
//! --load-state FILE, --save-state FILE.

mod clap;
mod json;
mod scenario;
mod wav;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{HostError, InEvent, Instance, Library, OutEvent};
use json::Json;
use scenario::Scenario;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("describe") if args.len() == 2 => describe(Path::new(&args[1])),
        Some("render") if args.len() >= 6 => render_cmd(&args[1..]),
        Some("state") if args.len() == 6 => state_cmd(&args[1..]),
        _ => {
            eprintln!("usage: see the module documentation in host/src/main.rs");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // A structured line the checker parses for load/lifecycle failures.
            println!(
                "{}",
                Json::obj([
                    ("error", Json::str(e.kind())),
                    ("detail", Json::str(e.to_string()))
                ])
            );
            ExitCode::from(3)
        }
    }
}

fn describe(bundle: &Path) -> Result<(), HostError> {
    let lib = Library::open(bundle)?;
    let mut plugins = Vec::new();
    for d in lib.descriptors() {
        let mut inst = lib.instantiate(&d.id)?;
        let params = inst
            .params()
            .into_iter()
            .map(|p| {
                Json::obj([
                    ("id", Json::num(p.id)),
                    ("name", Json::str(p.name)),
                    ("module", Json::str(p.module)),
                    ("min", Json::f(p.min)),
                    ("max", Json::f(p.max)),
                    ("default", Json::f(p.default)),
                    ("flags", Json::num(p.flags)),
                ])
            })
            .collect();
        let ports = |is_input: bool| {
            Json::Arr(
                inst.audio_ports(is_input)
                    .into_iter()
                    .map(|p| {
                        Json::obj([
                            ("name", Json::str(p.name)),
                            ("channels", Json::num(p.channels)),
                            ("flags", Json::num(p.flags)),
                        ])
                    })
                    .collect(),
            )
        };
        let note_ports = |is_input: bool| {
            Json::Arr(
                inst.note_ports(is_input)
                    .into_iter()
                    .map(|p| {
                        Json::obj([
                            ("name", Json::str(p.name)),
                            ("supported_dialects", Json::num(p.supported_dialects)),
                            ("preferred_dialect", Json::num(p.preferred_dialect)),
                        ])
                    })
                    .collect(),
            )
        };
        let audio_in = ports(true);
        let audio_out = ports(false);
        let notes_in = note_ports(true);
        let notes_out = note_ports(false);
        let hard_rt = inst.render_ext();
        inst.activate(48_000.0, 1, 512)?;
        let latency = inst.latency();
        let tail = inst.tail();
        inst.deactivate();
        plugins.push(Json::obj([
            ("id", Json::str(d.id)),
            ("name", Json::str(d.name)),
            ("vendor", Json::str(d.vendor)),
            ("version", Json::str(d.version)),
            (
                "features",
                Json::Arr(d.features.into_iter().map(Json::str).collect()),
            ),
            (
                "extensions",
                Json::Arr(inst.extensions().into_iter().map(Json::str).collect()),
            ),
            ("audio_inputs", audio_in),
            ("audio_outputs", audio_out),
            ("note_inputs", notes_in),
            ("note_outputs", notes_out),
            (
                "render_has_hard_realtime_requirement",
                Json::opt_bool(hard_rt),
            ),
            ("latency_after_activate_48k", Json::opt_num(latency)),
            ("tail_after_activate_48k", Json::opt_num(tail)),
            ("params", Json::Arr(params)),
        ]));
    }
    let v = lib.clap_version;
    println!(
        "{}",
        Json::obj([
            (
                "executable",
                Json::str(
                    lib.executable
                        .file_name()
                        .map_or(String::new(), |n| n.to_string_lossy().into_owned())
                ),
            ),
            (
                "clap_version",
                Json::str(format!("{}.{}.{}", v.0, v.1, v.2))
            ),
            ("plugins", Json::Arr(plugins)),
        ])
    );
    Ok(())
}

#[derive(Clone)]
struct RenderOptions {
    block: Option<u32>,
    offline: bool,
    live: bool,
    load_state: Option<PathBuf>,
    save_state: Option<PathBuf>,
}

fn parse_options(args: &[String]) -> RenderOptions {
    let mut o = RenderOptions {
        block: None,
        offline: true,
        live: false,
        load_state: None,
        save_state: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--block" => o.block = it.next().and_then(|v| v.parse().ok()),
            "--mode" => o.offline = it.next().map(String::as_str) != Some("realtime"),
            "--live" => o.live = true,
            "--load-state" => o.load_state = it.next().map(PathBuf::from),
            "--save-state" => o.save_state = it.next().map(PathBuf::from),
            other => panic!("unknown option {other}"),
        }
    }
    o
}

/// Everything one render produced, before it is written to disk.
struct Render {
    audio: Vec<Vec<f32>>,
    trace: Vec<String>,
    statuses: [u32; 5],
    process_ns: Vec<u64>,
    late_blocks: u32,
    max_lateness_ns: u64,
    nonfinite: u64,
    tail_first: Option<u32>,
    tail_last: Option<u32>,
    tail_changes: u32,
}

fn render_cmd(args: &[String]) -> Result<(), HostError> {
    let (bundle, plugin_id, scenario, out, name) = (
        Path::new(&args[0]),
        &args[1],
        Path::new(&args[2]),
        Path::new(&args[3]),
        &args[4],
    );
    let opts = parse_options(&args[5..]);
    let mut sc = Scenario::load(scenario);
    if let Some(b) = opts.block {
        sc.block = b;
    }
    let lib = Library::open(bundle)?;
    let mut inst = lib.instantiate(plugin_id)?;
    let hard_rt = inst.render_ext();
    let mode_set = inst.set_render_mode(opts.offline);
    if let Some(path) = &opts.load_state {
        inst.load_state(&std::fs::read(path).expect("read state"))?;
    }
    inst.activate(f64::from(sc.sample_rate), 1, sc.block)?;
    let latency = inst.latency();
    let events = sc.resolve(&inst)?;
    let (inst, r) = if opts.live {
        render_live(inst, &sc, &events)?
    } else {
        let r = render(&mut inst, &sc, &events)?;
        (inst, r)
    };
    let mut inst = inst;
    let state_len = match &opts.save_state {
        Some(path) => {
            let s = inst.save_state()?;
            std::fs::write(path, &s).expect("write state");
            Some(s.len() as u64)
        }
        None => None,
    };
    inst.deactivate();
    write_render(out, name, &sc, &r)?;
    let mut sorted = r.process_ns.clone();
    sorted.sort_unstable();
    let pct = |p: f64| {
        sorted
            .get(((sorted.len() - 1) as f64 * p) as usize)
            .copied()
            .unwrap_or(0)
    };
    let budget_ns = u64::from(sc.block) * 1_000_000_000 / u64::from(sc.sample_rate);
    let (restart, process, callback) = inst.host_requests();
    let summary = Json::obj([
        ("plugin_id", Json::str(plugin_id.clone())),
        ("frames", Json::num(sc.frames)),
        ("sample_rate", Json::num(sc.sample_rate)),
        ("block", Json::num(sc.block)),
        (
            "render_mode_requested",
            Json::str(if opts.offline { "offline" } else { "realtime" }),
        ),
        ("render_mode_accepted", Json::opt_bool(mode_set)),
        (
            "render_has_hard_realtime_requirement",
            Json::opt_bool(hard_rt),
        ),
        ("live_paced_thread", Json::Bool(opts.live)),
        ("latency", Json::opt_num(latency)),
        ("tail_first", Json::opt_num(r.tail_first)),
        ("tail_last", Json::opt_num(r.tail_last)),
        ("tail_changes", Json::num(r.tail_changes)),
        (
            "process_status_counts",
            Json::obj([
                ("error", Json::num(r.statuses[0])),
                ("continue", Json::num(r.statuses[1])),
                ("continue_if_not_quiet", Json::num(r.statuses[2])),
                ("tail", Json::num(r.statuses[3])),
                ("sleep", Json::num(r.statuses[4])),
            ]),
        ),
        ("nonfinite_samples", Json::num(r.nonfinite)),
        ("block_budget_ns", Json::num(budget_ns)),
        ("process_ns_p50", Json::num(pct(0.5))),
        ("process_ns_p99", Json::num(pct(0.99))),
        ("process_ns_max", Json::num(pct(1.0))),
        (
            "blocks_over_budget",
            Json::num(sorted.iter().filter(|n| **n > budget_ns).count() as u64),
        ),
        ("late_blocks", Json::num(r.late_blocks)),
        ("max_lateness_ns", Json::num(r.max_lateness_ns)),
        ("state_bytes_saved", Json::opt_num(state_len)),
        (
            "host_requests",
            Json::obj([
                ("restart", Json::num(restart)),
                ("process", Json::num(process)),
                ("callback", Json::num(callback)),
            ]),
        ),
    ]);
    std::fs::write(out.join(format!("{name}.json")), summary.to_string() + "\n")
        .expect("write json");
    Ok(())
}

fn alloc_ports(channels: &[u32], frames: usize) -> Vec<Vec<Vec<f32>>> {
    channels
        .iter()
        .map(|c| vec![vec![0.0; frames]; *c as usize])
        .collect()
}

fn new_render(sc: &Scenario, out_channels: usize) -> Render {
    Render {
        audio: vec![Vec::with_capacity(sc.frames as usize); out_channels],
        trace: Vec::new(),
        statuses: [0; 5],
        process_ns: Vec::new(),
        late_blocks: 0,
        max_lateness_ns: 0,
        nonfinite: 0,
        tail_first: None,
        tail_last: None,
        tail_changes: 0,
    }
}

/// One block. Shared by the offline and the paced live loops so both use
/// identical event slicing.
fn run_block(
    inst: &mut Instance,
    sc: &Scenario,
    events: &[InEvent],
    start: u32,
    r: &mut Render,
    input: &mut [Vec<Vec<f32>>],
    output: &mut [Vec<Vec<f32>>],
) {
    let n = sc.block.min(sc.frames - start);
    for port in input.iter_mut() {
        for (ch, buf) in port.iter_mut().enumerate() {
            buf.resize(n as usize, 0.0);
            for (i, s) in buf.iter_mut().enumerate() {
                *s = sc.input_sample(start + i as u32, ch);
            }
        }
    }
    for port in output.iter_mut() {
        for buf in port.iter_mut() {
            buf.clear();
            buf.resize(n as usize, 0.0);
        }
    }
    let block_events: Vec<InEvent> = events
        .iter()
        .filter(|e| e.time() >= start && e.time() < start + n)
        .map(|e| shift(e, start))
        .collect();
    for e in &block_events {
        r.trace.push(trace_in(e, start));
    }
    let t0 = Instant::now();
    let res = inst.process(n, input, output, &block_events);
    r.process_ns.push(t0.elapsed().as_nanos() as u64);
    if let Some(slot) = r.statuses.get_mut(res.status.clamp(0, 4) as usize) {
        *slot += 1;
    }
    if res.status == 0 {
        r.trace.push(format!("{start}\tstatus\terror\t-1\t-1\t0"));
    }
    for e in &res.events {
        r.trace.push(trace_out(e, start));
    }
    let tail = inst.tail();
    if tail != r.tail_last || r.tail_first.is_none() && tail.is_some() {
        if r.tail_first.is_none() {
            r.tail_first = tail;
        } else {
            r.tail_changes += 1;
        }
        r.trace.push(format!(
            "{}\ttail\tvalue\t-1\t-1\t{}",
            start + n,
            tail.map_or(-1, i64::from)
        ));
        r.tail_last = tail;
    }
    let main_out = output.first().map(Vec::as_slice).unwrap_or(&[]);
    for (ch, dst) in r.audio.iter_mut().enumerate() {
        let src = &main_out[ch.min(main_out.len().saturating_sub(1))];
        r.nonfinite += src.iter().filter(|s| !s.is_finite()).count() as u64;
        dst.extend_from_slice(src);
    }
}

fn render(inst: &mut Instance, sc: &Scenario, events: &[InEvent]) -> Result<Render, HostError> {
    let mut input = alloc_ports(&inst.inputs.clone(), sc.block as usize);
    let mut output = alloc_ports(&inst.outputs.clone(), sc.block as usize);
    let mut r = new_render(sc, 2);
    inst.start_processing()?;
    let mut start = 0;
    while start < sc.frames {
        run_block(inst, sc, events, start, &mut r, &mut input, &mut output);
        start += sc.block;
    }
    inst.stop_processing();
    Ok(r)
}

/// Runs processing on a dedicated thread paced by the wall clock, the way a
/// device callback would ask for blocks. It is not a device callback: no
/// CoreAudio deadline exists and nothing is heard. Lateness is measured
/// against the ideal block clock.
fn render_live(
    inst: Instance,
    sc: &Scenario,
    events: &[InEvent],
) -> Result<(Instance, Render), HostError> {
    let sc = sc.clone();
    let events = events.to_vec();
    std::thread::Builder::new()
        .name("probe-audio".into())
        .spawn(move || {
            let mut inst = inst;
            let mut input = alloc_ports(&inst.inputs.clone(), sc.block as usize);
            let mut output = alloc_ports(&inst.outputs.clone(), sc.block as usize);
            let mut r = new_render(&sc, 2);
            inst.start_processing()?;
            let period = Duration::from_nanos(
                u64::from(sc.block) * 1_000_000_000 / u64::from(sc.sample_rate),
            );
            let epoch = Instant::now();
            let mut start = 0;
            let mut index = 0u32;
            while start < sc.frames {
                let due = epoch + period * index;
                let now = Instant::now();
                if now < due {
                    std::thread::sleep(due - now);
                } else {
                    let late = (now - due).as_nanos() as u64;
                    if late > period.as_nanos() as u64 {
                        r.late_blocks += 1;
                    }
                    r.max_lateness_ns = r.max_lateness_ns.max(late);
                }
                run_block(
                    &mut inst,
                    &sc,
                    &events,
                    start,
                    &mut r,
                    &mut input,
                    &mut output,
                );
                start += sc.block;
                index += 1;
            }
            inst.stop_processing();
            Ok((inst, r))
        })
        .expect("spawn audio thread")
        .join()
        .expect("audio thread panicked")
}

fn shift(e: &InEvent, start: u32) -> InEvent {
    match *e {
        InEvent::NoteOn {
            time,
            note_id,
            key,
            velocity,
        } => InEvent::NoteOn {
            time: time - start,
            note_id,
            key,
            velocity,
        },
        InEvent::NoteOff { time, note_id, key } => InEvent::NoteOff {
            time: time - start,
            note_id,
            key,
        },
        InEvent::Param {
            time,
            param_id,
            value,
        } => InEvent::Param {
            time: time - start,
            param_id,
            value,
        },
    }
}

fn trace_in(e: &InEvent, start: u32) -> String {
    let t = start + e.time();
    match *e {
        InEvent::NoteOn {
            note_id,
            key,
            velocity,
            ..
        } => format!("{t}\tin\tnote-on\t{note_id}\t{key}\t{velocity}"),
        InEvent::NoteOff { note_id, key, .. } => format!("{t}\tin\tnote-off\t{note_id}\t{key}\t0"),
        InEvent::Param {
            param_id, value, ..
        } => format!("{t}\tin\tparam\t-1\t{param_id}\t{value}"),
    }
}

fn trace_out(e: &OutEvent, start: u32) -> String {
    format!(
        "{}\tout\t{}\t{}\t{}\t{}",
        start + e.time,
        e.kind,
        e.note_id,
        e.key,
        e.value
    )
}

fn write_render(out: &Path, name: &str, sc: &Scenario, r: &Render) -> Result<(), HostError> {
    std::fs::create_dir_all(out).expect("create out dir");
    wav::write_f32(&out.join(format!("{name}.wav")), sc.sample_rate, &r.audio).expect("write wav");
    let mut t = String::from("frame\tsource\tkind\tnote_id\tkey\tvalue\n");
    for line in &r.trace {
        t.push_str(line);
        t.push('\n');
    }
    std::fs::write(out.join(format!("{name}.trace.tsv")), t).expect("write trace");
    Ok(())
}

/// Save, mutate, restore inside one instance, then report the three
/// renders and the three state blobs. Comparing them is the checker's job.
fn state_cmd(args: &[String]) -> Result<(), HostError> {
    let (bundle, plugin_id, scenario, out, mutation) = (
        Path::new(&args[0]),
        &args[1],
        Path::new(&args[2]),
        Path::new(&args[3]),
        &args[4],
    );
    let (pname, pvalue) = mutation.split_once('=').expect("mutation is name=value");
    let pvalue: f64 = pvalue.parse().expect("mutation value");
    let sc = Scenario::load(scenario);
    let lib = Library::open(bundle)?;
    let mut inst = lib.instantiate(plugin_id)?;
    inst.set_render_mode(true);
    inst.activate(f64::from(sc.sample_rate), 1, sc.block)?;
    let events = sc.resolve(&inst)?;
    std::fs::create_dir_all(out).expect("create out dir");

    let s0 = inst.save_state()?;
    let r = render(&mut inst, &sc, &events)?;
    write_render(out, "state-reference", &sc, &r)?;

    let pid = inst.param_id(pname)?;
    let before = inst.param_value(pid);
    inst.reset();
    inst.flush_params(&[InEvent::Param {
        time: 0,
        param_id: pid,
        value: pvalue,
    }])?;
    let after = inst.param_value(pid);
    let s1 = inst.save_state()?;
    let r = render(&mut inst, &sc, &events)?;
    write_render(out, "state-mutated", &sc, &r)?;

    inst.load_state(&s0)?;
    inst.reset();
    let restored = inst.param_value(pid);
    let s2 = inst.save_state()?;
    let r = render(&mut inst, &sc, &events)?;
    write_render(out, "state-restored", &sc, &r)?;
    inst.deactivate();

    for (n, s) in [("s0", &s0), ("s1", &s1), ("s2", &s2)] {
        std::fs::write(out.join(format!("state-{n}.bin")), s).expect("write state");
    }
    let summary = Json::obj([
        ("param", Json::str(pname)),
        ("param_id", Json::num(pid)),
        ("value_before", Json::opt_f(before)),
        ("value_after_mutation", Json::opt_f(after)),
        ("value_after_restore", Json::opt_f(restored)),
        ("s0_bytes", Json::num(s0.len() as u64)),
        ("s1_bytes", Json::num(s1.len() as u64)),
        ("s2_bytes", Json::num(s2.len() as u64)),
        ("s2_equals_s0", Json::Bool(s2 == s0)),
        ("s1_equals_s0", Json::Bool(s1 == s0)),
    ]);
    std::fs::write(out.join("state.json"), summary.to_string() + "\n").expect("write json");
    Ok(())
}
