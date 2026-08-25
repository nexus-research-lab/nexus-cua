//! Capability-bounded Computer Use session runtime.
//!
//! The runtime is the sole authority for mapping public opaque references to
//! platform handles. Platform drivers cannot grant authority or silently widen
//! an input route.

mod artifact_store;
mod driver;
mod error;
mod service;
mod session;
mod validation;

pub use driver::*;
pub use error::*;
pub use service::{Runtime, RuntimeConfig};
