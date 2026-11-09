//! Minimal CLAP host boundary used by the probe.
//!
//! Every `unsafe` block in the probe lives here. The rest of the crate only
//! sees owned Rust values. This file is evidence for how large the FFI surface
//! of a bounded host is, not a production host.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_CHOKE, CLAP_EVENT_NOTE_END, CLAP_EVENT_NOTE_OFF,
    CLAP_EVENT_NOTE_ON, CLAP_EVENT_PARAM_VALUE, clap_event_header, clap_event_note,
    clap_event_param_value, clap_input_events, clap_output_events,
};
use clap_sys::ext::audio_ports::{
    CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::note_ports::{CLAP_EXT_NOTE_PORTS, clap_note_port_info, clap_plugin_note_ports};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_param_info, clap_plugin_params};
use clap_sys::ext::render::{
    CLAP_EXT_RENDER, CLAP_RENDER_OFFLINE, CLAP_RENDER_REALTIME, clap_plugin_render,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_plugin_tail};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};

#[derive(Debug)]
pub enum HostError {
    BundleMissing(PathBuf),
    Dlopen(String),
    NoEntry(String),
    IncompatibleVersion(String),
    EntryInitFailed,
    NoFactory,
    UnknownPlugin(String),
    CreateFailed(String),
    InitFailed,
    ActivateFailed,
    StartProcessingFailed,
    MissingExtension(&'static str),
    StateSaveFailed,
    StateLoadFailed,
    UnknownParam(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BundleMissing(p) => write!(f, "{}: {}", self.kind(), p.display()),
            Self::Dlopen(s)
            | Self::NoEntry(s)
            | Self::IncompatibleVersion(s)
            | Self::UnknownPlugin(s)
            | Self::CreateFailed(s)
            | Self::UnknownParam(s) => write!(f, "{}: {s}", self.kind()),
            Self::MissingExtension(s) => write!(f, "{}: {s}", self.kind()),
            _ => f.write_str(self.kind()),
        }
    }
}

impl HostError {
    /// Stable machine-readable class for traces and the checker.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::BundleMissing(_) => "bundle-missing",
            Self::Dlopen(_) => "dlopen-failed",
            Self::NoEntry(_) => "no-clap-entry",
            Self::IncompatibleVersion(_) => "incompatible-version",
            Self::EntryInitFailed => "entry-init-failed",
            Self::NoFactory => "no-plugin-factory",
            Self::UnknownPlugin(_) => "unknown-plugin-id",
            Self::CreateFailed(_) => "create-failed",
            Self::InitFailed => "init-failed",
            Self::ActivateFailed => "activate-failed",
            Self::StartProcessingFailed => "start-processing-failed",
            Self::MissingExtension(_) => "missing-extension",
            Self::StateSaveFailed => "state-save-failed",
            Self::StateLoadFailed => "state-load-failed",
            Self::UnknownParam(_) => "unknown-param",
        }
    }
}

