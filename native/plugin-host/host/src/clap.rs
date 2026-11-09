//! The CLAP FFI boundary. Every `unsafe` call into a plugin is in this file;
//! the rest of the helper sees owned Rust values. Ported from the #96 probe
//! (`native/docs/spikes/clap/host/src/clap.rs`), keeping what offline effect
//! rendering and the live instrument helper need: note ports, timed note and
//! parameter events in `process`, and the plugin's note-end reports.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_END, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON,
    CLAP_EVENT_PARAM_VALUE, clap_event_header, clap_event_note, clap_event_param_value,
    clap_input_events, clap_output_events,
};
use clap_sys::ext::audio_ports::{
    CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::note_ports::{CLAP_EXT_NOTE_PORTS, clap_note_port_info, clap_plugin_note_ports};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_param_info, clap_plugin_params};
use clap_sys::ext::render::{CLAP_EXT_RENDER, CLAP_RENDER_OFFLINE, clap_plugin_render};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_plugin_tail};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};

#[derive(Debug)]
pub enum HostError {
    BundleMissing,
    Dlopen(String),
    NoEntry(String),
    IncompatibleVersion(String),
    EntryInitFailed,
    NoFactory,
    UnknownPlugin(String),
    CreateFailed,
    InitFailed,
    ActivateFailed,
    StartProcessingFailed,
    StateRejected(String),
    StateSaveFailed,
    ParamRejected(String),
    Unsupported(String),
    ProcessFailed,
}

