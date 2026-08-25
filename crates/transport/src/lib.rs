//! Authenticated, local-only IPC for Nexus CUA.
//!
//! The transport implementation is intentionally separate from the runtime:
//! embedders can use the Rust API directly without opening a local endpoint.

mod auth;
mod client;
mod dispatcher;
mod endpoint;
mod error;
mod frame;
mod server;
#[cfg(windows)]
mod windows_security;

pub use client::request;
pub use dispatcher::Dispatcher;
pub use endpoint::{LocalEndpoint, ServerConfig};
pub use error::TransportError;
pub use server::serve_until;