fn cstr_or_empty(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: CLAP strings are NUL-terminated and owned by the plugin for the
    // lifetime of the descriptor or info struct being read.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn fixed_str(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Resolve a `.clap` bundle directory (macOS) or a plain shared library.
fn executable_path(bundle: &Path) -> Result<PathBuf, HostError> {
    if !bundle.exists() {
        return Err(HostError::BundleMissing(bundle.to_path_buf()));
    }
    if bundle.is_dir() {
        let macos = bundle.join("Contents/MacOS");
        let entries = std::fs::read_dir(&macos)
            .map_err(|e| HostError::Dlopen(format!("{}: {e}", macos.display())))?;
        let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        return files
            .into_iter()
            .next()
            .ok_or_else(|| HostError::Dlopen(format!("{}: empty", macos.display())));
    }
    Ok(bundle.to_path_buf())
}

#[derive(Clone, Debug)]
pub struct Descriptor {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub features: Vec<String>,
}

/// A loaded plugin binary. `clap_entry.deinit` runs on drop. The dynamic
/// library itself is intentionally never unloaded: several plugin runtimes
/// (Rust thread-locals, JUCE singletons) are not safe to `dlclose`, and a
/// probe that leaks one mapping per bundle is cheaper than chasing that.
pub struct Library {
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
    pub executable: PathBuf,
    pub clap_version: (u32, u32, u32),
}

impl Library {
    pub fn open(bundle: &Path) -> Result<Self, HostError> {
        let executable = executable_path(bundle)?;
        // SAFETY: loading arbitrary code is the point of a plugin host; the
        // library's initializers run here.
        let lib = unsafe { libloading::Library::new(&executable) }
            .map_err(|e| HostError::Dlopen(e.to_string()))?;
        // SAFETY: `clap_entry` is a data symbol of type `clap_plugin_entry`.
        let entry: *const clap_plugin_entry = unsafe {
            let sym = lib
                .get::<*const clap_plugin_entry>(b"clap_entry\0")
                .map_err(|e| HostError::NoEntry(e.to_string()))?;
            *sym
        };
        std::mem::forget(lib);
        if entry.is_null() {
            return Err(HostError::NoEntry("null clap_entry".into()));
        }
        // SAFETY: entry points into the loaded image and stays mapped.
        let e = unsafe { &*entry };
        let v = e.clap_version;
        if !clap_version_is_compatible(v) {
            return Err(HostError::IncompatibleVersion(format!(
                "{}.{}.{}",
                v.major, v.minor, v.revision
            )));
        }
        let path = CString::new(bundle.to_string_lossy().as_bytes()).expect("path has NUL");
        let init = e.init.ok_or(HostError::EntryInitFailed)?;
        // SAFETY: called once, on the main thread, before any other entry call.
        if !unsafe { init(path.as_ptr()) } {
            return Err(HostError::EntryInitFailed);
        }
        let get_factory = e.get_factory.ok_or(HostError::NoFactory)?;
        // SAFETY: entry initialised above.
        let factory =
            unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) } as *const clap_plugin_factory;
        if factory.is_null() {
            return Err(HostError::NoFactory);
        }
        Ok(Self {
            entry,
            factory,
            executable,
            clap_version: (v.major, v.minor, v.revision),
        })
    }

    pub fn descriptors(&self) -> Vec<Descriptor> {
        // SAFETY: factory is valid while the entry is initialised.
        let f = unsafe { &*self.factory };
        let count = f
            .get_plugin_count
            .map(|c| unsafe { c(self.factory) })
            .unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..count {
            let Some(get) = f.get_plugin_descriptor else {
                break;
            };
            // SAFETY: index is below the reported count.
            let d = unsafe { get(self.factory, i) };
            if d.is_null() {
                continue;
            }
            // SAFETY: non-null descriptor owned by the factory.
            let d = unsafe { &*d };
            let mut features = Vec::new();
            if !d.features.is_null() {
                let mut p = d.features;
                // SAFETY: features is a NULL-terminated array of C strings.
                unsafe {
                    while !(*p).is_null() {
                        features.push(cstr_or_empty(*p));
                        p = p.add(1);
                    }
                }
            }
            out.push(Descriptor {
                id: cstr_or_empty(d.id),
                name: cstr_or_empty(d.name),
                vendor: cstr_or_empty(d.vendor),
                version: cstr_or_empty(d.version),
                features,
            });
        }
        out
    }

    pub fn instantiate(&self, plugin_id: &str) -> Result<Instance, HostError> {
        if !self.descriptors().iter().any(|d| d.id == plugin_id) {
            return Err(HostError::UnknownPlugin(plugin_id.into()));
        }
        let mut host = Box::new(HostShell::new());
        host.clap.host_data = &*host as *const HostShell as *mut c_void;
        let id = CString::new(plugin_id).expect("id has NUL");
        // SAFETY: factory valid; host outlives the plugin (owned by Instance).
        let f = unsafe { &*self.factory };
        let create = f
            .create_plugin
            .ok_or(HostError::CreateFailed(plugin_id.into()))?;
        let plugin = unsafe { create(self.factory, &host.clap, id.as_ptr()) };
        if plugin.is_null() {
            return Err(HostError::CreateFailed(plugin_id.into()));
        }
        let mut inst = Instance {
            plugin,
            host,
            active: false,
            processing: false,
            inputs: Vec::new(),
            outputs: Vec::new(),
            steady_time: 0,
        };
        // SAFETY: init is the first call after create.
        let init = inst.p().init.ok_or(HostError::InitFailed)?;
        if !unsafe { init(plugin) } {
            // Per the spec a plugin that failed init is destroyed without
            // activation. Drop does exactly that.
            return Err(HostError::InitFailed);
        }
        let (inputs, outputs) = inst.audio_port_channels();
        inst.inputs = inputs;
        inst.outputs = outputs;
        Ok(inst)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: matches the successful init in `open`.
        unsafe {
            if let Some(deinit) = (*self.entry).deinit {
                deinit();
            }
        }
    }
}

