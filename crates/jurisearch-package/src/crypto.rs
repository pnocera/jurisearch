//! The trust seam: `Signer`/`Verifier` traits, the wire signature type (design §11.2), and the
//! concrete **Ed25519** scheme behind those traits (plan P6).
//!
//! P0 fixed the **shape** of signing — what a signature carries (`key_id`/epoch/algorithm) and the
//! `sign`/`verify` operations over canonical manifest bytes — behind a trait so the concrete scheme is
//! swappable (conception §6 LSP). P6 lands that scheme: [`Ed25519Signer`] (producer-side, holds the
//! private key) and the rotation-tolerant [`Ed25519Verifier`] (client-side, built from installed
//! [`TrustAnchor`]s — a client holds only public keys and cannot forge). The test-only [`StubSigner`]
//! / [`AcceptAllVerifier`] remain for the data-path loopback slices.

use crate::canonical::{CanonicalError, canonical_bytes};
use serde::{Deserialize, Serialize};

/// The key identity stamped into every manifest (design §11.2). Opaque to the contract — the
/// concrete scheme defines its meaning (KMS key arn, cert fingerprint, …).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    /// The signature scheme this signer emits (e.g. `"ed25519"`, `"stub"`). The producer stamps this
    /// into `manifest.integrity.signature_algorithm` BEFORE sealing, so the descriptive field matches
    /// the authoritative `Signed.signature.algorithm` (plan P6 — no more hard-coded `"stub"`).
    fn algorithm(&self) -> &str;

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
    #[error("malformed key material: {0}")]
    MalformedKey(String),
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
    fn algorithm(&self) -> &str {
        "stub"
    }

    fn sign_bytes(&self, _canonical: &[u8]) -> Result<Signature, SignError> {
        Ok(Signature {
            algorithm: "stub".to_owned(),
            key_id: KeyId("stub".to_owned()),
            key_epoch: KeyEpoch(0),
            signature_hex: String::new(),
        })
    }
}

/// The wire token for the concrete scheme (plan P6). The `Signer`/`Verifier` reject any other.
pub const ED25519_ALGORITHM: &str = "ed25519";

/// Decode `value` as STRICT lowercase hex of EXACTLY `expected_bytes` bytes (plan P6 r1 WARN). Rejects
/// uppercase A–F, non-hex characters, odd length, and the wrong byte count — so each byte string has a
/// single accepted wire encoding (`hex::decode` alone would accept uppercase). `None` on any violation.
fn decode_lower_hex_exact(value: &str, expected_bytes: usize) -> Option<Vec<u8>> {
    if value.len() != expected_bytes * 2 {
        return None;
    }
    if !value
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    hex::decode(value).ok()
}

/// A configured producer verifying key the client trusts (plan P6). Built from the client's
/// `jurisearch_control.trust_anchor` rows into an [`Ed25519Verifier`]; the `algorithm` must be
/// [`ED25519_ALGORITHM`] and `public_key_hex` a 32-byte lowercase-hex Ed25519 public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustAnchor {
    pub key_id: KeyId,
    pub key_epoch: KeyEpoch,
    pub algorithm: String,
    pub public_key_hex: String,
}

/// The concrete producer-side Ed25519 signer (plan P6): holds the private signing key and the
/// `(key_id, key_epoch)` it stamps into every [`Signature`]. Construct from a 32-byte seed so builds
/// are deterministic and key material never needs an ambient RNG.
#[derive(Clone)]
pub struct Ed25519Signer {
    signing_key: ed25519_dalek::SigningKey,
    key_id: KeyId,
    key_epoch: KeyEpoch,
}

impl std::fmt::Debug for Ed25519Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the private key.
        f.debug_struct("Ed25519Signer")
            .field("key_id", &self.key_id)
            .field("key_epoch", &self.key_epoch)
            .finish_non_exhaustive()
    }
}

impl Ed25519Signer {
    /// Build a signer from a 32-byte Ed25519 seed and its published key identity.
    #[must_use]
    pub fn from_seed(seed: &[u8; 32], key_id: KeyId, key_epoch: KeyEpoch) -> Self {
        Self {
            signing_key: ed25519_dalek::SigningKey::from_bytes(seed),
            key_id,
            key_epoch,
        }
    }

