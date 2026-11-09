//! Minimal LADSPA 1.1 host side: library loading, descriptor validation and
//! port-hint resolution. Layouts follow `ladspa.h` from the LADSPA SDK.

use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub type Data = f32;
pub type Handle = *mut c_void;

pub const PROPERTY_REALTIME: c_int = 0x1;
pub const PROPERTY_INPLACE_BROKEN: c_int = 0x2;
pub const PROPERTY_HARD_RT_CAPABLE: c_int = 0x4;

const PORT_INPUT: c_int = 0x1;
const PORT_OUTPUT: c_int = 0x2;
const PORT_CONTROL: c_int = 0x4;
const PORT_AUDIO: c_int = 0x8;

const HINT_BOUNDED_BELOW: c_int = 0x1;
const HINT_BOUNDED_ABOVE: c_int = 0x2;
const HINT_TOGGLED: c_int = 0x4;
const HINT_SAMPLE_RATE: c_int = 0x8;
const HINT_LOGARITHMIC: c_int = 0x10;
const HINT_INTEGER: c_int = 0x20;
const HINT_DEFAULT_MASK: c_int = 0x3C0;
const HINT_DEFAULT_NONE: c_int = 0x0;
const HINT_DEFAULT_MINIMUM: c_int = 0x40;
const HINT_DEFAULT_LOW: c_int = 0x80;
const HINT_DEFAULT_MIDDLE: c_int = 0xC0;
const HINT_DEFAULT_HIGH: c_int = 0x100;
const HINT_DEFAULT_MAXIMUM: c_int = 0x140;
const HINT_DEFAULT_0: c_int = 0x200;
const HINT_DEFAULT_1: c_int = 0x240;
const HINT_DEFAULT_100: c_int = 0x280;
const HINT_DEFAULT_440: c_int = 0x2C0;

/// Upper bound on descriptors and ports accepted from one library. A
/// library that reports more is treated as malformed rather than trusted.
const MAX_DESCRIPTORS: c_ulong = 1024;
const MAX_PORTS: c_ulong = 256;

#[repr(C)]
pub struct PortRangeHint {
    pub hint: c_int,
    pub lower: Data,
    pub upper: Data,
}

#[repr(C)]
pub struct Descriptor {
    pub unique_id: c_ulong,
    pub label: *const c_char,
    pub properties: c_int,
    pub name: *const c_char,
    pub maker: *const c_char,
    pub copyright: *const c_char,
    pub port_count: c_ulong,
    pub port_descriptors: *const c_int,
    pub port_names: *const *const c_char,
    pub port_range_hints: *const PortRangeHint,
    pub implementation_data: *mut c_void,
    pub instantiate: Option<unsafe extern "C" fn(*const Descriptor, c_ulong) -> Handle>,
    pub connect_port: Option<unsafe extern "C" fn(Handle, c_ulong, *mut Data)>,
    pub activate: Option<unsafe extern "C" fn(Handle)>,
    pub run: Option<unsafe extern "C" fn(Handle, c_ulong)>,
    pub run_adding: Option<unsafe extern "C" fn(Handle, c_ulong)>,
    pub set_run_adding_gain: Option<unsafe extern "C" fn(Handle, Data)>,
    pub deactivate: Option<unsafe extern "C" fn(Handle)>,
    pub cleanup: Option<unsafe extern "C" fn(Handle)>,
}

type DescriptorFn = unsafe extern "C" fn(c_ulong) -> *const Descriptor;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

const RTLD_NOW: c_int = 0x2;
#[cfg(target_os = "macos")]
const RTLD_LOCAL: c_int = 0x4;
#[cfg(not(target_os = "macos"))]
const RTLD_LOCAL: c_int = 0x0;

/// Where a library name was looked for. Kept for the render record.
pub struct Search {
    pub ladspa_path: Option<String>,
    pub dirs: Vec<PathBuf>,
}

/// The directories Audacity and other macOS hosts use when `LADSPA_PATH`
/// is unset. The LADSPA SDK itself names no default.
fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join("Library/Audio/Plug-Ins/LADSPA"));
    }
    dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/LADSPA"));
    dirs
}

