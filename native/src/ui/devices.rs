//! Output selection and recovery, owned by the feature, not the project.
pub(crate) mod preferences;
#[cfg(test)]
mod tests;
mod view;

use super::State;
use brownnote_native::{
    engine::Slots,
    output::{DeviceInfo, Output, OutputSelection, output_devices},
};
use cpal::{
    BufferSize, DeviceId,
    traits::{DeviceTrait, HostTrait},
};
use rune::TextEdit;
use std::{path::PathBuf, rc::Rc};
pub use view::{header, panel};

pub struct Discovery {
    pub devices: Vec<DeviceInfo>,
    pub default_id: Option<DeviceId>,
}

pub type DiscoverFn = Rc<dyn Fn() -> Result<Discovery, String>>;
pub type OpenFn =
    Rc<dyn Fn(Slots, &OutputSelection, &mut super::live::Live) -> Result<Output, String>>;

enum Request {
    Queued(OutputSelection),
    Opening(OutputSelection),
}

pub struct Devices {
    pub visible: bool,
    pub discovery: Option<Discovery>,
    pub draft: OutputSelection,
    pub rate: TextEdit,
    pub channels: TextEdit,
    pub buffer: TextEdit,
    pub error: Option<String>,
    pub preference_error: Option<String>,
    pub notice: Option<String>,
    pub discover: DiscoverFn,
    pub open: OpenFn,
    root: Option<PathBuf>,
    enabled: bool,
    refresh: bool,
    request: Option<Request>,
}

impl Devices {
    /// Safe for every headless/test constructor: no discovery, files or audio.
    pub fn disabled() -> Self {
        Self {
            visible: false,
            discovery: None,
            draft: OutputSelection::default(),
            rate: TextEdit::default(),
            channels: TextEdit::default(),
            buffer: TextEdit::default(),
            error: None,
            preference_error: None,
            notice: None,
            discover: Rc::new(|| Err("Audio is disabled for this session".into())),
            open: Rc::new(|_, _, _| Err("Audio is disabled for this session".into())),
            root: None,
            enabled: false,
            refresh: false,
            request: None,
        }
    }

    /// Only interactive startup calls this, with an explicit machine root.
    /// Bad preferences stay silent until the user chooses/retries explicitly.
    pub fn load(root: Result<PathBuf, String>) -> Self {
        let mut state = Self::disabled();
        state.enabled = true;
        state.refresh = true;
        state.discover = Rc::new(|| {
            let host = cpal::default_host();
            let default_id = host
                .default_output_device()
                .and_then(|device| device.id().ok());
            output_devices()
                .map(|devices| Discovery {
                    devices,
                    default_id,
                })
                .map_err(|error| error.to_string())
        });
        state.open = Rc::new(|slots, selection, live| {
            Output::open_selected_with_live(slots, selection, |rate, buffer| {
                let voice = live.mount_current(rate, buffer);
                let note = live.status_note();
                (voice, note)
            })
            .map_err(|error| error.to_string())
        });
        match root {
            Ok(root) => {
                match preferences::load(&root) {
                    Ok(selection) => {
                        state.set_draft(selection.clone());
                        state.request = Some(Request::Queued(selection));
                    }
                    Err(error) => state.preference_error = Some(error),
                }
                state.root = Some(root);
            }
            Err(error) => state.preference_error = Some(error),
        }
        state
    }

    pub fn initial_output(&self, slots: Slots, disabled_reason: String) -> Output {
        if self.enabled {
            Output::silent_selected(slots, self.draft.clone(), "Output not opened yet".into())
        } else {
            Output::virtual_driver(slots, disabled_reason, false)
        }
    }

    pub fn busy(&self) -> bool {
        self.request.is_some()
    }

    /// Whether this session owns a real audio device seam (interactive only).
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_draft(&mut self, selection: OutputSelection) {
        self.rate = TextEdit::new(
            selection
                .sample_rate
                .map(|n| n.to_string())
                .unwrap_or_default(),
        );
        self.channels = TextEdit::new(
            selection
                .channels
                .map(|n| n.to_string())
                .unwrap_or_default(),
        );
        self.buffer = TextEdit::new(match selection.buffer_size {
            BufferSize::Default => String::new(),
            BufferSize::Fixed(n) => n.to_string(),
        });
        self.draft = selection;
        self.error = None;
    }

    pub fn select_device(&mut self, id: Option<DeviceId>) {
        self.set_draft(OutputSelection {
            device_id: id,
            ..Default::default()
        });
    }

    fn parsed(&self) -> Result<OutputSelection, String> {
        fn number<T: std::str::FromStr>(text: &str, name: &str) -> Result<Option<T>, String> {
            let text = text.trim();
            if text.is_empty() {
                Ok(None)
            } else {
                text.parse().map(Some).map_err(|_| {
                    format!(
                        "Invalid {name}: use a positive whole number or leave blank for default"
                    )
                })
            }
        }
        Ok(OutputSelection {
            device_id: self.draft.device_id.clone(),
            sample_rate: number(&self.rate.text, "sample rate")?,
            channels: number(&self.channels.text, "channel count")?,
            buffer_size: number(&self.buffer.text, "buffer request")?
                .map_or(BufferSize::Default, BufferSize::Fixed),
        })
    }

