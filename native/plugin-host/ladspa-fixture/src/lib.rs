//! LADSPA plugins for `native/tests/ladspa_offline.rs`: pass-throughs with
//! every supported channel shape, a control-echo plugin for hint
//! resolution, and plugins that fault (NaN, endless output, hang, crash,
//! failed instantiation, realtime-only, malformed descriptors).
//!
//! Original Brownnote code, MIT licensed. It repeats the LADSPA 1.1 ABI
//! structs, so it needs no SDK header. Faults are defined behaviour: the
//! crash raises `SIGSEGV` with the default disposition rather than touching
//! an invalid pointer. If `BROWNNOTE_LADSPA_FIXTURE_LOG` names a file, each
//! lifecycle call appends one line to it.
use std::{
    ffi::{CStr, c_char, c_int, c_ulong, c_void},
    io::Write,
    ptr,
    sync::OnceLock,
};

#[repr(C)]
pub struct PortRangeHint {
    hint: c_int,
    lower: f32,
    upper: f32,
}

type Handle = *mut c_void;

#[repr(C)]
pub struct Descriptor {
    unique_id: c_ulong,
    label: *const c_char,
    properties: c_int,
    name: *const c_char,
    maker: *const c_char,
    copyright: *const c_char,
    port_count: c_ulong,
    port_descriptors: *const c_int,
    port_names: *const *const c_char,
    port_range_hints: *const PortRangeHint,
    implementation_data: *mut c_void,
    instantiate: Option<unsafe extern "C" fn(*const Descriptor, c_ulong) -> Handle>,
    connect_port: Option<unsafe extern "C" fn(Handle, c_ulong, *mut f32)>,
    activate: Option<unsafe extern "C" fn(Handle)>,
    run: Option<unsafe extern "C" fn(Handle, c_ulong)>,
    run_adding: Option<unsafe extern "C" fn(Handle, c_ulong)>,
    set_run_adding_gain: Option<unsafe extern "C" fn(Handle, f32)>,
    deactivate: Option<unsafe extern "C" fn(Handle)>,
    cleanup: Option<unsafe extern "C" fn(Handle)>,
}

const IN: c_int = 0x1;
const OUT: c_int = 0x2;
const CONTROL: c_int = 0x4;
const AUDIO: c_int = 0x8;
/// A private port bit like the one CAPS sets; hosts must mask it.
const GROUP: c_int = 0x10;

const BELOW: c_int = 0x1;
const ABOVE: c_int = 0x2;
const TOGGLED: c_int = 0x4;
const SAMPLE_RATE: c_int = 0x8;
const LOG: c_int = 0x10;
const INTEGER: c_int = 0x20;
const D_LOW: c_int = 0x80;
const D_MID: c_int = 0xC0;
const D_MAX: c_int = 0x140;
const D_1: c_int = 0x240;

const REALTIME: c_int = 0x1;
const INPLACE_BROKEN: c_int = 0x2;

/// The frame at which the faulting plugins fault.
const FAULT_FRAME: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Output channel k copies input channel k (mod the input count).
    Pass,
    /// Left is the input, right its negation.
    Split,
    /// The one output is the sum of both inputs.
    Sum,
    /// Left carries the control inputs, one per frame for the first
    /// frames, then silence. Right is the input.
    Controls,
    Nan,
    Endless,
    Hang,
    Crash,
    InitFails,
}

