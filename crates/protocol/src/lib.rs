//! Versioned, model-neutral wire contract for Nexus CUA.
//!
//! Public identifiers are opaque and session scoped. No platform process or
//! window handle is exposed as authority.

mod action;
mod capability;
mod command;
mod error;
mod geometry;
mod identifiers;
mod observation;
mod session;

pub use action::*;
pub use capability::*;
pub use command::*;
pub use error::*;
pub use geometry::*;
pub use identifiers::*;
pub use observation::*;
pub use session::*;

/// Stable protocol identifier for the v1 wire format.
pub const PROTOCOL_VERSION: &str = "nexus.cua.v1";