impl HostError {
    /// The protocol error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BundleMissing => "bundle-missing",
            Self::Dlopen(_) => "dlopen-failed",
            Self::NoEntry(_) => "no-entry",
            Self::IncompatibleVersion(_) => "incompatible-version",
            Self::EntryInitFailed => "entry-init-failed",
            Self::NoFactory => "no-plugin-factory",
            Self::UnknownPlugin(_) => "unknown-plugin-id",
            Self::CreateFailed => "create-failed",
            Self::InitFailed => "init-failed",
            Self::ActivateFailed => "activate-failed",
            Self::StartProcessingFailed => "start-processing-failed",
            Self::StateRejected(_) => "state-rejected",
            Self::StateSaveFailed => "state-save-failed",
            Self::ParamRejected(_) => "param-rejected",
            Self::Unsupported(_) => "unsupported",
            Self::ProcessFailed => "process-failed",
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dlopen(s)
            | Self::NoEntry(s)
            | Self::IncompatibleVersion(s)
            | Self::UnknownPlugin(s)
            | Self::StateRejected(s)
            | Self::ParamRejected(s)
            | Self::Unsupported(s) => write!(f, "{}: {s}", self.code()),
            _ => f.write_str(self.code()),
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
pub fn executable_path(bundle: &Path) -> Result<PathBuf, HostError> {
    if !bundle.exists() {
        return Err(HostError::BundleMissing);
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
    pub version: String,
}

/// A loaded plugin binary. `clap_entry.deinit` runs on drop. The library is
/// never unloaded: several plugin runtimes are not safe to `dlclose`, and the
/// helper process exits after one instance anyway.
pub struct Library {
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
    pub clap_version: (u32, u32, u32),
}

impl Library {
    pub fn open(bundle: &Path, executable: &Path) -> Result<Self, HostError> {
        // SAFETY: loading arbitrary code is the point of a plugin host; the
        // library's initializers run here, inside this disposable process.
        let lib = unsafe { libloading::Library::new(executable) }
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
        let path = CString::new(bundle.to_string_lossy().as_bytes())
            .map_err(|_| HostError::Dlopen("path contains NUL".into()))?;
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
            out.push(Descriptor {
                id: cstr_or_empty(d.id),
                name: cstr_or_empty(d.name),
                version: cstr_or_empty(d.version),
            });
        }
        out
    }

    /// Create and `init` a plugin. The caller has checked the ID exists.
    pub fn instantiate(&self, plugin_id: &str) -> Result<Instance, HostError> {
        let mut host = Box::new(host_shell());
        host.host_data = &*host as *const clap_host as *mut c_void;
        let id = CString::new(plugin_id).map_err(|_| HostError::UnknownPlugin(plugin_id.into()))?;
        // SAFETY: factory valid; host outlives the plugin (owned by Instance).
        let f = unsafe { &*self.factory };
        let create = f.create_plugin.ok_or(HostError::CreateFailed)?;
        let plugin = unsafe { create(self.factory, &*host, id.as_ptr()) };
        if plugin.is_null() {
            return Err(HostError::CreateFailed);
        }
        let inst = Instance {
            plugin,
            _host: host,
            active: false,
            processing: false,
            steady_time: 0,
        };
        // SAFETY: init is the first call after create.
        let init = inst.p().init.ok_or(HostError::InitFailed)?;
        if !unsafe { init(plugin) } {
            // A plugin that failed init is destroyed without activation; Drop does that.
            return Err(HostError::InitFailed);
        }
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

/// The helper offers no host extensions and ignores restart, process and
/// callback requests: an offline render has nothing to reschedule.
unsafe extern "C" fn host_get_extension(_h: *const clap_host, _id: *const c_char) -> *const c_void {
    ptr::null()
}
unsafe extern "C" fn host_request(_h: *const clap_host) {}

fn host_shell() -> clap_host {
    clap_host {
        clap_version: CLAP_VERSION,
        host_data: ptr::null_mut(),
        name: c"brownnote-plugin-host".as_ptr(),
        vendor: c"brownnote".as_ptr(),
        url: c"https://github.com/drawmeanelephant/brownnote".as_ptr(),
        version: c"0.1.0".as_ptr(),
        get_extension: Some(host_get_extension),
        request_restart: Some(host_request),
        request_process: Some(host_request),
        request_callback: Some(host_request),
    }
}

unsafe extern "C" fn in_size(list: *const clap_input_events) -> u32 {
    // SAFETY: ctx is the Vec<clap_event_param_value> built for this call.
    unsafe { (*((*list).ctx as *const Vec<clap_event_param_value>)).len() as u32 }
}
unsafe extern "C" fn in_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: as above; out-of-range indices return null per the spec.
    unsafe {
        let v = &*((*list).ctx as *const Vec<clap_event_param_value>);
        v.get(index as usize)
            .map_or(ptr::null(), |e| &e.header as *const _)
    }
}
unsafe extern "C" fn out_push(
    _list: *const clap_output_events,
    _ev: *const clap_event_header,
) -> bool {
    true
}

fn param_event(param_id: u32, value: f64) -> clap_event_param_value {
    clap_event_param_value {
        header: clap_event_header {
            size: size_of::<clap_event_param_value>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE,
            flags: 0,
        },
        param_id,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    }
}

fn no_events() -> (Vec<clap_event_param_value>, clap_output_events) {
    (
        Vec::new(),
        clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(out_push),
        },
    )
}

/// A timed input event for [`Instance::process_events`]. `time` is the frame
/// offset within the block; events must be in time order.
// Only the `brownnote-clap-live` binary constructs these; the offline
// binary compiles the same file.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputEvent {
    NoteOn {
        time: u32,
        note_id: i32,
        channel: i16,
        key: i16,
        velocity: f64,
    },
    NoteOff {
        time: u32,
        note_id: i32,
        channel: i16,
        key: i16,
        velocity: f64,
    },
    Param {
        time: u32,
        id: u32,
        value: f64,
    },
}

/// A `CLAP_EVENT_NOTE_END` the plugin reported from `process`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEnd {
    pub time: u32,
    pub note_id: i32,
    pub channel: i16,
    pub key: i16,
}

/// Every variant starts with the CLAP header, so a pointer to the union is a
/// valid `clap_event_header` pointer.
#[repr(C)]
#[derive(Clone, Copy)]
union RawEvent {
    header: clap_event_header,
    note: clap_event_note,
    param: clap_event_param_value,
}

fn raw_event(event: &InputEvent) -> RawEvent {
    let note = |type_, time, note_id, channel, key, velocity| RawEvent {
        note: clap_event_note {
            header: clap_event_header {
                size: size_of::<clap_event_note>() as u32,
                time,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_,
                flags: 0,
            },
            note_id,
            port_index: 0,
            channel,
            key,
            velocity,
        },
    };
    match *event {
        InputEvent::NoteOn {
            time,
            note_id,
            channel,
            key,
            velocity,
        } => note(CLAP_EVENT_NOTE_ON, time, note_id, channel, key, velocity),
        InputEvent::NoteOff {
            time,
            note_id,
            channel,
            key,
            velocity,
        } => note(CLAP_EVENT_NOTE_OFF, time, note_id, channel, key, velocity),
        InputEvent::Param { time, id, value } => {
            let mut param = param_event(id, value);
            param.header.time = time;
            RawEvent { param }
        }
    }
}