/// Host-side callbacks. Requests are counted so traces can show them; the
/// probe never services them asynchronously.
#[repr(C)]
struct HostShell {
    clap: clap_host,
    restart_requests: AtomicU32,
    process_requests: AtomicU32,
    callback_requests: AtomicU32,
}

unsafe extern "C" fn host_get_extension(_h: *const clap_host, _id: *const c_char) -> *const c_void {
    ptr::null()
}
unsafe extern "C" fn host_request_restart(h: *const clap_host) {
    // SAFETY: host_data was set to the owning HostShell.
    unsafe {
        (*((*h).host_data as *const HostShell))
            .restart_requests
            .fetch_add(1, Ordering::Relaxed)
    };
}
unsafe extern "C" fn host_request_process(h: *const clap_host) {
    // SAFETY: as above.
    unsafe {
        (*((*h).host_data as *const HostShell))
            .process_requests
            .fetch_add(1, Ordering::Relaxed)
    };
}
unsafe extern "C" fn host_request_callback(h: *const clap_host) {
    // SAFETY: as above.
    unsafe {
        (*((*h).host_data as *const HostShell))
            .callback_requests
            .fetch_add(1, Ordering::Relaxed)
    };
}

impl HostShell {
    fn new() -> Self {
        Self {
            clap: clap_host {
                clap_version: CLAP_VERSION,
                host_data: ptr::null_mut(),
                name: c"brownnote-clap-probe".as_ptr(),
                vendor: c"brownnote".as_ptr(),
                url: c"https://github.com/drawmeanelephant/brownnote".as_ptr(),
                version: c"0.1.0".as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
            restart_requests: AtomicU32::new(0),
            process_requests: AtomicU32::new(0),
            callback_requests: AtomicU32::new(0),
        }
    }
}

/// An event the host sends into `process` or `params.flush`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InEvent {
    NoteOn {
        time: u32,
        note_id: i32,
        key: i16,
        velocity: f64,
    },
    NoteOff {
        time: u32,
        note_id: i32,
        key: i16,
    },
    Param {
        time: u32,
        param_id: u32,
        value: f64,
    },
}

impl InEvent {
    pub fn time(&self) -> u32 {
        match *self {
            Self::NoteOn { time, .. } | Self::NoteOff { time, .. } | Self::Param { time, .. } => {
                time
            }
        }
    }
}

/// An event the plugin reported back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutEvent {
    pub time: u32,
    pub kind: &'static str,
    pub note_id: i32,
    pub key: i16,
    pub value: f64,
}

#[repr(C)]
union RawEvent {
    header: clap_event_header,
    note: clap_event_note,
    param: clap_event_param_value,
}

