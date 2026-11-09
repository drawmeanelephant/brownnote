//! Deliberately faulty CLAP plugins for the helper tests
//! (`native/tests/clap_offline.rs`, `native/tests/clap_live.rs`), carried over
//! from the #96 failure probe.
//!
//! One bundle exposes several stereo pass-through effects and a small
//! instrument family. Each faulty variant misbehaves in one documented way
//! once it has processed `FAULT_BLOCK` blocks since activation. They exist to
//! exercise host failure handling; nothing here is a model for a real plugin.
//!
//! The fixture instrument is exact by construction, so tests can predict
//! every output sample: each sounding voice adds `gain * 0.1 * velocity` to
//! the left channel and `gain * 0.1 * velocity * key / 127` to the right,
//! from its note-on frame up to (not including) its note-off frame. A note-off
//! ends the voice at once and reports `CLAP_EVENT_NOTE_END` at that frame.
//! Parameter 0 is the gain (0..=2, default 1). Its state is 16 bytes: a save
//! generation and the gain.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_END, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON,
    CLAP_EVENT_PARAM_VALUE, clap_event_header, clap_event_note, clap_event_param_value,
    clap_input_events, clap_output_events,
};
use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, CLAP_PORT_STEREO, clap_audio_port_info,
    clap_plugin_audio_ports,
};
use clap_sys::ext::note_ports::{
    CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, clap_note_port_info, clap_plugin_note_ports,
};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, clap_param_info, clap_plugin_params,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::id::CLAP_INVALID_ID;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{
    CLAP_PROCESS_CONTINUE, CLAP_PROCESS_ERROR, clap_process, clap_process_status,
};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;

/// Blocks processed normally before a fault fires.
const FAULT_BLOCK: u32 = 8;
/// Voices the fixture instrument can hold; further note-ons are ignored.
const VOICES: usize = 64;
const GAIN_PARAM: u32 = 0;

unsafe extern "C" {
    fn signal(
        signal: std::ffi::c_int,
        handler: Option<unsafe extern "C" fn(std::ffi::c_int)>,
    ) -> Option<unsafe extern "C" fn(std::ffi::c_int)>;
    fn raise(signal: std::ffi::c_int) -> std::ffi::c_int;
}

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    Crash,
    Hang,
    NonFinite,
    InitFails,
    ActivateFails,
}

struct Desc(clap_plugin_descriptor);
// SAFETY: descriptors only point at 'static string literals.
unsafe impl Sync for Desc {}

const FEATURES: [*const c_char; 3] = [c"audio-effect".as_ptr(), c"stereo".as_ptr(), ptr::null()];
const INSTRUMENT_FEATURES: [*const c_char; 3] =
    [c"instrument".as_ptr(), c"stereo".as_ptr(), ptr::null()];
struct Features([*const c_char; 3]);
// SAFETY: as Desc.
unsafe impl Sync for Features {}
static FEATURE_LIST: Features = Features(FEATURES);
static INSTRUMENT_FEATURE_LIST: Features = Features(INSTRUMENT_FEATURES);

const fn instrument(id: &'static CStr, name: &'static CStr) -> Desc {
    let mut d = desc(id, name);
    d.0.description = c"Exact test instrument for live host tests".as_ptr();
    d.0.features = INSTRUMENT_FEATURE_LIST.0.as_ptr();
    d
}

const fn desc(id: &'static CStr, name: &'static CStr) -> Desc {
    Desc(clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: id.as_ptr(),
        name: name.as_ptr(),
        vendor: c"brownnote test fixture".as_ptr(),
        url: c"".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"0.1.0".as_ptr(),
        description: c"Deliberately faulty pass-through for host failure tests".as_ptr(),
        features: FEATURE_LIST.0.as_ptr(),
    })
}

static PLUGINS: [(Desc, Fault); 10] = [
    (
        desc(
            c"org.brownnote.fault.passthrough",
            c"Fault fixture: passthrough",
        ),
        Fault::None,
    ),
    (
        desc(
            c"org.brownnote.fault.crash",
            c"Fault fixture: segfault in process",
        ),
        Fault::Crash,
    ),
    (
        desc(
            c"org.brownnote.fault.hang",
            c"Fault fixture: hang in process",
        ),
        Fault::Hang,
    ),
    (
        desc(
            c"org.brownnote.fault.nonfinite",
            c"Fault fixture: NaN/Inf output",
        ),
        Fault::NonFinite,
    ),
    (
        desc(
            c"org.brownnote.fault.init-fails",
            c"Fault fixture: init fails",
        ),
        Fault::InitFails,
    ),
    (
        desc(
            c"org.brownnote.fault.activate-fails",
            c"Fault fixture: activate fails",
        ),
        Fault::ActivateFails,
    ),
    (
        instrument(
            c"org.brownnote.fixture.instrument",
            c"Fixture instrument: exact voices",
        ),
        Fault::None,
    ),
    (
        instrument(
            c"org.brownnote.fixture.instrument.crash",
            c"Fixture instrument: segfault in process",
        ),
        Fault::Crash,
    ),
    (
        instrument(
            c"org.brownnote.fixture.instrument.hang",
            c"Fixture instrument: hang in process",
        ),
        Fault::Hang,
    ),
    (
        instrument(
            c"org.brownnote.fixture.instrument.nonfinite",
            c"Fixture instrument: NaN/Inf output",
        ),
        Fault::NonFinite,
    ),
];