    fn validate(&self, selection: &OutputSelection) -> Result<(), String> {
        let discovery = self
            .discovery
            .as_ref()
            .ok_or("Refresh outputs before applying a configuration")?;
        selection
            .resolve(&discovery.devices, discovery.default_id.as_ref())
            .and_then(|device| device.configuration(selection))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn remember(&mut self, selection: &OutputSelection) {
        if let Some(root) = &self.root {
            self.preference_error = preferences::save(root, selection).err();
        }
    }
}

/// Uses the draft only after validating the complete tuple at the device seam.
/// Invalid edits neither stop a stream nor overwrite its remembered selection.
pub fn apply(s: &mut State) {
    if s.devices.busy() {
        return;
    }
    match s.devices.parsed().and_then(|selection| {
        s.devices.validate(&selection)?;
        Ok(selection)
    }) {
        Ok(selection) => {
            s.devices.error = None;
            s.devices.remember(&selection);
            s.devices.request = Some(Request::Queued(selection));
        }
        Err(error) => s.devices.error = Some(error),
    }
}

pub fn retry(s: &mut State) {
    if !s.devices.busy() {
        s.devices.error = None;
        s.devices.request = Some(Request::Queued(s.output.selection().clone()));
    }
}

/// Reopen the remembered output after a live-instrument change (D16). The
/// voice and helper are rebuilt at the new stream's real rate and buffer.
pub fn reopen(s: &mut State) {
    if s.devices.enabled() && !s.devices.busy() {
        s.devices.error = None;
        s.devices.request = Some(Request::Queued(s.output.selection().clone()));
    }
}

/// This is a deliberate identity/config reset, never an automatic fallback.
pub fn system_default(s: &mut State) {
    if !s.devices.busy() {
        let selection = OutputSelection::default();
        s.devices.set_draft(selection.clone());
        s.devices.remember(&selection);
        s.devices.request = Some(Request::Queued(selection));
    }
}

pub fn refresh(s: &mut State) {
    s.devices.refresh = true;
}

/// Discovery is UI-side and explicitly refreshed, not callback work. Opening is
/// split across polls so Retrying is observable and the old stream is dropped
/// before opening a replacement. Stream objects stay on their owning thread.
pub fn poll(s: &mut State) -> bool {
    let mut changed = false;
    if std::mem::take(&mut s.devices.refresh) {
        match (s.devices.discover)() {
            Ok(discovery) => {
                s.devices.discovery = Some(discovery);
                s.devices.error = None;
            }
            Err(error) => {
                s.devices.discovery = None;
                s.devices.error = Some(format!("Cannot discover outputs: {error}"));
            }
        }
        changed = true;
    }
    match s.devices.request.take() {
        Some(Request::Queued(selection)) => {
            s.stop_for_output_change(selection.clone(), "Retrying selected output".into());
            s.devices.notice = Some(
                "Playing voices stopped; a fresh mixer is used for every output or rate change."
                    .into(),
            );
            s.devices.request = Some(Request::Opening(selection));
            if let Some(wake) = &s.wake {
                wake();
            }
            true
        }
        Some(Request::Opening(selection)) => {
            let open = s.devices.open.clone();
            s.open_output_selection(&selection, open.as_ref());
            true
        }
        None => changed,
    }
}

/// Never infer Running from a factory's success or a cached status string.
pub fn status(s: &State) -> String {
    if matches!(s.devices.request, Some(Request::Queued(_))) {
        "RETRYING · change queued; current voices stop when reopening starts".into()
    } else if s.devices.busy() {
        "RETRYING · silent while opening the remembered output".into()
    } else if let Some(opened) = s.output.opened() {
        format!(
            "RUNNING · {} · {} Hz · {} ch · {:?}",
            opened.name,
            opened.configuration.stream.sample_rate,
            opened.configuration.stream.channels,
            opened.configuration.format
        )
    } else {
        format!("SILENT · {}", s.output.status)
    }
}

pub enum SceneKind {
    Selection,
    Missing,
    Invalid,
}

/// Deterministic fixture scenes, with no real preferences or audio access.
pub fn scene(s: &mut State, kind: SceneKind) {
    s.devices = Devices::disabled();
    s.devices.visible = true;
    let id = DeviceId::new(cpal::default_host().id(), "brownnote-scene-output");
    let range = cpal::SupportedStreamConfigRange::new(
        2,
        44_100,
        48_000,
        cpal::SupportedBufferSize::Range { min: 64, max: 1024 },
        cpal::SampleFormat::F32,
    );
    s.devices.discovery = Some(Discovery {
        default_id: Some(id.clone()),
        devices: vec![DeviceInfo {
            id: id.clone(),
            name: "Scene output".into(),
            default_config: range.with_sample_rate(44_100),
            supported_configs: vec![range],
        }],
    });
    s.devices.discover = Rc::new(|| Err("Scene discovery is disabled".into()));
    s.devices.open = Rc::new(|_, _, _| Err("Scene output is silent".into()));
    match kind {
        SceneKind::Selection => s.devices.select_device(Some(id)),
        SceneKind::Missing => {
            s.devices.select_device(Some(DeviceId::new(
                cpal::default_host().id(),
                "missing-scene-output",
            )));
            s.stop_for_output_change(
                s.devices.draft.clone(),
                "Selected output is unavailable; retry or choose system default".into(),
            );
        }
        SceneKind::Invalid => {
            s.devices.select_device(Some(id));
            s.devices.rate = TextEdit::new("96000");
            apply(s);
        }
    }
}
