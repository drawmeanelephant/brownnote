//! LADSPA 1.1 FFI for the offline helper: library loading, descriptor
//! validation, port-hint resolution and one plugin instance with its own
//! port buffers. The structs follow `ladspa.h` from the LADSPA SDK. This is
//! the only LADSPA file that uses `unsafe`; the rules are taken from the #97
//! probe (`native/docs/spikes/ladspa`).
use libloading::os::unix::{Library as Dl, RTLD_LOCAL, RTLD_NOW};
use std::{
    ffi::{CStr, c_char, c_int, c_ulong, c_void},
    fmt,
    path::Path,
    ptr,
    sync::Arc,
};

pub const PROPERTY_REALTIME: c_int = 0x1;

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

/// A library that reports more descriptors or ports than this is treated
/// as malformed rather than walked.
const MAX_DESCRIPTORS: c_ulong = 1024;
const MAX_PORTS: c_ulong = 256;

#[repr(C)]
struct PortRangeHint {
    hint: c_int,
    lower: f32,
    upper: f32,
}

type Handle = *mut c_void;
type RunFn = unsafe extern "C" fn(Handle, c_ulong);
type HandleFn = unsafe extern "C" fn(Handle);

#[repr(C)]
struct Descriptor {
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
    activate: Option<HandleFn>,
    run: Option<RunFn>,
    run_adding: Option<RunFn>,
    set_run_adding_gain: Option<unsafe extern "C" fn(Handle, f32)>,
    deactivate: Option<HandleFn>,
    cleanup: Option<HandleFn>,
}

type DescriptorFn = unsafe extern "C" fn(c_ulong) -> *const Descriptor;

#[derive(Debug)]
pub enum LadspaError {
    Missing(String),
    Dlopen(String),
    NoEntry(String),
    UnknownPlugin(String),
    InvalidDescriptor(String),
    InitFailed(String),
    ParamRejected(String),
    Unsupported(String),
}

impl LadspaError {
    /// The protocol error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing(_) => "bundle-missing",
            Self::Dlopen(_) => "dlopen-failed",
            Self::NoEntry(_) => "no-entry",
            Self::UnknownPlugin(_) => "unknown-plugin-id",
            Self::InvalidDescriptor(_) => "invalid-descriptor",
            Self::InitFailed(_) => "init-failed",
            Self::ParamRejected(_) => "param-rejected",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

impl fmt::Display for LadspaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Missing(message)
        | Self::Dlopen(message)
        | Self::NoEntry(message)
        | Self::UnknownPlugin(message)
        | Self::InvalidDescriptor(message)
        | Self::InitFailed(message)
        | Self::ParamRejected(message)
        | Self::Unsupported(message)) = self;
        write!(f, "{}: {message}", self.code())
    }
}