#[derive(Clone, Copy)]
struct Voice {
    note_id: i32,
    port: i16,
    channel: i16,
    key: i16,
    velocity: f64,
}

struct Plugin {
    clap: clap_plugin,
    fault: Fault,
    instrument: bool,
    blocks: u32,
    /// Opaque state: how many times state was saved from this lineage.
    generation: u64,
    gain: f64,
    voices: [Option<Voice>; VOICES],
}

fn this<'a>(p: *const clap_plugin) -> &'a mut Plugin {
    // SAFETY: plugin_data always points at the owning Plugin.
    unsafe { &mut *((*p).plugin_data as *mut Plugin) }
}

unsafe extern "C" fn init(p: *const clap_plugin) -> bool {
    this(p).fault != Fault::InitFails
}
unsafe extern "C" fn destroy(p: *const clap_plugin) {
    // SAFETY: created by Box::into_raw in create_plugin.
    drop(unsafe { Box::from_raw((*p).plugin_data as *mut Plugin) });
}
unsafe extern "C" fn activate(p: *const clap_plugin, _sr: f64, _min: u32, _max: u32) -> bool {
    let me = this(p);
    me.blocks = 0;
    me.voices = [None; VOICES];
    me.fault != Fault::ActivateFails
}
unsafe extern "C" fn deactivate(_p: *const clap_plugin) {}
unsafe extern "C" fn start_processing(_p: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn stop_processing(_p: *const clap_plugin) {}
unsafe extern "C" fn reset(_p: *const clap_plugin) {}
unsafe extern "C" fn on_main_thread(_p: *const clap_plugin) {}

/// Applies one host event to the instrument; note ends go to `out`.
fn apply_event(me: &mut Plugin, header: *const clap_event_header, out: *const clap_output_events) {
    // SAFETY: the host passes valid events; sizes are checked before the
    // typed fields are read.
    unsafe {
        let h = &*header;
        if h.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return;
        }
        if h.type_ == CLAP_EVENT_PARAM_VALUE
            && h.size as usize >= size_of::<clap_event_param_value>()
        {
            let e = &*(header as *const clap_event_param_value);
            if e.param_id == GAIN_PARAM {
                me.gain = e.value.clamp(0.0, 2.0);
            }
            return;
        }
        if h.size as usize >= size_of::<clap_event_note>() {
            let e = &*(header as *const clap_event_note);
            if h.type_ == CLAP_EVENT_NOTE_ON {
                if let Some(slot) = me.voices.iter_mut().find(|v| v.is_none()) {
                    *slot = Some(Voice {
                        note_id: e.note_id,
                        port: e.port_index,
                        channel: e.channel,
                        key: e.key,
                        velocity: e.velocity,
                    });
                }
            } else if h.type_ == CLAP_EVENT_NOTE_OFF {
                for slot in me.voices.iter_mut() {
                    let Some(v) = *slot else { continue };
                    let matches = if e.note_id != -1 {
                        v.note_id == e.note_id
                    } else {
                        (e.channel == -1 || e.channel == v.channel)
                            && (e.key == -1 || e.key == v.key)
                    };
                    if matches {
                        *slot = None;
                        let end = clap_event_note {
                            header: clap_event_header {
                                size: size_of::<clap_event_note>() as u32,
                                time: h.time,
                                space_id: CLAP_CORE_EVENT_SPACE_ID,
                                type_: CLAP_EVENT_NOTE_END,
                                flags: 0,
                            },
                            note_id: v.note_id,
                            port_index: v.port,
                            channel: v.channel,
                            key: v.key,
                            velocity: 0.0,
                        };
                        if let Some(push) = (*out).try_push {
                            push(out, &end.header);
                        }
                    }
                }
            }
        }
    }
}

fn instrument_frame(me: &Plugin) -> (f32, f32) {
    let mut left = 0.0;
    let mut right = 0.0;
    for v in me.voices.iter().flatten() {
        let amp = me.gain * 0.1 * v.velocity;
        left += amp;
        right += amp * f64::from(v.key) / 127.0;
    }
    (left as f32, right as f32)
}