/// Resolves a library argument: a path containing `/` is used as given;
/// a bare file name is searched in `LADSPA_PATH` (colon separated).
pub fn resolve(name: &str) -> Result<(PathBuf, Search), String> {
    let ladspa_path = std::env::var("LADSPA_PATH").ok();
    let dirs: Vec<PathBuf> = match &ladspa_path {
        Some(value) => value
            .split(':')
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .collect(),
        None => default_dirs(),
    };
    let search = Search { ladspa_path, dirs };
    if name.contains('/') {
        let path = PathBuf::from(name);
        if !path.is_file() {
            return Err(format!("missing plugin library: {name}"));
        }
        return Ok((path, search));
    }
    for dir in &search.dirs {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok((candidate, search));
        }
    }
    let searched: Vec<String> = search
        .dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect();
    Err(format!(
        "missing plugin library: {name} not found in [{}]",
        searched.join(", ")
    ))
}

fn last_dl_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated thread-local string.
    let message = unsafe { dlerror() };
    if message.is_null() {
        return "unknown dynamic loader error".into();
    }
    // SAFETY: checked non-null above.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

pub struct Library {
    handle: *mut c_void,
    entry: DescriptorFn,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self, String> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("library path contains NUL: {}", path.display()))?;
        // SAFETY: c_path is NUL-terminated. Loading runs the library's
        // constructors; that is the trust boundary this probe documents.
        let handle = unsafe { dlopen(c_path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!(
                "cannot load plugin library {}: {}",
                path.display(),
                last_dl_error()
            ));
        }
        // SAFETY: handle is a live dlopen handle; the symbol name is static.
        let symbol = unsafe { dlsym(handle, c"ladspa_descriptor".as_ptr()) };
        if symbol.is_null() {
            // SAFETY: handle came from dlopen and is closed exactly once.
            unsafe { dlclose(handle) };
            return Err(format!(
                "not a LADSPA library (no ladspa_descriptor): {}",
                path.display()
            ));
        }
        // SAFETY: the LADSPA ABI fixes this symbol's signature.
        let entry = unsafe { std::mem::transmute::<*mut c_void, DescriptorFn>(symbol) };
        Ok(Self { handle, entry })
    }

    pub fn descriptors(&self) -> Result<Vec<&Descriptor>, String> {
        let mut found = Vec::new();
        for index in 0..=MAX_DESCRIPTORS {
            // SAFETY: ladspa_descriptor accepts any index and returns null
            // past the end; the result lives as long as the library.
            let raw = unsafe { (self.entry)(index) };
            if raw.is_null() {
                return Ok(found);
            }
            if index == MAX_DESCRIPTORS {
                break;
            }
            // SAFETY: non-null descriptors stay valid until dlclose.
            found.push(unsafe { &*raw });
        }
        Err(format!(
            "library reports more than {MAX_DESCRIPTORS} descriptors"
        ))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: handle is live; every instance is cleaned up before drop.
        unsafe { dlclose(self.handle) };
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Input,
    Output,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Control,
    Audio,
}

pub struct Port {
    pub name: String,
    pub dir: Dir,
    pub kind: Kind,
    /// Descriptor bits outside the four LADSPA defines. CAPS sets 0x10 as a
    /// private "group" flag; a host must mask such bits, not reject them.
    pub extra_bits: c_int,
    pub hint: c_int,
    pub lower: f32,
    pub upper: f32,
}

pub struct Plugin<'a> {
    pub raw: &'a Descriptor,
    pub unique_id: c_ulong,
    pub label: String,
    pub name: String,
    pub maker: String,
    pub copyright: String,
    pub properties: c_int,
    pub ports: Vec<Port>,
}

fn text(ptr: *const c_char, what: &str) -> Result<String, String> {
    if ptr.is_null() {
        return Err(format!("descriptor {what} is null"));
    }
    // SAFETY: non-null strings in a descriptor are NUL-terminated.
    let value = unsafe { CStr::from_ptr(ptr) };
    value
        .to_str()
        .map(str::to_owned)
        .map_err(|_| format!("descriptor {what} is not UTF-8"))
}

fn optional_text(ptr: *const c_char) -> String {
    text(ptr, "").unwrap_or_default()
}

pub fn label_of(raw: &Descriptor) -> String {
    optional_text(raw.label)
}

