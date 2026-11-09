//! Plugin hosting. The live CLAP instrument backend (#112) lives here; the
//! offline plugin renderer (#113) lives in `pedals::offline`.
pub mod helper;
pub mod live;
pub mod live_protocol;