unsafe extern "C" fn raw_size(list: *const clap_input_events) -> u32 {
    // SAFETY: ctx is the Vec<RawEvent> built for this call.
    unsafe { (*((*list).ctx as *const Vec<RawEvent>)).len() as u32 }
}
unsafe extern "C" fn raw_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: as above; out-of-range indices return null per the spec.
    unsafe {
        let v = &*((*list).ctx as *const Vec<RawEvent>);
        v.get(index as usize).map_or(ptr::null(), |e| {
            e as *const RawEvent as *const clap_event_header
        })
    }
}
unsafe extern "C" fn collect_note_end(
    list: *const clap_output_events,
    ev: *const clap_event_header,
) -> bool {
    // SAFETY: ctx is the Vec<NoteEnd> for this call; the plugin passes an
    // event that is valid for the call, and its size is checked before the
    // note fields are read.
    unsafe {
        let header = &*ev;
        if header.space_id == CLAP_CORE_EVENT_SPACE_ID
            && header.type_ == CLAP_EVENT_NOTE_END
            && header.size as usize >= size_of::<clap_event_note>()
        {
            let note = &*(ev as *const clap_event_note);
            (*((*list).ctx as *mut Vec<NoteEnd>)).push(NoteEnd {
                time: header.time,
                note_id: note.note_id,
                channel: note.channel,
                key: note.key,
            });
        }
    }
    true
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub flags: u32,
}

pub struct Instance {
    plugin: *const clap_plugin,
    _host: Box<clap_host>,
    active: bool,
    processing: bool,
    steady_time: i64,
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