unsafe extern "C" fn process(
    p: *const clap_plugin,
    pr: *const clap_process,
) -> clap_process_status {
    let me = this(p);
    // SAFETY: the host passes a valid process struct for the call.
    let pr = unsafe { &*pr };
    let inputs_needed = u32::from(!me.instrument);
    if pr.audio_inputs_count < inputs_needed || pr.audio_outputs_count < 1 {
        return CLAP_PROCESS_ERROR;
    }
    let fire = me.blocks >= FAULT_BLOCK;
    me.blocks += 1;
    if fire {
        match me.fault {
            Fault::Crash => {
                // SAFETY: SIGSEGV is 11 on the tested macOS/Unix platform.
                // Restore its default disposition so Rust's stack-overflow
                // handler cannot return from this explicitly raised signal.
                unsafe {
                    signal(11, None);
                    raise(11);
                };
                return CLAP_PROCESS_ERROR;
            }
            Fault::Hang => loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            },
            _ => {}
        }
    }
    if me.instrument {
        // SAFETY: event lists and buffers are valid for the call and hold
        // frames_count samples per channel.
        unsafe {
            let ins = &*pr.in_events;
            let count = ins.size.map_or(0, |size| size(ins));
            let mut next = 0;
            let output = &*pr.audio_outputs;
            for i in 0..pr.frames_count {
                while next < count {
                    let Some(get) = ins.get else { break };
                    let header = get(ins, next);
                    if header.is_null() || (*header).time > i {
                        break;
                    }
                    apply_event(me, header, pr.out_events);
                    next += 1;
                }
                let (left, right) = if fire && me.fault == Fault::NonFinite {
                    (f32::NAN, f32::INFINITY)
                } else {
                    instrument_frame(me)
                };
                for ch in 0..output.channel_count as usize {
                    *(*output.data32.add(ch)).add(i as usize) = if ch == 0 { left } else { right };
                }
            }
            while next < count {
                let Some(get) = ins.get else { break };
                let header = get(ins, next);
                if !header.is_null() {
                    apply_event(me, header, pr.out_events);
                }
                next += 1;
            }
        }
        return CLAP_PROCESS_CONTINUE;
    }
    // SAFETY: buffers hold frames_count samples per channel.
    unsafe {
        let input = &*pr.audio_inputs;
        let output = &*pr.audio_outputs;
        for ch in 0..output.channel_count.min(input.channel_count) as usize {
            let src = *input.data32.add(ch);
            let dst = *output.data32.add(ch);
            for i in 0..pr.frames_count as usize {
                *dst.add(i) = if fire && me.fault == Fault::NonFinite {
                    if i % 2 == 0 { f32::NAN } else { f32::INFINITY }
                } else {
                    *src.add(i)
                };
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

unsafe extern "C" fn ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    u32::from(!(is_input && this(p).instrument))
}
unsafe extern "C" fn ports_get(
    _p: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if index != 0 {
        return false;
    }
    // SAFETY: host-provided out parameter.
    let info = unsafe { &mut *info };
    info.id = u32::from(!is_input);
    info.name = [0; 256];
    for (d, s) in info.name.iter_mut().zip(b"Main") {
        *d = *s as c_char;
    }
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = CLAP_INVALID_ID;
    true
}

unsafe extern "C" fn state_save(p: *const clap_plugin, s: *const clap_ostream) -> bool {
    let me = this(p);
    me.generation += 1;
    let mut bytes = me.generation.to_le_bytes().to_vec();
    if me.instrument {
        bytes.extend_from_slice(&me.gain.to_le_bytes());
    }
    // SAFETY: host stream valid for the call.
    let s = unsafe { &*s };
    let Some(write) = s.write else { return false };
    let len = bytes.len() as i64;
    unsafe { write(s, bytes.as_ptr() as *const c_void, len as u64) == len }
}
unsafe extern "C" fn state_load(p: *const clap_plugin, s: *const clap_istream) -> bool {
    let me = this(p);
    let len = if me.instrument { 16 } else { 8 };
    let mut bytes = [0u8; 16];
    // SAFETY: host stream valid for the call.
    let s = unsafe { &*s };
    let Some(read) = s.read else { return false };
    let mut got = 0;
    while got < len {
        let n = unsafe {
            read(
                s,
                bytes[got..].as_mut_ptr() as *mut c_void,
                (len - got) as u64,
            )
        };
        if n <= 0 {
            return false;
        }
        got += n as usize;
    }
    let mut generation = [0u8; 8];
    generation.copy_from_slice(&bytes[..8]);
    me.generation = u64::from_le_bytes(generation);
    if me.instrument {
        let mut gain = [0u8; 8];
        gain.copy_from_slice(&bytes[8..16]);
        let gain = f64::from_le_bytes(gain);
        if !(0.0..=2.0).contains(&gain) {
            return false;
        }
        me.gain = gain;
    }
    true
}

unsafe extern "C" fn note_ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    u32::from(is_input && this(p).instrument)
}
unsafe extern "C" fn note_ports_get(
    _p: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if index != 0 || !is_input {
        return false;
    }
    // SAFETY: host-provided out parameter.
    let info = unsafe { &mut *info };
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    info.name = [0; 256];
    for (d, s) in info.name.iter_mut().zip(b"Notes") {
        *d = *s as c_char;
    }
    true
}

unsafe extern "C" fn params_count(_p: *const clap_plugin) -> u32 {
    1
}
unsafe extern "C" fn params_info(
    _p: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    if index != 0 {
        return false;
    }
    // SAFETY: host-provided out parameter.
    let info = unsafe { &mut *info };
    info.id = GAIN_PARAM;
    info.flags = CLAP_PARAM_IS_AUTOMATABLE;
    info.cookie = ptr::null_mut();
    info.name = [0; 256];
    for (d, s) in info.name.iter_mut().zip(b"Gain") {
        *d = *s as c_char;
    }
    info.module = [0; 1024];
    info.min_value = 0.0;
    info.max_value = 2.0;
    info.default_value = 1.0;
    true
}
unsafe extern "C" fn params_value(p: *const clap_plugin, id: u32, value: *mut f64) -> bool {
    if id != GAIN_PARAM {
        return false;
    }
    // SAFETY: host-provided out parameter.
    unsafe { *value = this(p).gain };
    true
}
unsafe extern "C" fn params_flush(
    p: *const clap_plugin,
    ins: *const clap_input_events,
    out: *const clap_output_events,
) {
    let me = this(p);
    // SAFETY: the host passes valid event lists for the call.
    unsafe {
        let ins = &*ins;
        let count = ins.size.map_or(0, |size| size(ins));
        for i in 0..count {
            let Some(get) = ins.get else { break };
            let header = get(ins, i);
            if !header.is_null() && (*header).type_ == CLAP_EVENT_PARAM_VALUE {
                apply_event(me, header, out);
            }
        }
    }
}

static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(ports_count),
    get: Some(ports_get),
};
static STATE: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};
static NOTE_PORTS: clap_plugin_note_ports = clap_plugin_note_ports {
    count: Some(note_ports_count),
    get: Some(note_ports_get),
};
static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_info),
    get_value: Some(params_value),
    value_to_text: None,
    text_to_value: None,
    flush: Some(params_flush),
};