fn header(type_: u16, size: usize, time: u32) -> clap_event_header {
    clap_event_header {
        size: size as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

fn encode(e: &InEvent) -> RawEvent {
    match *e {
        InEvent::NoteOn {
            time,
            note_id,
            key,
            velocity,
        } => RawEvent {
            note: clap_event_note {
                header: header(CLAP_EVENT_NOTE_ON, size_of::<clap_event_note>(), time),
                note_id,
                port_index: 0,
                channel: 0,
                key,
                velocity,
            },
        },
        InEvent::NoteOff { time, note_id, key } => RawEvent {
            note: clap_event_note {
                header: header(CLAP_EVENT_NOTE_OFF, size_of::<clap_event_note>(), time),
                note_id,
                port_index: 0,
                channel: 0,
                key,
                velocity: 0.0,
            },
        },
        InEvent::Param {
            time,
            param_id,
            value,
        } => RawEvent {
            param: clap_event_param_value {
                header: header(
                    CLAP_EVENT_PARAM_VALUE,
                    size_of::<clap_event_param_value>(),
                    time,
                ),
                param_id,
                cookie: ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            },
        },
    }
}

unsafe extern "C" fn in_size(list: *const clap_input_events) -> u32 {
    // SAFETY: ctx is the Vec<RawEvent> built for this call.
    unsafe { (*((*list).ctx as *const Vec<RawEvent>)).len() as u32 }
}
unsafe extern "C" fn in_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: as above; out-of-range indices return null per the spec.
    unsafe {
        let v = &*((*list).ctx as *const Vec<RawEvent>);
        v.get(index as usize)
            .map_or(ptr::null(), |e| &e.header as *const _)
    }
}
unsafe extern "C" fn out_push(
    list: *const clap_output_events,
    ev: *const clap_event_header,
) -> bool {
    // SAFETY: ctx is the RefCell<Vec<OutEvent>> owned by the caller frame;
    // the plugin hands us a valid header for the duration of the call.
    unsafe {
        let out = &*((*list).ctx as *const RefCell<Vec<OutEvent>>);
        let h = &*ev;
        if h.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return true;
        }
        let kind = match h.type_ {
            CLAP_EVENT_NOTE_ON => "note-on",
            CLAP_EVENT_NOTE_OFF => "note-off",
            CLAP_EVENT_NOTE_CHOKE => "note-choke",
            CLAP_EVENT_NOTE_END => "note-end",
            CLAP_EVENT_PARAM_VALUE => "param-value",
            _ => "other",
        };
        let (note_id, key, value) = match h.type_ {
            CLAP_EVENT_NOTE_ON
            | CLAP_EVENT_NOTE_OFF
            | CLAP_EVENT_NOTE_CHOKE
            | CLAP_EVENT_NOTE_END => {
                let n = &*(ev as *const clap_event_note);
                (n.note_id, n.key, n.velocity)
            }
            CLAP_EVENT_PARAM_VALUE => {
                let p = &*(ev as *const clap_event_param_value);
                (-1, -1, p.value)
            }
            _ => (-1, -1, 0.0),
        };
        out.borrow_mut().push(OutEvent {
            time: h.time,
            kind,
            note_id,
            key,
            value,
        });
        true
    }
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub flags: u32,
}

#[derive(Clone, Debug)]
pub struct PortInfo {
    pub name: String,
    pub channels: u32,
    pub flags: u32,
}

#[derive(Clone, Debug)]
pub struct NotePortInfo {
    pub name: String,
    pub supported_dialects: u32,
    pub preferred_dialect: u32,
}

pub struct Instance {
    plugin: *const clap_plugin,
    host: Box<HostShell>,
    active: bool,
    processing: bool,
    /// Channel count per input / output audio port.
    pub inputs: Vec<u32>,
    pub outputs: Vec<u32>,
    steady_time: i64,
}

// The probe moves an instance to a dedicated audio thread for the live run
// and back. Calls are never concurrent; the main/audio thread split is
// enforced by the caller joining the thread before touching it again.
unsafe impl Send for Instance {}

pub struct ProcessResult {
    pub status: i32,
    pub events: Vec<OutEvent>,
}

impl Instance {
    fn p(&self) -> &clap_plugin {
        // SAFETY: plugin is valid until destroy in Drop.
        unsafe { &*self.plugin }
    }

    fn ext<T>(&self, id: &CStr) -> Option<&T> {
        let get = self.p().get_extension?;
        // SAFETY: extension pointers stay valid for the instance's lifetime.
        let p = unsafe { get(self.plugin, id.as_ptr()) } as *const T;
        if p.is_null() {
            None
        } else {
            Some(unsafe { &*p })
        }
    }

    pub fn host_requests(&self) -> (u32, u32, u32) {
        (
            self.host.restart_requests.load(Ordering::Relaxed),
            self.host.process_requests.load(Ordering::Relaxed),
            self.host.callback_requests.load(Ordering::Relaxed),
        )
    }