fn text(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null descriptor strings are NUL-terminated and live as
    // long as the library.
    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

pub struct Library {
    entry: DescriptorFn,
    // Kept loaded for as long as any descriptor or instance uses it.
    _dl: Dl,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self, LadspaError> {
        // SAFETY: loading runs the library's initialisers. Only allow-listed
        // plugins get this far, and a crash ends this helper process, not the
        // app.
        let dl = unsafe { Dl::open(Some(path), RTLD_NOW | RTLD_LOCAL) }
            .map_err(|error| LadspaError::Dlopen(error.to_string()))?;
        // SAFETY: the LADSPA ABI fixes the signature of `ladspa_descriptor`.
        let entry = unsafe { dl.get::<DescriptorFn>(b"ladspa_descriptor\0") }
            .map(|symbol| *symbol)
            .map_err(|error| {
                LadspaError::NoEntry(format!(
                    "{} has no ladspa_descriptor: {error}",
                    path.display()
                ))
            })?;
        Ok(Self { entry, _dl: dl })
    }

    /// The validated descriptor with this `UniqueID` and `Label`. Other
    /// descriptors in the library are not validated or instantiated.
    pub fn find(self: &Arc<Self>, unique_id: u64, label: &str) -> Result<Plugin, LadspaError> {
        for index in 0..=MAX_DESCRIPTORS {
            // SAFETY: `ladspa_descriptor` accepts any index and returns null
            // past the last descriptor.
            let raw = unsafe { (self.entry)(index) };
            if raw.is_null() {
                break;
            }
            if index == MAX_DESCRIPTORS {
                return Err(LadspaError::InvalidDescriptor(format!(
                    "the library reports more than {MAX_DESCRIPTORS} descriptors"
                )));
            }
            // SAFETY: a non-null descriptor stays valid while the library is
            // loaded, and `self` keeps it loaded.
            let descriptor = unsafe { &*raw };
            if u64::from(descriptor.unique_id) == unique_id
                && text(descriptor.label).as_deref() == Some(label)
            {
                return Plugin::validate(Arc::clone(self), raw);
            }
        }
        Err(LadspaError::UnknownPlugin(format!(
            "the library has no plugin {unique_id}:{label}"
        )))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Control,
    Audio,
}

#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    pub direction: Direction,
    pub kind: Kind,
    hint: c_int,
    lower: f32,
    upper: f32,
}

/// A validated descriptor. Holds its library open.
pub struct Plugin {
    raw: *const Descriptor,
    pub name: String,
    pub properties: c_int,
    pub ports: Vec<Port>,
    library: Arc<Library>,
}

impl Plugin {
    fn validate(library: Arc<Library>, raw: *const Descriptor) -> Result<Self, LadspaError> {
        // SAFETY: `raw` is non-null and `library` keeps it valid.
        let descriptor = unsafe { &*raw };
        let label = text(descriptor.label).unwrap_or_default();
        let invalid =
            |message: String| LadspaError::InvalidDescriptor(format!("{label}: {message}"));
        if descriptor.instantiate.is_none()
            || descriptor.connect_port.is_none()
            || descriptor.run.is_none()
            || descriptor.cleanup.is_none()
        {
            return Err(invalid(
                "missing a required function (instantiate, connect_port, run or cleanup)".into(),
            ));
        }
        if descriptor.port_count > MAX_PORTS {
            return Err(invalid(format!(
                "{} ports is more than {MAX_PORTS}",
                descriptor.port_count
            )));
        }
        let count = descriptor.port_count as usize;
        if count > 0
            && (descriptor.port_descriptors.is_null()
                || descriptor.port_names.is_null()
                || descriptor.port_range_hints.is_null())
        {
            return Err(invalid("its port arrays are null".into()));
        }
        let mut ports = Vec::with_capacity(count);
        for index in 0..count {
            // SAFETY: the three arrays hold `port_count` entries and were
            // checked non-null above.
            let (bits, name, range) = unsafe {
                (
                    *descriptor.port_descriptors.add(index),
                    *descriptor.port_names.add(index),
                    &*descriptor.port_range_hints.add(index),
                )
            };
            let name = text(name).unwrap_or_else(|| format!("port {index}"));
            // Bits outside the four port defines are masked, not rejected:
            // CAPS sets a private 0x10 bit on some ports.
            let direction = match bits & (PORT_INPUT | PORT_OUTPUT) {
                PORT_INPUT => Direction::Input,
                PORT_OUTPUT => Direction::Output,
                _ => {
                    return Err(invalid(format!(
                        "port {index} ({name}) is not exactly an input or an output"
                    )));
                }
            };
            let kind = match bits & (PORT_CONTROL | PORT_AUDIO) {
                PORT_CONTROL => Kind::Control,
                PORT_AUDIO => Kind::Audio,
                _ => {
                    return Err(invalid(format!(
                        "port {index} ({name}) is not exactly control or audio"
                    )));
                }
            };
            ports.push(Port {
                name,
                direction,
                kind,
                hint: range.hint,
                lower: range.lower,
                upper: range.upper,
            });
        }
        let name = text(descriptor.name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(label);
        Ok(Self {
            raw,
            name,
            properties: descriptor.properties,
            ports,
            library,
        })
    }

    fn descriptor(&self) -> &Descriptor {
        // SAFETY: `self.library` keeps the descriptor valid.
        unsafe { &*self.raw }
    }

    /// Indices of the audio ports in one direction, in port order.
    pub fn audio(&self, direction: Direction) -> Vec<usize> {
        self.ports
            .iter()
            .enumerate()
            .filter(|(_, port)| port.kind == Kind::Audio && port.direction == direction)
            .map(|(index, _)| index)
            .collect()
    }

    /// Instantiate at `rate`, connect every port to buffers the instance
    /// owns, set `controls` (port index and value; other control ports start
    /// at 0), and activate.
    pub fn instantiate(
        &self,
        rate: u32,
        max_frames: usize,
        controls: &[(usize, f32)],
    ) -> Result<Instance, LadspaError> {
        let descriptor = self.descriptor();
        let (Some(instantiate), Some(connect), Some(run), Some(cleanup)) = (
            descriptor.instantiate,
            descriptor.connect_port,
            descriptor.run,
            descriptor.cleanup,
        ) else {
            return Err(LadspaError::InvalidDescriptor(
                "a required function is missing".into(),
            ));
        };
        let mut control_values = Buffer::new(self.ports.len());
        for &(port, value) in controls {
            if let Some(slot) = control_values.as_mut().get_mut(port) {
                *slot = value;
            }
        }
        let inputs: Vec<Buffer> = self
            .audio(Direction::Input)
            .iter()
            .map(|_| Buffer::new(max_frames))
            .collect();
        let outputs: Vec<Buffer> = self
            .audio(Direction::Output)
            .iter()
            .map(|_| Buffer::new(max_frames))
            .collect();
        // SAFETY: the descriptor is validated and its library is loaded.
        let handle = unsafe { instantiate(self.raw, c_ulong::from(rate)) };
        if handle.is_null() {
            return Err(LadspaError::InitFailed(format!(
                "{} did not instantiate at {rate} Hz",
                self.name
            )));
        }
        let mut instance = Instance {
            handle,
            run,
            deactivate: descriptor.deactivate,
            cleanup,
            active: false,
            max_frames,
            inputs,
            outputs,
            controls: control_values,
            _library: Arc::clone(&self.library),
        };
        let (mut input, mut output) = (0, 0);
        for (index, port) in self.ports.iter().enumerate() {
            // Input and output buffers are always separate allocations, so
            // plugins marked INPLACE_BROKEN are always honoured.
            let data = match (port.kind, port.direction) {
                (Kind::Audio, Direction::Input) => {
                    input += 1;
                    instance.inputs[input - 1].ptr
                }
                (Kind::Audio, Direction::Output) => {
                    output += 1;
                    instance.outputs[output - 1].ptr
                }
                // SAFETY: `index` is below the port count, the control
                // buffer's length.
                (Kind::Control, _) => unsafe { instance.controls.ptr.add(index) },
            };
            // SAFETY: every buffer lives, unmoved, until the instance is
            // cleaned up. Every port is connected, control outputs included.
            unsafe { connect(handle, index as c_ulong, data) };
        }
        if let Some(activate) = descriptor.activate {
            // SAFETY: the instance is live and every port is connected.
            unsafe { activate(handle) };
        }
        instance.active = true;
        Ok(instance)
    }
}

/// Samples whose address a plugin keeps. Only ever accessed through `ptr`,
/// so no Rust borrow can invalidate the plugin's pointer.
struct Buffer {
    ptr: *mut f32,
    len: usize,
}

impl Buffer {
    fn new(len: usize) -> Self {
        let samples: Box<[f32]> = vec![0.0; len.max(1)].into_boxed_slice();
        let len = samples.len();
        Self {
            ptr: Box::into_raw(samples).cast(),
            len,
        }
    }

    fn as_mut(&mut self) -> &mut [f32] {
        // SAFETY: `ptr` owns `len` initialised samples; no plugin call runs
        // while this borrow lives.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` came from `Box::into_raw` in `new`.
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(self.ptr, self.len)) });
    }
}