unsafe extern "C" fn get_extension(p: *const clap_plugin, id: *const c_char) -> *const c_void {
    // SAFETY: host passes a NUL-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    let instrument = this(p).instrument;
    if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE as *const _ as *const c_void
    } else if instrument && id == CLAP_EXT_NOTE_PORTS {
        &NOTE_PORTS as *const _ as *const c_void
    } else if instrument && id == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    PLUGINS.len() as u32
}
unsafe extern "C" fn factory_desc(
    _f: *const clap_plugin_factory,
    i: u32,
) -> *const clap_plugin_descriptor {
    PLUGINS
        .get(i as usize)
        .map_or(ptr::null(), |(d, _)| &d.0 as *const _)
}
unsafe extern "C" fn factory_create(
    _f: *const clap_plugin_factory,
    _host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    // SAFETY: host passes a NUL-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    let Some((d, fault)) = PLUGINS
        .iter()
        .find(|(d, _)| unsafe { CStr::from_ptr(d.0.id) } == id)
    else {
        return ptr::null();
    };
    let plugin = Box::into_raw(Box::new(Plugin {
        clap: clap_plugin {
            desc: &d.0,
            plugin_data: ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start_processing),
            stop_processing: Some(stop_processing),
            reset: Some(reset),
            process: Some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        fault: *fault,
        instrument: d.0.features == INSTRUMENT_FEATURE_LIST.0.as_ptr(),
        blocks: 0,
        generation: 0,
        gain: 1.0,
        voices: [None; VOICES],
    }));
    // SAFETY: freshly allocated; the clap_plugin is the struct's first field.
    unsafe {
        (*plugin).clap.plugin_data = plugin as *mut c_void;
        &(*plugin).clap
    }
}

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_desc),
    create_plugin: Some(factory_create),
};

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    // SAFETY: host passes a NUL-terminated id.
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const _ as *const c_void
    } else {
        ptr::null()
    }
}

#[unsafe(no_mangle)]
#[used]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};
