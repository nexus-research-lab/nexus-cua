//! Constant-time transport-token verification.

use nexus_cua_protocol::AuthorizationToken;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

const MINIMUM_TOKEN_BYTES: usize = 32;
const MAXIMUM_TOKEN_BYTES: usize = 4 * 1024;

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct TokenVerifier {
    digest: [u8; 32],
}

impl TokenVerifier {
    pub(crate) fn new(token: &AuthorizationToken) -> Option<Self> {
        (MINIMUM_TOKEN_BYTES..=MAXIMUM_TOKEN_BYTES)
            .contains(&token.expose().len())
            .then(|| Self {
                digest: Sha256::digest(token.expose().as_bytes()).into(),
            })
    }

    pub(crate) fn accepts(&self, candidate: &AuthorizationToken) -> bool {
        if !(MINIMUM_TOKEN_BYTES..=MAXIMUM_TOKEN_BYTES).contains(&candidate.expose().len()) {
            return false;
        }
        let candidate_digest: [u8; 32] = Sha256::digest(candidate.expose().as_bytes()).into();
        bool::from(self.digest.ct_eq(&candidate_digest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_configured_and_candidate_tokens() {
        let valid = AuthorizationToken::new("a".repeat(MINIMUM_TOKEN_BYTES));
        let verifier = TokenVerifier::new(&valid).expect("valid token");
        assert!(verifier.accepts(&valid));
        assert!(!verifier.accepts(&AuthorizationToken::new(
            "a".repeat(MAXIMUM_TOKEN_BYTES + 1)
        )));
        assert!(TokenVerifier::new(&AuthorizationToken::new("short")).is_none());
    }
}
