//! DAW-neutral export bundle (issue #109).
//!
//! Wave 3's first PR delivers the versioned manifest (`manifest`) and the
//! bundle transaction (`bundle`) against synthetic DTOs (`fixture`); the MIDI
//! and audio writers are follow-up PRs and plug into the same transaction.
//! SoundCraft does not shape these types (D19): this is the neutral bundle
//! target. Media files are named by content hash like a project directory
//! (D3), and retained renders are bundle media, never cache (D4).

pub mod bundle;
pub mod fixture;
pub mod manifest;
