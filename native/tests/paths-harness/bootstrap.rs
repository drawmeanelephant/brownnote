//! Temporary test-only registration until the integrator exports `paths`.
//! These are the production modules, not alternate implementations.
#[path = "../../src/engine/mod.rs"]
pub mod engine;
#[path = "../../src/kit.rs"]
pub mod kit;
#[path = "../../src/paths.rs"]
pub mod paths;
#[path = "../../src/pedals/mod.rs"]
pub mod pedals;
