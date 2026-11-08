use super::{State, pedals};
use brownnote_native::{
    assets::{AssetRegistry, ChokeIdentity, RegistryError, Replacement},
    engine::Slots,
    import::{
        ImportFailure, ImportJob, ImportService, Imported, MediaPublisher, SessionMedia, StartError,
    },
    kit::{Kit, PADS, WAVEFORM_BINS, Waveform},
    project::{PadId, SampleAssignment},
};
use std::{path::PathBuf, sync::Arc};

pub(super) fn pad_id(index: usize) -> PadId {
    // All callers use indices into the fixed sixteen-pad bank.
    assert!(index < 16);
    match PadId::try_from(index as u8) {
        Ok(pad) => pad,
        Err(_) => unreachable!("fixed pad index"),
    }
}

struct Pending {
    ticket: Replacement,
    job: ImportJob,
    ready: Option<Imported>,
}

pub struct Media {
    pub owned: Arc<SessionMedia>,
    service: Result<ImportService, String>,
    pending: [Option<Pending>; 16],
}

impl Media {
    pub fn with_service(owned: Arc<SessionMedia>, service: Result<ImportService, String>) -> Self {
        Self {
            owned,
            service,
            pending: std::array::from_fn(|_| None),
        }
    }

    pub fn busy(&self) -> bool {
        self.pending.iter().any(Option::is_some)
    }
}

pub(super) fn bootstrap(kit: &Kit) -> (AssetRegistry, Media) {
    let owned = Arc::new(SessionMedia::default());
    let service = ImportService::new(owned.clone()).map_err(|error| error.to_string());
    let mut slots = Slots::empty();
    let mut registered = Vec::<Imported>::new();
    let mut errors = kit.errors.clone();
    for (index, media) in kit.media.iter().enumerate() {
        let Some(media) = media else {
            continue;
        };
        let imported = &media.imported;
        if let Err(error) = owned.publish(&imported.asset.reference, &media.bytes) {
            errors[index] = Some(error.to_string());
        } else if !registered
            .iter()
            .any(|asset| asset.asset.reference.id == imported.asset.reference.id)
        {
            slots.set(registered.len(), Some(imported.sample.clone()));
            registered.push(imported.clone());
        }
    }
    let mut assets = AssetRegistry::new(slots);
    for (slot, imported) in registered.into_iter().enumerate() {
        let result = assets.register_existing(slot, imported);
        debug_assert!(result.is_ok(), "captured kit snapshot must match its slot");
    }
    for (index, spec) in PADS.iter().enumerate() {
        let ticket = assets.begin(pad_id(index));
        if let Some(media) = &kit.media[spec.sample]
            && errors[spec.sample].is_none()
        {
            let id = media.imported.asset.reference.id;
            let result = assets.bind_existing(
                pad_id(index),
                SampleAssignment {
                    source: id,
                    retained: None,
                },
                ChokeIdentity::Group(spec.sample as u64),
            );
            debug_assert!(result.is_ok(), "registered kit asset must bind");
        } else {
            assets.report_error(
                ticket,
                errors[spec.sample]
                    .clone()
                    .unwrap_or_else(|| "Missing encoded sample".into()),
            );
        }
    }
    (assets, Media::with_service(owned, service))
}

impl State {
    /// Service entry point only. No picker, document write or file I/O here.
    #[allow(dead_code)] // Public service entry for the next, separately scoped import UI.
    pub fn import_sample(&mut self, pad: usize, path: Option<PathBuf>) -> Result<(), StartError> {
        let Some(path) = path else {
            self.cancel_import(pad);
            return Ok(());
        };
        let service = self
            .media
            .service
            .as_ref()
            .map_err(|_| StartError::WorkerStopped)?;
        let job = service.import_sample(Some(path))?;
        self.cancel_import(pad);
        pedals::cancel_work(self, pad);
        let ticket = self.assets.begin(pad_id(pad));
        self.media.pending[pad] = Some(Pending {
            ticket,
            job,
            ready: None,
        });
        if let Some(wake) = &self.wake {
            wake();
        }
        Ok(())
    }

    pub fn cancel_import(&mut self, pad: usize) {
        if let Some(pending) = self.media.pending[pad].take() {
            pending.job.cancel();
            self.assets.cancel(pending.ticket);
        }
    }

    pub fn source_wave(&self, pad: usize) -> &Waveform {
        self.assets
            .source(pad_id(pad))
            .map(|asset| &asset.imported.waveform)
            .unwrap_or(&SILENT_WAVE)
    }

    pub fn pad_wave(&self, pad: usize) -> &Waveform {
        self.assets
            .audible(pad_id(pad))
            .map(|asset| &asset.imported.waveform)
            .unwrap_or(&SILENT_WAVE)
    }

    pub fn pad_name(&self, pad: usize) -> &str {
        self.assets
            .source(pad_id(pad))
            .map(|asset| asset.imported.label.as_str())
            .unwrap_or("Missing sample")
    }

    pub fn pad_error(&self, pad: usize) -> Option<&str> {
        self.assets.pad(pad_id(pad)).error.as_deref()
    }

    pub fn poll_assets(&mut self) -> bool {
        let mut changed = false;
        for returned in self.output.take_retired() {
            // Output's only allocator is this registry. Rejected Load/Trigger
            // returns are still dropped here, never on the callback.
            changed |= self.assets.returned(returned).is_ok();
        }
        self.assets
            .flush_unloads(|command| self.output.send(command));
        changed
    }
}

const SILENT_WAVE: Waveform = [[0.0; 2]; WAVEFORM_BINS];

pub(super) fn poll(s: &mut State) -> bool {
    let mut changed = s.poll_assets();
    for pad in 0..16 {
        let Some(mut pending) = s.media.pending[pad].take() else {
            continue;
        };
        if !s.assets.current(pending.ticket) {
            continue;
        }
        if pending.ready.is_none() {
            match pending.job.take() {
                Some(Ok(imported)) => pending.ready = Some(imported),
                Some(Err(error)) => {
                    if error.failure != ImportFailure::Cancelled {
                        s.assets.report_error(pending.ticket, error.to_string());
                    }
                    changed = true;
                    continue;
                }
                None => {}
            }
        }
        if let Some(imported) = &pending.ready {
            match s.assets.replace_source(
                pending.ticket,
                imported,
                ChokeIdentity::Asset(imported.asset.reference.id),
                |command| s.output.send(command),
            ) {
                Ok(()) => {
                    pedals::source_changed(s, pad);
                    changed = true;
                    continue;
                }
                Err(RegistryError::QueueFull | RegistryError::Full) => {}
                Err(_) => continue,
            }
        }
        s.media.pending[pad] = Some(pending);
    }
    changed
}