pub fn validate(raw: &Descriptor) -> Result<Plugin<'_>, String> {
    let label = text(raw.label, "label")?;
    let fail = |message: String| Err(format!("plugin {label}: {message}"));
    if raw.instantiate.is_none()
        || raw.connect_port.is_none()
        || raw.run.is_none()
        || raw.cleanup.is_none()
    {
        return fail("missing a required function (instantiate/connect_port/run/cleanup)".into());
    }
    if raw.port_count > MAX_PORTS {
        return fail(format!("{} ports exceeds {MAX_PORTS}", raw.port_count));
    }
    let count = raw.port_count as usize;
    if count > 0
        && (raw.port_descriptors.is_null()
            || raw.port_names.is_null()
            || raw.port_range_hints.is_null())
    {
        return fail("port arrays are null".into());
    }
    let mut ports = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: the three arrays hold port_count entries (checked non-null).
        let (bits, name_ptr, range) = unsafe {
            (
                *raw.port_descriptors.add(index),
                *raw.port_names.add(index),
                &*raw.port_range_hints.add(index),
            )
        };
        let name = text(name_ptr, &format!("port {index} name"))?;
        let dir = match bits & (PORT_INPUT | PORT_OUTPUT) {
            PORT_INPUT => Dir::Input,
            PORT_OUTPUT => Dir::Output,
            _ => {
                return fail(format!(
                    "port {index} ({name}) is not exactly input or output"
                ));
            }
        };
        let kind = match bits & (PORT_CONTROL | PORT_AUDIO) {
            PORT_CONTROL => Kind::Control,
            PORT_AUDIO => Kind::Audio,
            _ => {
                return fail(format!(
                    "port {index} ({name}) is not exactly control or audio"
                ));
            }
        };
        ports.push(Port {
            name,
            dir,
            kind,
            extra_bits: bits & !(PORT_INPUT | PORT_OUTPUT | PORT_CONTROL | PORT_AUDIO),
            hint: range.hint,
            lower: range.lower,
            upper: range.upper,
        });
    }
    Ok(Plugin {
        raw,
        unique_id: raw.unique_id,
        name: optional_text(raw.name),
        maker: optional_text(raw.maker),
        copyright: optional_text(raw.copyright),
        properties: raw.properties,
        label,
        ports,
    })
}

impl Plugin<'_> {
    pub fn audio(&self, dir: Dir) -> Vec<usize> {
        (0..self.ports.len())
            .filter(|&i| self.ports[i].kind == Kind::Audio && self.ports[i].dir == dir)
            .collect()
    }

    pub fn property_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.properties & PROPERTY_REALTIME != 0 {
            names.push("REALTIME");
        }
        if self.properties & PROPERTY_INPLACE_BROKEN != 0 {
            names.push("INPLACE_BROKEN");
        }
        if self.properties & PROPERTY_HARD_RT_CAPABLE != 0 {
            names.push("HARD_RT_CAPABLE");
        }
        names
    }
}

pub fn hint_names(hint: c_int) -> Vec<&'static str> {
    let mut names = Vec::new();
    for (bit, name) in [
        (HINT_BOUNDED_BELOW, "BOUNDED_BELOW"),
        (HINT_BOUNDED_ABOVE, "BOUNDED_ABOVE"),
        (HINT_TOGGLED, "TOGGLED"),
        (HINT_SAMPLE_RATE, "SAMPLE_RATE"),
        (HINT_LOGARITHMIC, "LOGARITHMIC"),
        (HINT_INTEGER, "INTEGER"),
    ] {
        if hint & bit != 0 {
            names.push(name);
        }
    }
    names.push(match hint & HINT_DEFAULT_MASK {
        HINT_DEFAULT_NONE => "DEFAULT_NONE",
        HINT_DEFAULT_MINIMUM => "DEFAULT_MINIMUM",
        HINT_DEFAULT_LOW => "DEFAULT_LOW",
        HINT_DEFAULT_MIDDLE => "DEFAULT_MIDDLE",
        HINT_DEFAULT_HIGH => "DEFAULT_HIGH",
        HINT_DEFAULT_MAXIMUM => "DEFAULT_MAXIMUM",
        HINT_DEFAULT_0 => "DEFAULT_0",
        HINT_DEFAULT_1 => "DEFAULT_1",
        HINT_DEFAULT_100 => "DEFAULT_100",
        HINT_DEFAULT_440 => "DEFAULT_440",
        _ => "DEFAULT_UNKNOWN",
    });
    names
}

