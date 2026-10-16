//! Bounded host-side desktop-worker scheduling and remote gateway contracts.
//!
//! The local desktop remains the lightweight default. Independent foreground
//! work, direct network/filesystem enforcement, and machine lifecycle require
//! a distinct worker environment that truthfully advertises those capabilities.

mod environment;
mod gateway;
mod local;
mod registry;
mod wire;

pub use environment::*;
pub use gateway::*;
pub use local::*;
pub use registry::*;
pub use wire::*;