    /// The lowercase-hex 32-byte public key to publish as a client [`TrustAnchor`].
    #[must_use]
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing_key.verifying_key().to_bytes())
    }

    #[must_use]
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    #[must_use]
    pub fn key_epoch(&self) -> KeyEpoch {
        self.key_epoch
    }

    /// The [`TrustAnchor`] a client must install to verify this signer's signatures.
    #[must_use]
    pub fn trust_anchor(&self) -> TrustAnchor {
        TrustAnchor {
            key_id: self.key_id.clone(),
            key_epoch: self.key_epoch,
            algorithm: ED25519_ALGORITHM.to_owned(),
            public_key_hex: self.public_key_hex(),
        }
    }
}

impl Signer for Ed25519Signer {
    fn algorithm(&self) -> &str {
        ED25519_ALGORITHM
    }

    fn sign_bytes(&self, canonical: &[u8]) -> Result<Signature, SignError> {
        use ed25519_dalek::Signer as _;
        let sig = self.signing_key.sign(canonical);
        Ok(Signature {
            algorithm: ED25519_ALGORITHM.to_owned(),
            key_id: self.key_id.clone(),
            key_epoch: self.key_epoch,
            signature_hex: hex::encode(sig.to_bytes()),
        })
    }
}

/// The concrete client-side Ed25519 verifier (plan P6): a rotation-tolerant set of trusted producer
/// keys, keyed by `(key_id, key_epoch)`. A signature verifies iff its `(key_id, key_epoch)` is present
/// AND the bytes check out — so removing an anchor revokes that key, and a key from an unknown epoch is
/// rejected. The production client builds this from its `trust_anchor` rows; it is NEVER an
/// accept-everything fallback (an empty set rejects all signatures with `UnknownKey`).
#[derive(Debug, Clone, Default)]
pub struct Ed25519Verifier {
    trust_anchors: std::collections::BTreeMap<(KeyId, KeyEpoch), ed25519_dalek::VerifyingKey>,
}

impl Ed25519Verifier {
    /// Build a verifier from the client's configured trust anchors. Fails if any anchor names a
    /// non-Ed25519 algorithm or carries malformed (non-hex / wrong-length) key material.
    ///
    /// # Errors
    /// [`VerifyError::UnsupportedAlgorithm`] or [`VerifyError::MalformedKey`].
    pub fn from_anchors(anchors: &[TrustAnchor]) -> Result<Self, VerifyError> {
        let mut trust_anchors = std::collections::BTreeMap::new();
        for anchor in anchors {
            if anchor.algorithm != ED25519_ALGORITHM {
                return Err(VerifyError::UnsupportedAlgorithm(anchor.algorithm.clone()));
            }
            let bytes = decode_lower_hex_exact(&anchor.public_key_hex, 32).ok_or_else(|| {
                VerifyError::MalformedKey(format!(
                    "public_key_hex must be 64 lowercase-hex chars (32 bytes), got `{}`",
                    anchor.public_key_hex
                ))
            })?;
            let key_bytes: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .expect("decode_lower_hex_exact guarantees 32 bytes");
            let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
                .map_err(|error| VerifyError::MalformedKey(error.to_string()))?;
            trust_anchors.insert((anchor.key_id.clone(), anchor.key_epoch), verifying_key);
        }
        Ok(Self { trust_anchors })
    }

    /// How many anchors are configured (an empty verifier rejects everything).
    #[must_use]
    pub fn anchor_count(&self) -> usize {
        self.trust_anchors.len()
    }
}