struct Port(c_int, &'static CStr, c_int, f32, f32);

struct Spec {
    id: c_ulong,
    label: &'static CStr,
    properties: c_int,
    ports: &'static [Port],
    mode: Mode,
    run: bool,
    /// `run_adding` writes NaN: a host that calls it fails the render.
    run_adding_trap: bool,
}

const fn audio(direction: c_int, name: &'static CStr) -> Port {
    Port(direction | AUDIO, name, 0, 0.0, 0.0)
}

static STEREO: [Port; 4] = [
    audio(IN, c"in.l"),
    audio(IN, c"in.r"),
    audio(OUT, c"out.l"),
    audio(OUT, c"out.r"),
];
static MONO: [Port; 2] = [audio(IN, c"in"), audio(OUT, c"out")];
static SPLIT: [Port; 3] = [audio(IN, c"in"), audio(OUT, c"out.l"), audio(OUT, c"out.r")];
static SUM: [Port; 3] = [audio(IN, c"in.l"), audio(IN, c"in.r"), audio(OUT, c"out")];
static CONTROLS: [Port; 10] = [
    audio(IN, c"in"),
    audio(OUT, c"out.l"),
    audio(OUT, c"out.r"),
    Port(
        IN | CONTROL,
        c"rate",
        BELOW | ABOVE | SAMPLE_RATE | D_MAX,
        0.0,
        0.5,
    ),
    Port(
        IN | CONTROL | GROUP,
        c"log",
        BELOW | ABOVE | LOG | D_MID,
        20.0,
        20000.0,
    ),
    Port(
        IN | CONTROL,
        c"int",
        BELOW | ABOVE | INTEGER | D_LOW,
        1.0,
        8.0,
    ),
    Port(IN | CONTROL, c"toggle", TOGGLED | D_1, 0.0, 0.0),
    Port(IN | CONTROL, c"bounded", BELOW | ABOVE, -2.0, 5.0),
    Port(IN | CONTROL, c"free", 0, 0.0, 0.0),
    Port(OUT | CONTROL, c"status", 0, 0.0, 0.0),
];
static WIDE: [Port; 4] = [
    audio(IN, c"in"),
    audio(OUT, c"out.1"),
    audio(OUT, c"out.2"),
    audio(OUT, c"out.3"),
];
static NO_OUTPUT: [Port; 1] = [audio(IN, c"in")];
static BAD_PORT: [Port; 2] = [audio(IN, c"in"), audio(IN | OUT, c"both")];
static BAD_HINT: [Port; 3] = [
    audio(IN, c"in"),
    audio(OUT, c"out"),
    Port(IN | CONTROL, c"inverted", BELOW | ABOVE | D_MID, 1.0, 0.0),
];

const fn spec(id: c_ulong, label: &'static CStr, ports: &'static [Port], mode: Mode) -> Spec {
    Spec {
        id,
        label,
        properties: 0,
        ports,
        mode,
        run: true,
        run_adding_trap: false,
    }
}

static SPECS: [Spec; 16] = [
    Spec {
        properties: INPLACE_BROKEN,
        run_adding_trap: true,
        ..spec(9801, c"pass_stereo", &STEREO, Mode::Pass)
    },
    spec(9802, c"pass_mono", &MONO, Mode::Pass),
    spec(9803, c"split", &SPLIT, Mode::Split),
    spec(9804, c"sum", &SUM, Mode::Sum),
    spec(9805, c"controls", &CONTROLS, Mode::Controls),
    spec(9806, c"nan", &STEREO, Mode::Nan),
    spec(9807, c"endless", &STEREO, Mode::Endless),
    spec(9808, c"hang", &STEREO, Mode::Hang),
    spec(9809, c"crash", &STEREO, Mode::Crash),
    spec(9810, c"init_fails", &STEREO, Mode::InitFails),
    Spec {
        properties: REALTIME,
        ..spec(9811, c"realtime", &STEREO, Mode::Pass)
    },
    spec(9812, c"wide", &WIDE, Mode::Pass),
    spec(9813, c"no_output", &NO_OUTPUT, Mode::Pass),
    spec(9814, c"bad_port", &BAD_PORT, Mode::Pass),
    spec(9815, c"bad_hint", &BAD_HINT, Mode::Pass),
    Spec {
        run: false,
        ..spec(9816, c"no_run", &STEREO, Mode::Pass)
    },
];

struct Table(Vec<Descriptor>);
// SAFETY: the descriptors are built once and never mutated; their pointers
// refer to leaked, immutable allocations and static strings.
unsafe impl Sync for Table {}
// SAFETY: as above.
unsafe impl Send for Table {}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        Table(
            SPECS
                .iter()
                .enumerate()
                .map(|(index, spec)| {
                    let kinds: &'static [c_int] =
                        Box::leak(spec.ports.iter().map(|p| p.0).collect());
                    let names: &'static [*const c_char] =
                        Box::leak(spec.ports.iter().map(|p| p.1.as_ptr()).collect());
                    let hints: &'static [PortRangeHint] = Box::leak(
                        spec.ports
                            .iter()
                            .map(|p| PortRangeHint {
                                hint: p.2,
                                lower: p.3,
                                upper: p.4,
                            })
                            .collect(),
                    );
                    Descriptor {
                        unique_id: spec.id,
                        label: spec.label.as_ptr(),
                        properties: spec.properties,
                        name: spec.label.as_ptr(),
                        maker: c"Brownnote fixture".as_ptr(),
                        copyright: c"MIT".as_ptr(),
                        port_count: spec.ports.len() as c_ulong,
                        port_descriptors: kinds.as_ptr(),
                        port_names: names.as_ptr(),
                        port_range_hints: hints.as_ptr(),
                        implementation_data: ptr::without_provenance_mut(index),
                        instantiate: Some(instantiate),
                        connect_port: Some(connect_port),
                        activate: Some(activate),
                        run: spec
                            .run
                            .then_some(run as unsafe extern "C" fn(Handle, c_ulong)),
                        run_adding: spec
                            .run_adding_trap
                            .then_some(run_adding as unsafe extern "C" fn(Handle, c_ulong)),
                        set_run_adding_gain: spec
                            .run_adding_trap
                            .then_some(set_run_adding_gain as unsafe extern "C" fn(Handle, f32)),
                        deactivate: Some(deactivate),
                        cleanup: Some(cleanup),
                    }
                })
                .collect(),
        )
    })
}

