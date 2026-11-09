//! DAW-neutral export bundle (issue #109).
//!
//! MIDI and audio writers publish through the same bundle transaction.
//! `adapter` alone knows the project/takes/edit APIs; the writers consume
//! neutral inputs and never consult a plugin or evictable render cache.
//! SoundCraft does not shape these types (D19): this is the neutral bundle
//! target. Media files are named by content hash like a project directory
//! (D3), and retained renders are bundle media, never cache (D4).

pub mod adapter;
pub mod audio;
pub mod bundle;
pub mod fixture;
pub mod manifest;
pub mod midi;
