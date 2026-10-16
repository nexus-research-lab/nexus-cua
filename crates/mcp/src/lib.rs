//! Bounded local MCP adapter for Nexus Computer Use Runtime.

mod authority;
mod runner;
mod server;

pub use authority::{
    AuthorityChallenge, AuthorityContext, AuthorityError, AuthorityGrant, AuthorityGrantMetadata,
    AuthorityPolicy, AuthorityProvider, AuthorityProviderError, current_unix_ms,
};
pub use runner::{
    AuthoritySource, DEFAULT_MAX_IMAGE_BYTES, DEFAULT_MAX_OBSERVATION_AGE_SECONDS,
    HARD_MAX_IMAGE_BYTES, HARD_MAX_OBSERVATION_AGE_SECONDS, McpHttpConfig, McpHttpReady,
    McpHttpResponseMode, McpServeConfig, McpServeError, serve_http, serve_http_until, serve_stdio,
};
pub use server::{BootstrapError, MCP_SERVER_NAME, NexusCuaMcp, ToolProfile};
