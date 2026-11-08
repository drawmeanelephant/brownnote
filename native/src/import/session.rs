use super::MediaPublisher;
use crate::project::{AssetId, AssetRef};
use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex},
};

/// Unsaved document's owned encoded media. Never evicted with render cache.
/// This is not durable storage: adoption must precede a real Store save.
#[derive(Default)]
pub struct SessionMedia {
    bytes: Mutex<BTreeMap<AssetId, Arc<[u8]>>>,
}

impl SessionMedia {
    /// Run on the storage worker. Publish exact owned bytes to the store's
    /// coordinated publisher, then Store::save(document, &[]) verifies them.
    pub fn adopt(&self, publisher: &dyn MediaPublisher) -> io::Result<()> {
        let snapshot = self
            .bytes
            .lock()
            .map_err(|_| io::Error::other("session media poisoned"))?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for bytes in snapshot {
            publisher.publish(&AssetRef::of(&bytes), &bytes)?;
        }
        Ok(())
    }
}

impl MediaPublisher for SessionMedia {
    fn publish(&self, reference: &AssetRef, bytes: &[u8]) -> io::Result<()> {
        if AssetRef::of(bytes) != *reference {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "media identity mismatch",
            ));
        }
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("session media poisoned"))?
            .entry(reference.id)
            .or_insert_with(|| Arc::from(bytes));
        Ok(())
    }
}