    /// `(supported, preferred)` note dialects of each note port.
    #[allow(dead_code)]
    pub fn note_ports(&self, is_input: bool) -> Vec<(u32, u32)> {
        let Some(ext) = self.ext::<clap_plugin_note_ports>(CLAP_EXT_NOTE_PORTS) else {
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
                let mut info: clap_note_port_info = unsafe { std::mem::zeroed() };
                unsafe { get(self.plugin, i, is_input, &mut info) }
                    .then_some((info.supported_dialects, info.preferred_dialect))
            })
            .collect()
    }

    /// Channel count of each audio port.
    pub fn audio_ports(&self, is_input: bool) -> Vec<u32> {
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
                unsafe { get(self.plugin, i, is_input, &mut info) }.then_some(info.channel_count)
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
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    flags: info.flags,
                })
            })
            .collect()
    }

    /// Apply parameter values outside `process`, as the spec allows when the
    /// plugin is not processing.
    pub fn flush_params(&mut self, values: &[(u32, f64)]) -> Result<(), HostError> {
        let ext = self
            .ext::<clap_plugin_params>(CLAP_EXT_PARAMS)
            .ok_or_else(|| HostError::ParamRejected("the plugin has no parameters".into()))?;
        let flush = ext
            .flush
            .ok_or_else(|| HostError::ParamRejected("the plugin cannot flush parameters".into()))?;
        let raw: Vec<clap_event_param_value> = values
            .iter()
            .map(|(id, value)| param_event(*id, *value))
            .collect();
        let ins = clap_input_events {
            ctx: &raw as *const _ as *mut c_void,
            size: Some(in_size),
            get: Some(in_get),
        };
        let (_, outs) = no_events();
        // SAFETY: lists and their storage outlive the call.
        unsafe { flush(self.plugin, &ins, &outs) };
        Ok(())
    }

    /// `None` when the plugin has no render extension.
    pub fn hard_realtime(&self) -> Option<bool> {
        let r = self.ext::<clap_plugin_render>(CLAP_EXT_RENDER)?;
        // SAFETY: main-thread query.
        r.has_hard_realtime_requirement
            .map(|f| unsafe { f(self.plugin) })
    }

    /// `None` when the plugin has no render extension.
    pub fn set_offline(&mut self) -> Option<bool> {
        let r = self.ext::<clap_plugin_render>(CLAP_EXT_RENDER)?;
        // SAFETY: main-thread call before processing starts.
        r.set
            .map(|f| unsafe { f(self.plugin, CLAP_RENDER_OFFLINE) })
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

    pub fn activate(&mut self, sample_rate: f64, max_frames: u32) -> Result<(), HostError> {
        let a = self.p().activate.ok_or(HostError::ActivateFailed)?;
        // SAFETY: main thread, instance initialised and inactive.
        if !unsafe { a(self.plugin, sample_rate, 1, max_frames) } {
            return Err(HostError::ActivateFailed);
        }
        self.active = true;
        Ok(())
    }

    pub fn deactivate(&mut self) {
        self.stop_processing();
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
        // SAFETY: active instance. The helper is single-threaded, so this
        // thread is the audio thread for the whole render.
        if !unsafe { s(self.plugin) } {
            return Err(HostError::StartProcessingFailed);
        }
        self.processing = true;
        Ok(())
    }

    pub fn stop_processing(&mut self) {
        if self.processing {
            if let Some(s) = self.p().stop_processing {
                // SAFETY: processing instance, same thread.
                unsafe { s(self.plugin) };
            }
            self.processing = false;
        }
    }

    /// One `clap_plugin.process` call. `input` and `output` are indexed
    /// `[port][channel][frame]`; every channel holds at least `frames` samples.
    pub fn process(
        &mut self,
        frames: u32,
        input: &mut [Vec<Vec<f32>>],
        output: &mut [Vec<Vec<f32>>],
    ) -> Result<(), HostError> {
        self.process_events(frames, &[], &mut Vec::new(), input, output)
    }

    /// [`Self::process`] with timed input events. Note ends the plugin reports
    /// are appended to `note_ends`; other output events are ignored.
    pub fn process_events(
        &mut self,
        frames: u32,
        events: &[InputEvent],
        note_ends: &mut Vec<NoteEnd>,
        input: &mut [Vec<Vec<f32>>],
        output: &mut [Vec<Vec<f32>>],
    ) -> Result<(), HostError> {
        let mut in_ptrs: Vec<Vec<*mut f32>> = input
            .iter_mut()
            .map(|p| p.iter_mut().map(|c| c.as_mut_ptr()).collect())
            .collect();
        let mut out_ptrs: Vec<Vec<*mut f32>> = output
            .iter_mut()
            .map(|p| p.iter_mut().map(|c| c.as_mut_ptr()).collect())
            .collect();
        let buffer = |p: &mut Vec<*mut f32>| clap_audio_buffer {
            data32: p.as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: p.len() as u32,
            latency: 0,
            constant_mask: 0,
        };
        let in_bufs: Vec<clap_audio_buffer> = in_ptrs.iter_mut().map(buffer).collect();
        let mut out_bufs: Vec<clap_audio_buffer> = out_ptrs.iter_mut().map(buffer).collect();
        let raw: Vec<RawEvent> = events.iter().map(raw_event).collect();
        let ins = clap_input_events {
            ctx: &raw as *const _ as *mut c_void,
            size: Some(raw_size),
            get: Some(raw_get),
        };
        let outs = clap_output_events {
            ctx: note_ends as *mut Vec<NoteEnd> as *mut c_void,
            try_push: Some(collect_note_end),
        };
        let process = clap_process {
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
            Some(p) => unsafe { p(self.plugin, &process) },
            None => CLAP_PROCESS_ERROR,
        };
        self.steady_time += i64::from(frames);
        if status == CLAP_PROCESS_ERROR {
            return Err(HostError::ProcessFailed);
        }
        Ok(())
    }

    /// `Ok(None)` when the plugin has no state extension.
    pub fn save_state(&self) -> Result<Option<Vec<u8>>, HostError> {
        let Some(ext) = self.ext::<clap_plugin_state>(CLAP_EXT_STATE) else {
            return Ok(None);
        };
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
        Ok(Some(buf.into_inner()))
    }

    /// Loads state, serving reads in small chunks so plugins that assume one
    /// big read fail here rather than misreading.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), HostError> {
        let ext = self
            .ext::<clap_plugin_state>(CLAP_EXT_STATE)
            .ok_or_else(|| HostError::StateRejected("the plugin has no state".into()))?;
        let load = ext
            .load
            .ok_or_else(|| HostError::StateRejected("the plugin cannot load state".into()))?;
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
            return Err(HostError::StateRejected(
                "the plugin refused the state".into(),
            ));
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
