//! Opaque identifiers crossing the public protocol boundary.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

macro_rules! opaque_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Creates an identifier from an already validated opaque value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the opaque wire value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

opaque_id!(
    RequestId,
    "Client-generated idempotency identity for one request."
);
opaque_id!(
    SessionId,
    "Runtime-issued identity for an authorized session."
);
opaque_id!(AppRef, "Session-scoped opaque application reference.");
opaque_id!(
    DiscoveryRef,
    "Short-lived runtime-local reference for one discovered process generation."
);
opaque_id!(WindowRef, "Session-scoped opaque window reference.");
opaque_id!(
    ObservationId,
    "Identity for one immutable window observation."
);
opaque_id!(
    ElementRef,
    "Observation-scoped opaque accessibility element reference."
);
opaque_id!(
    ArtifactRef,
    "Session-scoped opaque transient artifact reference."
);

/// Transport authorization token whose debug output is always redacted.
#[derive(Clone, Deserialize, JsonSchema, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct AuthorizationToken(String);

impl AuthorizationToken {
    /// Creates a token from host-controlled input.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the token only to the transport verifier.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AuthorizationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthorizationToken([REDACTED])")
    }
}

/// User input whose debug output is always redacted.
#[derive(Clone, Deserialize, JsonSchema, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct SensitiveText(String);

impl SensitiveText {
    /// Wraps text supplied for a desktop input action.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the text only to the selected platform action implementation.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SensitiveText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveText([REDACTED])")
    }
}
