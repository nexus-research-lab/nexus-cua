//! Constant-time transport-token verification.

use nexus_cua_protocol::AuthorizationToken;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

const MINIMUM_TOKEN_BYTES: usize = 32;

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct TokenVerifier {
    digest: [u8; 32],
}

impl TokenVerifier {
    pub(crate) fn new(token: &AuthorizationToken) -> Option<Self> {
        (token.expose().len() >= MINIMUM_TOKEN_BYTES).then(|| Self {
            digest: Sha256::digest(token.expose().as_bytes()).into(),
        })
    }

    pub(crate) fn accepts(&self, candidate: &AuthorizationToken) -> bool {
        let candidate_digest: [u8; 32] = Sha256::digest(candidate.expose().as_bytes()).into();
        bool::from(self.digest.ct_eq(&candidate_digest))
    }
}