    pub fn extensions(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let probe: [(&CStr, &'static str); 8] = [
            (CLAP_EXT_AUDIO_PORTS, "audio-ports"),
            (CLAP_EXT_NOTE_PORTS, "note-ports"),
            (CLAP_EXT_PARAMS, "params"),
            (CLAP_EXT_STATE, "state"),
            (CLAP_EXT_RENDER, "render"),
            (CLAP_EXT_TAIL, "tail"),
            (CLAP_EXT_LATENCY, "latency"),
            (c"clap.voice-info", "voice-info"),
        ];
        for (id, name) in probe {
            if self.ext::<c_void>(id).is_some() {
                out.push(name);
            }
        }
        out
    }

    fn audio_port_channels(&self) -> (Vec<u32>, Vec<u32>) {
        (
            self.audio_ports(true).iter().map(|p| p.channels).collect(),
            self.audio_ports(false).iter().map(|p| p.channels).collect(),
        )
    }

    pub fn audio_ports(&self, is_input: bool) -> Vec<PortInfo> {
        let Some(ext) = self.ext::<clap_plugin_audio_ports>(CLAP_EXT_AUDIO_PORTS) else {
            return Vec::new();
        };
        let (Some(count), Some(get)) = (ext.count, ext.get) else {
            return Vec::new();
        };
        // SAFETY: main-thread queries on a valid instance.
        let n = unsafe { count(self.plugin, is_input) };
        (0..n)
            .filter_map(|i| {
                // SAFETY: zeroed info is a valid out-parameter.
                let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
                unsafe { get(self.plugin, i, is_input, &mut info) }.then(|| PortInfo {
                    name: fixed_str(&info.name),
                    channels: info.channel_count,
                    flags: info.flags,
                })
            })
            .collect()
    }

    pub fn note_ports(&self, is_input: bool) -> Vec<NotePortInfo> {
        let Some(ext) = self.ext::<clap_plugin_note_ports>(CLAP_EXT_NOTE_PORTS) else {
            return Vec::new();
        };
        let (Some(count), Some(get)) = (ext.count, ext.get) else {
            return Vec::new();
        };
        // SAFETY: as audio_ports.
        let n = unsafe { count(self.plugin, is_input) };
        (0..n)
            .filter_map(|i| {
                let mut info: clap_note_port_info = unsafe { std::mem::zeroed() };
                unsafe { get(self.plugin, i, is_input, &mut info) }.then(|| NotePortInfo {
                    name: fixed_str(&info.name),
                    supported_dialects: info.supported_dialects,
                    preferred_dialect: info.preferred_dialect,
                })
            })
            .collect()
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        let Some(ext) = self.ext::<clap_plugin_params>(CLAP_EXT_PARAMS) else {
            return Vec::new();
        };
        let (Some(count), Some(get)) = (ext.count, ext.get_info) else {
            return Vec::new();
        };
        // SAFETY: as audio_ports.
        let n = unsafe { count(self.plugin) };
        (0..n)
            .filter_map(|i| {
                let mut info: clap_param_info = unsafe { std::mem::zeroed() };
                unsafe { get(self.plugin, i, &mut info) }.then(|| ParamInfo {
                    id: info.id,
                    name: fixed_str(&info.name),
                    module: fixed_str(&info.module),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    flags: info.flags,
                })
            })
            .collect()
    }

    pub fn param_id(&self, name: &str) -> Result<u32, HostError> {
        self.params()
            .into_iter()
            .find(|p| p.name == name)
            .map(|p| p.id)
            .ok_or_else(|| HostError::UnknownParam(name.into()))
    }

    pub fn param_value(&self, id: u32) -> Option<f64> {
        let ext = self.ext::<clap_plugin_params>(CLAP_EXT_PARAMS)?;
        let get = ext.get_value?;
        let mut v = 0.0;
        // SAFETY: main-thread query.
        unsafe { get(self.plugin, id, &mut v) }.then_some(v)
    }

    /// Apply parameter events outside `process`, as the spec allows when the
    /// plugin is not processing.
    pub fn flush_params(&mut self, events: &[InEvent]) -> Result<Vec<OutEvent>, HostError> {
        let ext = self
            .ext::<clap_plugin_params>(CLAP_EXT_PARAMS)
            .ok_or(HostError::MissingExtension("params"))?;
        let flush = ext
            .flush
            .ok_or(HostError::MissingExtension("params.flush"))?;
        let raw: Vec<RawEvent> = events.iter().map(encode).collect();
        let sink = RefCell::new(Vec::new());
        let ins = clap_input_events {
            ctx: &raw as *const _ as *mut c_void,
            size: Some(in_size),
            get: Some(in_get),
        };
        let outs = clap_output_events {
            ctx: &sink as *const _ as *mut c_void,
            try_push: Some(out_push),
        };
        // SAFETY: lists and their storage outlive the call.
        unsafe { flush(self.plugin, &ins, &outs) };
        Ok(sink.into_inner())
    }

    pub fn render_ext(&self) -> Option<bool> {
        let r = self.ext::<clap_plugin_render>(CLAP_EXT_RENDER)?;
        // SAFETY: main-thread query.
        r.has_hard_realtime_requirement
            .map(|f| unsafe { f(self.plugin) })
    }

    /// Returns `None` when the plugin has no render extension.
    pub fn set_render_mode(&mut self, offline: bool) -> Option<bool> {
        let r = self.ext::<clap_plugin_render>(CLAP_EXT_RENDER)?;
        let mode = if offline {
            CLAP_RENDER_OFFLINE
        } else {
            CLAP_RENDER_REALTIME
        };
        // SAFETY: main-thread call, allowed while active (spec: not while processing is
        // implied by the main-thread requirement; the probe calls it before start).
        r.set.map(|f| unsafe { f(self.plugin, mode) })
    }

    pub fn latency(&self) -> Option<u32> {
        let l = self.ext::<clap_plugin_latency>(CLAP_EXT_LATENCY)?;
        // SAFETY: main-thread query while active.
        l.get.map(|f| unsafe { f(self.plugin) })
    }

    pub fn tail(&self) -> Option<u32> {
        let t = self.ext::<clap_plugin_tail>(CLAP_EXT_TAIL)?;
        // SAFETY: tail.get is [main-thread,audio-thread].
        t.get.map(|f| unsafe { f(self.plugin) })
    }

    pub fn activate(
        &mut self,
        sample_rate: f64,
        min_frames: u32,
        max_frames: u32,
    ) -> Result<(), HostError> {
        let a = self.p().activate.ok_or(HostError::ActivateFailed)?;
        // SAFETY: main thread, instance initialised and inactive.
        if !unsafe { a(self.plugin, sample_rate, min_frames, max_frames) } {
            return Err(HostError::ActivateFailed);
        }
        self.active = true;
        Ok(())
    }

    pub fn deactivate(&mut self) {
        if self.processing {
            self.stop_processing();
        }
        if self.active {
            if let Some(d) = self.p().deactivate {
                // SAFETY: active instance, main thread.
                unsafe { d(self.plugin) };
            }
            self.active = false;
        }
    }

    pub fn start_processing(&mut self) -> Result<(), HostError> {
        let s = self
            .p()
            .start_processing
            .ok_or(HostError::StartProcessingFailed)?;
        // SAFETY: active instance, audio thread (the caller's current thread).
        if !unsafe { s(self.plugin) } {
            return Err(HostError::StartProcessingFailed);
        }
        self.processing = true;
        Ok(())
    }

    pub fn stop_processing(&mut self) {
        if self.processing {
            if let Some(s) = self.p().stop_processing {
                // SAFETY: processing instance, audio thread.
                unsafe { s(self.plugin) };
            }
            self.processing = false;
        }
    }

    pub fn reset(&mut self) {
        if let Some(r) = self.p().reset {
            // SAFETY: active instance, audio thread.
            unsafe { r(self.plugin) };
        }
        self.steady_time = 0;
    }

    /// One `clap_plugin.process` call. `input` and `output` are indexed
    /// `[port][channel][frame]`; every channel must hold `frames` samples.
    pub fn process(
        &mut self,
        frames: u32,
        input: &mut [Vec<Vec<f32>>],
        output: &mut [Vec<Vec<f32>>],
        events: &[InEvent],
    ) -> ProcessResult {
        let mut in_ptrs: Vec<Vec<*mut f32>> = input
            .iter_mut()
            .map(|p| p.iter_mut().map(|c| c.as_mut_ptr()).collect())
            .collect();
        let mut out_ptrs: Vec<Vec<*mut f32>> = output
            .iter_mut()
            .map(|p| p.iter_mut().map(|c| c.as_mut_ptr()).collect())
            .collect();
        let in_bufs: Vec<clap_audio_buffer> = in_ptrs
            .iter_mut()
            .map(|p| clap_audio_buffer {
                data32: p.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: p.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();
        let mut out_bufs: Vec<clap_audio_buffer> = out_ptrs
            .iter_mut()
            .map(|p| clap_audio_buffer {
                data32: p.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: p.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();
        let raw: Vec<RawEvent> = events.iter().map(encode).collect();
        let sink = RefCell::new(Vec::new());
        let ins = clap_input_events {
            ctx: &raw as *const _ as *mut c_void,
            size: Some(in_size),
            get: Some(in_get),
        };
        let outs = clap_output_events {
            ctx: &sink as *const _ as *mut c_void,
            try_push: Some(out_push),
        };
        let proc_ = clap_process {
            steady_time: self.steady_time,
            frames_count: frames,
            transport: ptr::null(),
            audio_inputs: in_bufs.as_ptr(),
            audio_outputs: out_bufs.as_mut_ptr(),
            audio_inputs_count: in_bufs.len() as u32,
            audio_outputs_count: out_bufs.len() as u32,
            in_events: &ins,
            out_events: &outs,
        };
        let status = match self.p().process {
            // SAFETY: buffers, events and lists outlive the call; the
            // instance is processing on this thread.
            Some(p) => unsafe { p(self.plugin, &proc_) },
            None => 0,
        };
        self.steady_time += i64::from(frames);
        ProcessResult {
            status,
            events: sink.into_inner(),
        }
    }

    pub fn save_state(&self) -> Result<Vec<u8>, HostError> {
        let ext = self
            .ext::<clap_plugin_state>(CLAP_EXT_STATE)
            .ok_or(HostError::MissingExtension("state"))?;
        let save = ext.save.ok_or(HostError::StateSaveFailed)?;
        let buf = RefCell::new(Vec::<u8>::new());
        unsafe extern "C" fn write(s: *const clap_ostream, data: *const c_void, size: u64) -> i64 {
            // SAFETY: ctx is the RefCell above; data has `size` readable bytes.
            unsafe {
                let b = &*((*s).ctx as *const RefCell<Vec<u8>>);
                b.borrow_mut().extend_from_slice(std::slice::from_raw_parts(
                    data as *const u8,
                    size as usize,
                ));
            }
            size as i64
        }
        let stream = clap_ostream {
            ctx: &buf as *const _ as *mut c_void,
            write: Some(write),
        };
        // SAFETY: stream outlives the call.
        if !unsafe { save(self.plugin, &stream) } {
            return Err(HostError::StateSaveFailed);
        }
        Ok(buf.into_inner())
    }

    /// Loads state, serving reads in deliberately small chunks so plugins
    /// that assume one big read are caught.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), HostError> {
        let ext = self
            .ext::<clap_plugin_state>(CLAP_EXT_STATE)
            .ok_or(HostError::MissingExtension("state"))?;
        let load = ext.load.ok_or(HostError::StateLoadFailed)?;
        struct Cursor<'a> {
            data: &'a [u8],
            pos: usize,
        }
        let cursor = RefCell::new(Cursor {
            data: bytes,
            pos: 0,
        });
        unsafe extern "C" fn read(s: *const clap_istream, out: *mut c_void, size: u64) -> i64 {
            // SAFETY: ctx is the RefCell above; out has `size` writable bytes.
            unsafe {
                let c = &*((*s).ctx as *const RefCell<Cursor>);
                let mut c = c.borrow_mut();
                let n = (size as usize).min(c.data.len() - c.pos).min(4096);
                ptr::copy_nonoverlapping(c.data.as_ptr().add(c.pos), out as *mut u8, n);
                c.pos += n;
                n as i64
            }
        }
        let stream = clap_istream {
            ctx: &cursor as *const _ as *mut c_void,
            read: Some(read),
        };
        // SAFETY: stream outlives the call.
        if !unsafe { load(self.plugin, &stream) } {
            return Err(HostError::StateLoadFailed);
        }
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.deactivate();
        if let Some(d) = self.p().destroy {
            // SAFETY: last call on this instance.
            unsafe { d(self.plugin) };
        }
    }
}
