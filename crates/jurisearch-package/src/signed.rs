//! A detached-signature wrapper for whole signed documents (design §6.2, §11.1).
//!
//! Both manifests are "signed and self-sufficient" (§6.2). Rather than embed a signature *inside*
//! the bytes it signs (which forces a fragile sign-around-the-signature-field dance), a signed
//! document is modelled as `{ payload, signature }` where the signature is computed over the
//! **canonical bytes of `payload` alone** (§canonical). This makes signing and verification
//! symmetric and the payload immutable.

use crate::canonical::CanonicalError;
use crate::crypto::{Signature, Signer, Verifier, VerifyError};
use serde::{Deserialize, Serialize};

/// A payload plus a detached signature over its canonical bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed<T> {
    pub payload: T,
    pub signature: Signature,
}

impl<T: Serialize> Signed<T> {
    /// Sign `payload` with `signer`, producing a self-contained signed document. Accepts a
    /// `&dyn Signer` (the signer is `?Sized`) so a runtime-selected signer works.
    ///
    /// # Errors
    /// Propagates a [`crate::crypto::SignError`] from the signer.
    pub fn seal(
        payload: T,
        signer: &(impl Signer + ?Sized),
    ) -> Result<Self, crate::crypto::SignError> {
        let signature = crate::crypto::sign_value(signer, &payload)?;
        Ok(Self { payload, signature })
    }

    /// Verify the signature over the payload's canonical bytes (§11.1 steps 1 & 3). Accepts a
    /// `&dyn Verifier` (the verifier is `?Sized`) so client code can inject a runtime-selected,
    /// rotation-tolerant verifier.
    ///
    /// # Errors
    /// [`VerifyError`] if canonicalisation fails or the signature does not verify.
    pub fn verify(&self, verifier: &(impl Verifier + ?Sized)) -> Result<(), VerifyError> {
        crate::crypto::verify_value(verifier, &self.payload, &self.signature)
    }

    /// The canonical bytes that were (or would be) signed — useful for digesting.
    ///
    /// # Errors
    /// [`CanonicalError`] if the payload cannot be canonicalised.
    pub fn canonical_payload(&self) -> Result<Vec<u8>, CanonicalError> {
        crate::canonical::canonical_bytes(&self.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{AcceptAllVerifier, StubSigner};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Body {
        corpus: String,
        head: i64,
    }

    #[test]
    fn seal_then_verify_round_trips() {
        let body = Body {
            corpus: "core".to_owned(),
            head: 1088,
        };
        let signed = Signed::seal(body.clone(), &StubSigner).unwrap();
        assert_eq!(signed.payload, body);
        signed.verify(&AcceptAllVerifier).unwrap();

        let json = serde_json::to_string(&signed).unwrap();
        let restored: Signed<Body> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, signed);
    }

    #[test]
    fn seal_and_verify_accept_dyn_trait_objects() {
        // The signed-document API must work with a runtime-selected `&dyn Signer`/`&dyn Verifier`,
        // which is how client code will hold the manifest signer/verifier (WARN: object safety).
        let body = Body {
            corpus: "core".to_owned(),
            head: 1,
        };
        let signer: &dyn Signer = &StubSigner;
        let signed = Signed::seal(body, signer).unwrap();
        let verifier: &dyn Verifier = &AcceptAllVerifier;
        signed.verify(verifier).unwrap();
    }
}