/// # Safety
/// The LADSPA entry point; any index is accepted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ladspa_descriptor(index: c_ulong) -> *const Descriptor {
    table()
        .0
        .get(index as usize)
        .map_or(ptr::null(), |descriptor| descriptor as *const Descriptor)
}

struct Fixture {
    spec: &'static Spec,
    ports: Vec<*mut f32>,
    frame: usize,
}

fn note(spec: &Spec, what: &str) {
    let Some(path) = std::env::var_os("BROWNNOTE_LADSPA_FIXTURE_LOG") else {
        return;
    };
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(log, "{} {what}", spec.label.to_string_lossy());
    }
}

unsafe extern "C" {
    fn raise(signal: c_int) -> c_int;
    fn signal(signal: c_int, handler: usize) -> usize;
}

const SIGSEGV: c_int = 11;
const SIG_DFL: usize = 0;

/// Ends the process the way a segfaulting plugin would, without undefined
/// behaviour. Rust's own SIGSEGV handler is replaced first, because it
/// ignores a signal that was raised rather than caused by a fault.
fn crash() -> ! {
    // SAFETY: resetting a signal disposition and raising a signal are
    // defined operations.
    unsafe {
        signal(SIGSEGV, SIG_DFL);
        raise(SIGSEGV);
    }
    std::process::abort()
}

unsafe extern "C" fn instantiate(descriptor: *const Descriptor, _rate: c_ulong) -> Handle {
    // SAFETY: the host passes one of this library's descriptors.
    let index = unsafe { (*descriptor).implementation_data }.addr();
    let spec = &SPECS[index];
    note(spec, "instantiate");
    if spec.mode == Mode::InitFails {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(Fixture {
        spec,
        ports: vec![ptr::null_mut(); spec.ports.len()],
        frame: 0,
    }))
    .cast()
}

unsafe extern "C" fn connect_port(handle: Handle, port: c_ulong, data: *mut f32) {
    // SAFETY: `handle` came from `instantiate`.
    let fixture = unsafe { &mut *handle.cast::<Fixture>() };
    if let Some(slot) = fixture.ports.get_mut(port as usize) {
        *slot = data;
    }
}

