//! Deliberately faulty CLAP plugins for the offline helper tests
//! (`native/tests/clap_offline.rs`), carried over from the #96 failure probe.
//!
//! One bundle exposes several stereo pass-through effects. Each misbehaves in
//! one documented way once it has processed `FAULT_BLOCK` blocks since
//! activation. They exist to exercise host failure handling; nothing here is
//! a model for a real plugin.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, CLAP_PORT_STEREO, clap_audio_port_info,
    clap_plugin_audio_ports,
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
struct Features([*const c_char; 3]);
// SAFETY: as Desc.
unsafe impl Sync for Features {}
static FEATURE_LIST: Features = Features(FEATURES);

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

static PLUGINS: [(Desc, Fault); 6] = [
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
];

struct Plugin {
    clap: clap_plugin,
    fault: Fault,
    blocks: u32,
    /// Opaque state: how many times state was saved from this lineage.
    generation: u64,
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
    me.fault != Fault::ActivateFails
}
unsafe extern "C" fn deactivate(_p: *const clap_plugin) {}
unsafe extern "C" fn start_processing(_p: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn stop_processing(_p: *const clap_plugin) {}
unsafe extern "C" fn reset(_p: *const clap_plugin) {}
unsafe extern "C" fn on_main_thread(_p: *const clap_plugin) {}

unsafe extern "C" fn process(
    p: *const clap_plugin,
    pr: *const clap_process,
) -> clap_process_status {
    let me = this(p);
    // SAFETY: the host passes a valid process struct for the call.
    let pr = unsafe { &*pr };
    if pr.audio_inputs_count < 1 || pr.audio_outputs_count < 1 {
        return CLAP_PROCESS_ERROR;
    }
    let fire = me.blocks >= FAULT_BLOCK;
    me.blocks += 1;
    if fire {
        match me.fault {
            Fault::Crash => {
                // SAFETY: intentionally invalid; this is the fault under test.
                unsafe { ptr::write_volatile(ptr::null_mut::<u32>(), 0xdead) };
            }
            Fault::Hang => loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            },
            _ => {}
        }
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

unsafe extern "C" fn ports_count(_p: *const clap_plugin, _is_input: bool) -> u32 {
    1
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
    let bytes = me.generation.to_le_bytes();
    // SAFETY: host stream valid for the call.
    let s = unsafe { &*s };
    let Some(write) = s.write else { return false };
    unsafe { write(s, bytes.as_ptr() as *const c_void, 8) == 8 }
}
unsafe extern "C" fn state_load(p: *const clap_plugin, s: *const clap_istream) -> bool {
    let mut bytes = [0u8; 8];
    // SAFETY: host stream valid for the call.
    let s = unsafe { &*s };
    let Some(read) = s.read else { return false };
    if unsafe { read(s, bytes.as_mut_ptr() as *mut c_void, 8) } != 8 {
        return false;
    }
    this(p).generation = u64::from_le_bytes(bytes);
    true
}

static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(ports_count),
    get: Some(ports_get),
};
static STATE: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

unsafe extern "C" fn get_extension(_p: *const clap_plugin, id: *const c_char) -> *const c_void {
    // SAFETY: host passes a NUL-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_AUDIO_PORTS {
        &AUDIO_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE as *const _ as *const c_void
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
        blocks: 0,
        generation: 0,
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