/// A control input's usable range and default at one sample rate.
pub struct Control {
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub default: f64,
    pub toggled: bool,
    pub integer: bool,
}

/// Resolves bounds and the default for a control input per `ladspa.h`:
/// SAMPLE_RATE scales the bounds, LOW/MIDDLE/HIGH interpolate (geometrically
/// when LOGARITHMIC), INTEGER rounds, TOGGLED admits only 0 and 1.
pub fn resolve_control(port: &Port, rate: u32) -> Result<Control, String> {
    let fail = |message: &str| Err(format!("control {}: {message}", port.name));
    let hint = port.hint;
    let scale = if hint & HINT_SAMPLE_RATE != 0 {
        f64::from(rate)
    } else {
        1.0
    };
    let lower = (hint & HINT_BOUNDED_BELOW != 0).then(|| f64::from(port.lower) * scale);
    let upper = (hint & HINT_BOUNDED_ABOVE != 0).then(|| f64::from(port.upper) * scale);
    for bound in [lower, upper].into_iter().flatten() {
        if !bound.is_finite() {
            return fail("non-finite bound");
        }
    }
    if let (Some(lo), Some(hi)) = (lower, upper)
        && lo > hi
    {
        return fail("lower bound above upper bound");
    }
    let toggled = hint & HINT_TOGGLED != 0;
    let integer = hint & HINT_INTEGER != 0;
    let default_bits = hint & HINT_DEFAULT_MASK;
    if toggled {
        let default = match default_bits {
            HINT_DEFAULT_NONE | HINT_DEFAULT_0 => 0.0,
            HINT_DEFAULT_1 => 1.0,
            _ => return fail("toggled port with a default other than 0 or 1"),
        };
        return Ok(Control {
            lower: None,
            upper: None,
            default,
            toggled,
            integer: false,
        });
    }
    let log = hint & HINT_LOGARITHMIC != 0;
    // Weight is the lower bound's share, as in ladspa.h's LOW/MIDDLE/HIGH.
    let mix = |weight: f64| -> Result<f64, String> {
        let (Some(lo), Some(hi)) = (lower, upper) else {
            return Err(format!("control {}: default needs both bounds", port.name));
        };
        if log {
            if lo <= 0.0 || hi <= 0.0 {
                return Err(format!(
                    "control {}: logarithmic default with non-positive bound",
                    port.name
                ));
            }
            Ok((lo.ln() * weight + hi.ln() * (1.0 - weight)).exp())
        } else {
            Ok(lo * weight + hi * (1.0 - weight))
        }
    };
    let mut default = match default_bits {
        HINT_DEFAULT_NONE => {
            let floor = lower.unwrap_or(0.0);
            upper.map_or(floor, |hi| floor.min(hi))
        }
        HINT_DEFAULT_MINIMUM => match lower {
            Some(lo) => lo,
            None => return fail("DEFAULT_MINIMUM without a lower bound"),
        },
        HINT_DEFAULT_MAXIMUM => match upper {
            Some(hi) => hi,
            None => return fail("DEFAULT_MAXIMUM without an upper bound"),
        },
        HINT_DEFAULT_LOW => mix(0.75)?,
        HINT_DEFAULT_MIDDLE => mix(0.5)?,
        HINT_DEFAULT_HIGH => mix(0.25)?,
        HINT_DEFAULT_0 => 0.0,
        HINT_DEFAULT_1 => 1.0,
        HINT_DEFAULT_100 => 100.0,
        HINT_DEFAULT_440 => 440.0,
        _ => return fail("unknown default hint"),
    };
    if integer {
        default = default.round();
    }
    let control = Control {
        lower,
        upper,
        default,
        toggled,
        integer,
    };
    if !control.admits(default) {
        return fail("default lies outside its bounds");
    }
    Ok(control)
}

impl Control {
    pub fn admits(&self, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        if self.toggled {
            return value == 0.0 || value == 1.0;
        }
        if self.integer && value.fract() != 0.0 {
            return false;
        }
        self.lower.is_none_or(|lo| value >= lo) && self.upper.is_none_or(|hi| value <= hi)
    }
}