impl Verifier for Ed25519Verifier {
    fn verify_bytes(&self, canonical: &[u8], signature: &Signature) -> Result<(), VerifyError> {
        if signature.algorithm != ED25519_ALGORITHM {
            return Err(VerifyError::UnsupportedAlgorithm(
                signature.algorithm.clone(),
            ));
        }
        let verifying_key = self
            .trust_anchors
            .get(&(signature.key_id.clone(), signature.key_epoch))
            .ok_or_else(|| VerifyError::UnknownKey(signature.key_id.0.clone()))?;
        let sig_bytes =
            decode_lower_hex_exact(&signature.signature_hex, 64).ok_or(VerifyError::Invalid)?;
        let dalek_sig =
            ed25519_dalek::Signature::from_slice(&sig_bytes).map_err(|_| VerifyError::Invalid)?;
        verifying_key
            .verify_strict(canonical, &dalek_sig)
            .map_err(|_| VerifyError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_signer() -> Ed25519Signer {
        Ed25519Signer::from_seed(&[7u8; 32], KeyId("producer-k1".to_owned()), KeyEpoch(1))
    }

    #[test]
    fn ed25519_round_trips_and_rejects_tamper_unknown_key_and_wrong_algorithm() {
        let signer = test_signer();
        let verifier = Ed25519Verifier::from_anchors(&[signer.trust_anchor()]).unwrap();
        let value = serde_json::json!({"package_id": "core-1-1", "n": 42});

        let signature = sign_value(&signer, &value).unwrap();
        assert_eq!(signature.algorithm, "ed25519");
        verify_value(&verifier, &value, &signature).expect("valid signature verifies");

        // Tampered payload → Invalid.
        let tampered = serde_json::json!({"package_id": "core-1-1", "n": 43});
        assert!(matches!(
            verify_value(&verifier, &tampered, &signature),
            Err(VerifyError::Invalid)
        ));

        // Unknown key (verifier with no/other anchors) → UnknownKey.
        let empty = Ed25519Verifier::default();
        assert!(matches!(
            verify_value(&empty, &value, &signature),
            Err(VerifyError::UnknownKey(_))
        ));

        // Wrong algorithm token → UnsupportedAlgorithm.
        let mut wrong_algo = signature.clone();
        wrong_algo.algorithm = "stub".to_owned();
        assert!(matches!(
            verify_value(&verifier, &value, &wrong_algo),
            Err(VerifyError::UnsupportedAlgorithm(_))
        ));
    }

    #[test]
    fn ed25519_verifier_rejects_a_signature_from_a_different_key() {
        let signer = test_signer();
        let other =
            Ed25519Signer::from_seed(&[9u8; 32], KeyId("producer-k1".to_owned()), KeyEpoch(1));
        // Same key_id/epoch ANCHOR but a DIFFERENT private key — bytes must not verify.
        let verifier = Ed25519Verifier::from_anchors(&[signer.trust_anchor()]).unwrap();
        let value = serde_json::json!({"x": 1});
        let forged = sign_value(&other, &value).unwrap();
        assert!(matches!(
            verify_value(&verifier, &value, &forged),
            Err(VerifyError::Invalid)
        ));
    }

    #[test]
    fn ed25519_verifier_construction_rejects_malformed_anchor() {
        let anchor = |hex: &str| TrustAnchor {
            key_id: KeyId("k".to_owned()),
            key_epoch: KeyEpoch(0),
            algorithm: "ed25519".to_owned(),
            public_key_hex: hex.to_owned(),
        };
        // Non-hex, too short, and UPPERCASE (strict lowercase wire form) all fail construction.
        let good_hex = package_signer_pubkey_hex();
        assert!(matches!(
            Ed25519Verifier::from_anchors(&[anchor("not-hex")]),
            Err(VerifyError::MalformedKey(_))
        ));
        assert!(matches!(
            Ed25519Verifier::from_anchors(&[anchor("ab12")]),
            Err(VerifyError::MalformedKey(_))
        ));
        assert!(
            matches!(
                Ed25519Verifier::from_anchors(&[anchor(&good_hex.to_uppercase())]),
                Err(VerifyError::MalformedKey(_))
            ),
            "uppercase hex is rejected by the strict lowercase wire form"
        );
        // The lowercase original is accepted.
        assert!(Ed25519Verifier::from_anchors(&[anchor(&good_hex)]).is_ok());
    }

    fn package_signer_pubkey_hex() -> String {
        test_signer().public_key_hex()
    }

    #[test]
    fn decode_lower_hex_exact_is_strict() {
        assert_eq!(decode_lower_hex_exact("ab12", 2), Some(vec![0xab, 0x12]));
        assert_eq!(
            decode_lower_hex_exact("AB12", 2),
            None,
            "uppercase rejected"
        );
        assert_eq!(
            decode_lower_hex_exact("ab1", 2),
            None,
            "odd length rejected"
        );
        assert_eq!(
            decode_lower_hex_exact("ab12", 3),
            None,
            "wrong byte count rejected"
        );
        assert_eq!(decode_lower_hex_exact("zz12", 2), None, "non-hex rejected");
    }

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