/// One live, activated plugin instance. Dropping it deactivates (when the
/// plugin has `deactivate`) and cleans up, then frees the port buffers,
/// then releases the library.
pub struct Instance {
    handle: Handle,
    run: RunFn,
    deactivate: Option<HandleFn>,
    cleanup: HandleFn,
    active: bool,
    max_frames: usize,
    inputs: Vec<Buffer>,
    outputs: Vec<Buffer>,
    controls: Buffer,
    _library: Arc<Library>,
}

impl Instance {
    pub fn input(&mut self, index: usize) -> &mut [f32] {
        let max = self.max_frames;
        &mut self.inputs[index].as_mut()[..max]
    }

    pub fn output(&mut self, index: usize) -> &[f32] {
        let max = self.max_frames;
        &self.outputs[index].as_mut()[..max]
    }

    /// Process `frames` samples of every connected port with `run`.
    /// `run_adding` is never used, so output never depends on what an
    /// output buffer held before.
    pub fn run(&mut self, frames: usize) -> Result<(), LadspaError> {
        if frames == 0 || frames > self.max_frames {
            return Err(LadspaError::Unsupported(format!(
                "a block of {frames} frames (at most {})",
                self.max_frames
            )));
        }
        // SAFETY: the instance is active and every port is connected to a
        // buffer of at least `max_frames` samples.
        unsafe { (self.run)(self.handle, frames as c_ulong) };
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.active
            && let Some(deactivate) = self.deactivate
        {
            // SAFETY: the instance is live and was activated.
            unsafe { deactivate(self.handle) };
        }
        // SAFETY: `handle` came from `instantiate` and is cleaned up once,
        // before its buffers and library are released.
        unsafe { (self.cleanup)(self.handle) };
    }
}

