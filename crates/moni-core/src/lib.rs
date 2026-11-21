//! Core engine library: world simulation, plugin host contracts, profile
//! resolution, scene formats, and the wire protocol. Core never names a
//! concrete plugin; the binary is the only composition point that does.

pub mod components;
pub mod engine;
pub mod host;
pub mod plugins;
pub mod profile;
pub mod project;
pub mod protocol;
pub mod render;
pub mod scene;
pub mod scene3d;
pub mod session;
pub mod workspace;

pub use workspace::workspace_path;
