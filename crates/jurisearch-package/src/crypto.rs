//! The trust seam: `Signer`/`Verifier` traits and the wire signature type (design §11.2).
//!
//! P0 fixes the **shape** of signing — what a signature carries (`key_id`/epoch/algorithm) and the
//! `sign`/`verify` operations over canonical manifest bytes — behind a trait so the concrete
//! cryptographic scheme is swappable (conception §6 LSP) and lands later (P6). There is **no**
//! concrete algorithm here; a stub `Verifier` that accepts everything is used for the early loopback
//! slices, and the real implementation replaces it without touching any manifest type.

use crate::canonical::{CanonicalError, canonical_bytes};
use serde::{Deserialize, Serialize};

/// The key identity stamped into every manifest (design §11.2). Opaque to the contract — the
/// concrete scheme defines its meaning (KMS key arn, cert fingerprint, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyId(pub String);

/// The key/cert epoch (design §11.2), so a rotation-tolerant verifier can accept signatures from a
/// prior epoch during rollover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyEpoch(pub u32);

/// A detached signature over a manifest's canonical bytes (design §6.2.2 "integrity & signing").
///
/// The `signature` bytes are base64-free hex so the manifest stays plain JSON-stable; the algorithm
/// token names the scheme the [`Verifier`] must use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// The signature scheme (e.g. `ed25519`, `ecdsa-p256-sha256`). Fixed by the concrete signer.
    pub algorithm: String,
    pub key_id: KeyId,
    pub key_epoch: KeyEpoch,
    /// Lowercase-hex signature bytes over the canonical manifest encoding.
    pub signature_hex: String,
}

/// Signs canonical manifest bytes (producer side, P4/P6). The default [`Signer::sign_value`] handles
/// canonicalisation so implementors only provide the raw-byte primitive [`Signer::sign_bytes`].
///
/// The generic helper is bounded `where Self: Sized` so the trait stays **object-safe**: a service
/// can hold a `Box<dyn Signer>` and call `sign_bytes` on it; for the canonicalise-then-sign
/// convenience, use the free function [`sign_value`] or a concrete `&impl Signer`.
pub trait Signer {
    /// Sign already-canonicalised bytes. The object-safe core primitive.
    fn sign_bytes(&self, canonical: &[u8]) -> Result<Signature, SignError>;

    /// Canonicalise `value` (§canonical) then sign it.
    ///
    /// # Errors
    /// [`SignError`] on canonicalisation or signing failure.
    fn sign_value<T: Serialize>(&self, value: &T) -> Result<Signature, SignError>
    where
        Self: Sized,
    {
        sign_value(self, value)
    }
}

/// Verifies a [`Signature`] over canonical manifest bytes (client side, §11.1). The default
/// [`Verifier::verify_value`] canonicalises so implementors only provide [`Verifier::verify_bytes`].
///
/// The generic helper is bounded `where Self: Sized` so the trait stays **object-safe**: a service
/// can inject a runtime-selected `Box<dyn Verifier>` (e.g. a rotation-tolerant verifier) and call
/// `verify_bytes`; for canonicalise-then-verify, use the free function [`verify_value`].
pub trait Verifier {
    /// Verify a signature over already-canonicalised bytes. The object-safe core primitive.
    ///
    /// # Errors
    /// [`VerifyError`] (mapped by the caller to [`crate::reject::RejectCode::SignatureInvalid`]).
    fn verify_bytes(&self, canonical: &[u8], signature: &Signature) -> Result<(), VerifyError>;

    /// Canonicalise `value` then verify the signature against it.
    ///
    /// # Errors
    /// [`VerifyError`] on canonicalisation failure or an invalid signature.
    fn verify_value<T: Serialize>(
        &self,
        value: &T,
        signature: &Signature,
    ) -> Result<(), VerifyError>
    where
        Self: Sized,
    {
        verify_value(self, value, signature)
    }
}

/// Canonicalise `value` and sign it with any [`Signer`] (incl. a `&dyn Signer`).
///
/// # Errors
/// [`SignError`] on canonicalisation or signing failure.
pub fn sign_value<T: Serialize>(
    signer: &(impl Signer + ?Sized),
    value: &T,
) -> Result<Signature, SignError> {
    let bytes = canonical_bytes(value)?;
    signer.sign_bytes(&bytes)
}

/// Canonicalise `value` and verify `signature` with any [`Verifier`] (incl. a `&dyn Verifier`).
///
/// # Errors
/// [`VerifyError`] on canonicalisation failure or an invalid signature.
pub fn verify_value<T: Serialize>(
    verifier: &(impl Verifier + ?Sized),
    value: &T,
    signature: &Signature,
) -> Result<(), VerifyError> {
    let bytes = canonical_bytes(value)?;
    verifier.verify_bytes(&bytes, signature)
}

/// Signing failure.
#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error("signing failed: {0}")]
    Backend(String),
}

/// Verification failure (the caller maps this to `signature_invalid`, §6.3).
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error("signature does not verify")]
    Invalid,
    #[error("unknown key_id `{0}`")]
    UnknownKey(String),
    #[error("unsupported signature algorithm `{0}`")]
    UnsupportedAlgorithm(String),
}

/// A verifier that accepts any signature. **Test/loopback only** — used by the early baseline slice
/// (P3) so the data path is correct before trust is hardened (P6). Never wired on a real client.
#[derive(Debug, Default, Clone, Copy)]
pub struct AcceptAllVerifier;

impl Verifier for AcceptAllVerifier {
    fn verify_bytes(&self, _canonical: &[u8], _signature: &Signature) -> Result<(), VerifyError> {
        Ok(())
    }
}

/// A signer that emits a fixed, non-cryptographic marker signature. **Test/loopback only** (P3).
/// Pairs with [`AcceptAllVerifier`]; replaced by the real scheme in P6.
#[derive(Debug, Default, Clone)]
pub struct StubSigner;

impl Signer for StubSigner {
    fn sign_bytes(&self, _canonical: &[u8]) -> Result<Signature, SignError> {
        Ok(Signature {
            algorithm: "stub".to_owned(),
            key_id: KeyId("stub".to_owned()),
            key_epoch: KeyEpoch(0),
            signature_hex: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_signer_and_accept_all_verifier_round_trip() {
        let value = serde_json::json!({"package_id": "core-1041-1042"});
        let signer = StubSigner;
        let signature = signer.sign_value(&value).unwrap();
        assert_eq!(signature.algorithm, "stub");
        AcceptAllVerifier
            .verify_value(&value, &signature)
            .expect("stub verifier accepts");
    }

    #[test]
    fn verifier_is_object_safe() {
        // The trait must be usable as `&dyn Verifier` (a runtime-selected verifier).
        let verifier: &dyn Verifier = &AcceptAllVerifier;
        let signature = StubSigner.sign_bytes(b"x").unwrap();
        verifier.verify_bytes(b"x", &signature).unwrap();
        // The canonicalise-then-verify convenience works through the free function on a dyn ref.
        super::verify_value(verifier, &serde_json::json!({"a": 1}), &signature).unwrap();

        // Likewise a `&dyn Signer`.
        let signer: &dyn Signer = &StubSigner;
        let sig = super::sign_value(signer, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(sig.algorithm, "stub");
    }

    #[test]
    fn signature_round_trips_through_serde() {
        let signature = Signature {
            algorithm: "ed25519".to_owned(),
            key_id: KeyId("k1".to_owned()),
            key_epoch: KeyEpoch(3),
            signature_hex: "ab12".to_owned(),
        };
        let json = serde_json::to_string(&signature).unwrap();
        let restored: Signature = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, signature);
    }
}