unsafe extern "C" fn activate(handle: Handle) {
    // SAFETY: `handle` came from `instantiate`.
    note(unsafe { &*handle.cast::<Fixture>() }.spec, "activate");
}

unsafe extern "C" fn deactivate(handle: Handle) {
    // SAFETY: `handle` came from `instantiate`.
    note(unsafe { &*handle.cast::<Fixture>() }.spec, "deactivate");
}

unsafe extern "C" fn cleanup(handle: Handle) {
    // SAFETY: `handle` came from `instantiate` and is cleaned up once.
    let fixture = unsafe { Box::from_raw(handle.cast::<Fixture>()) };
    note(fixture.spec, "cleanup");
}

unsafe extern "C" fn set_run_adding_gain(_handle: Handle, _gain: f32) {}

unsafe extern "C" fn run_adding(handle: Handle, frames: c_ulong) {
    // SAFETY: `handle` came from `instantiate`; the host connected ports.
    let fixture = unsafe { &mut *handle.cast::<Fixture>() };
    for (port, data) in fixture.spec.ports.iter().zip(&fixture.ports) {
        if port.0 & (OUT | AUDIO) == OUT | AUDIO && !data.is_null() {
            for frame in 0..frames as usize {
                // SAFETY: the host connected `frames` samples.
                unsafe { data.add(frame).write(f32::NAN) };
            }
        }
    }
}

unsafe extern "C" fn run(handle: Handle, frames: c_ulong) {
    // SAFETY: `handle` came from `instantiate`.
    let fixture = unsafe { &mut *handle.cast::<Fixture>() };
    // A host must connect every port, control outputs included.
    if fixture.ports.iter().any(|data| data.is_null()) {
        std::process::abort();
    }
    let spec = fixture.spec;
    let ports = |want: c_int| -> Vec<*mut f32> {
        spec.ports
            .iter()
            .zip(&fixture.ports)
            .filter(|(port, _)| port.0 & (IN | OUT | CONTROL | AUDIO) == want)
            .map(|(_, data)| *data)
            .collect()
    };
    let inputs = ports(IN | AUDIO);
    let outputs = ports(OUT | AUDIO);
    let controls = ports(IN | CONTROL);
    let in_place =
        spec.properties & INPLACE_BROKEN != 0 && inputs.iter().any(|input| outputs.contains(input));
    // Raw reads and writes: an in-place host would alias inputs and outputs.
    // SAFETY (all accesses below): the host connected `frames` samples to
    // each audio port and one value to each control port.
    let read = |data: *mut f32, frame: usize| unsafe { data.add(frame).read() };
    for frame in 0..frames as usize {
        let position = fixture.frame + frame;
        if position >= FAULT_FRAME {
            match spec.mode {
                Mode::Hang => loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                },
                Mode::Crash => crash(),
                _ => {}
            }
        }
        let first = read(inputs[0], frame);
        let values: Vec<f32> = match spec.mode {
            Mode::Split => vec![first, -first],
            Mode::Sum => vec![first + read(inputs[1], frame)],
            Mode::Controls => {
                let control = controls.get(position).map_or(0.0, |data| read(*data, 0));
                vec![control, first]
            }
            Mode::Nan if position >= FAULT_FRAME => vec![f32::NAN; outputs.len()],
            Mode::Endless => vec![0.5; outputs.len()],
            _ => (0..outputs.len())
                .map(|k| read(inputs[k % inputs.len()], frame))
                .collect(),
        };
        for (output, value) in outputs.iter().zip(values) {
            let value = if in_place { f32::NAN } else { value };
            // SAFETY: as above.
            unsafe { output.add(frame).write(value) };
        }
    }
    for status in ports(OUT | CONTROL) {
        // SAFETY: as above.
        unsafe { status.write(1.0) };
    }
    fixture.frame += frames as usize;
}