/// A control input's usable range and default at one sample rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub default: f64,
    pub toggled: bool,
    pub integer: bool,
}

impl Port {
    /// Bounds and default per `ladspa.h`: `SAMPLE_RATE` scales the bounds,
    /// LOW/MIDDLE/HIGH interpolate (geometrically when `LOGARITHMIC`),
    /// `INTEGER` rounds and `TOGGLED` admits only 0 and 1. A missing default
    /// is the lower bound (or 0), but never above the upper bound.
    pub fn control(&self, rate: u32) -> Result<Control, LadspaError> {
        let fail = |message: &str| {
            Err(LadspaError::InvalidDescriptor(format!(
                "control {}: {message}",
                self.name
            )))
        };
        let hint = self.hint;
        let scale = if hint & HINT_SAMPLE_RATE != 0 {
            f64::from(rate)
        } else {
            1.0
        };
        let lower = (hint & HINT_BOUNDED_BELOW != 0).then(|| f64::from(self.lower) * scale);
        let upper = (hint & HINT_BOUNDED_ABOVE != 0).then(|| f64::from(self.upper) * scale);
        if [lower, upper].into_iter().flatten().any(|b| !b.is_finite()) {
            return fail("a bound is not finite");
        }
        if let (Some(lo), Some(hi)) = (lower, upper)
            && lo > hi
        {
            return fail("its lower bound is above its upper bound");
        }
        let default_bits = hint & HINT_DEFAULT_MASK;
        if hint & HINT_TOGGLED != 0 {
            let default = match default_bits {
                HINT_DEFAULT_NONE | HINT_DEFAULT_0 => 0.0,
                HINT_DEFAULT_1 => 1.0,
                _ => return fail("a toggle's default must be 0 or 1"),
            };
            return Ok(Control {
                lower: Some(0.0),
                upper: Some(1.0),
                default,
                toggled: true,
                integer: false,
            });
        }
        let log = hint & HINT_LOGARITHMIC != 0;
        // `weight` is the lower bound's share, as in ladspa.h.
        let mix = |weight: f64| -> Result<f64, &'static str> {
            let (Some(lo), Some(hi)) = (lower, upper) else {
                return Err("an interpolated default needs both bounds");
            };
            if log {
                if lo <= 0.0 || hi <= 0.0 {
                    return Err("a logarithmic default needs positive bounds");
                }
                Ok((lo.ln() * weight + hi.ln() * (1.0 - weight)).exp())
            } else {
                Ok(lo * weight + hi * (1.0 - weight))
            }
        };
        let mixed = |weight| {
            mix(weight).map_err(|message| {
                LadspaError::InvalidDescriptor(format!("control {}: {message}", self.name))
            })
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
            HINT_DEFAULT_LOW => mixed(0.75)?,
            HINT_DEFAULT_MIDDLE => mixed(0.5)?,
            HINT_DEFAULT_HIGH => mixed(0.25)?,
            HINT_DEFAULT_0 => 0.0,
            HINT_DEFAULT_1 => 1.0,
            HINT_DEFAULT_100 => 100.0,
            HINT_DEFAULT_440 => 440.0,
            _ => return fail("unknown default hint"),
        };
        let integer = hint & HINT_INTEGER != 0;
        if integer {
            default = default.round();
        }
        let control = Control {
            lower,
            upper,
            default,
            toggled: false,
            integer,
        };
        if !control.admits(default) {
            return fail("its default lies outside its bounds");
        }
        Ok(control)
    }
}

impl Control {
    /// Whether the plugin can be given `value`. Values outside the hints are
    /// rejected, never clamped.
    pub fn admits(&self, value: f64) -> bool {
        if !value.is_finite() || !(value as f32).is_finite() {
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

    /// The range reported to the app. A side without a bound reports the
    /// largest finite `f32`, which is what the port can carry.
    pub fn range(&self) -> (f64, f64) {
        let max = f64::from(f32::MAX);
        (self.lower.unwrap_or(-max), self.upper.unwrap_or(max))
    }
}
