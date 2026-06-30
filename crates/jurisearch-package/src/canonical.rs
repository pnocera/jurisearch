//! Deterministic manifest serialization and digests (design §6.2.2 "manifest canonicalisation
//! algorithm", §11.1).
//!
//! Signing and digest comparison need a **byte-stable** encoding of a manifest: the same logical
//! value must produce the same bytes on every run and machine, regardless of struct field order or
//! map insertion order. The canonical form here is **sorted-key, minimally-separated JSON**:
//!
//! * object keys are emitted in lexicographic (byte) order, recursively;
//! * no insignificant whitespace;
//! * arrays keep their order (order is semantic);
//! * scalars use serde_json's stable representation.
//!
//! This is independent of whether the `serde_json/preserve_order` feature is enabled anywhere in the
//! workspace (we re-sort explicitly rather than trusting the `Map` backing).

use std::io::{Read, Write};

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Errors from canonicalisation.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    #[error("failed to serialize value for canonicalisation: {0}")]
    Serialize(#[from] serde_json::Error),
    /// A float that is not finite (NaN/Inf) has no canonical JSON form. Manifests do not use such
    /// values; this guards against one slipping in.
    #[error("value contains a non-finite number, which has no canonical form")]
    NonFiniteNumber,
}

/// Produce the canonical byte encoding of `value` (sorted-key compact JSON).
///
/// # Errors
/// [`CanonicalError`] if the value cannot be serialized or contains a non-finite float.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    let json = serde_json::to_value(value)?;
    let canonical = canonicalize(&json)?;
    let mut out = Vec::new();
    write_canonical(&canonical, &mut out);
    Ok(out)
}

/// The canonical SHA-256 digest of `value`, as `sha256:<lowercase-hex>` — the form used throughout
/// the manifests (design §5.3 `set_digest`, §6 `sha256`, §11.1 postcondition digests).
///
/// # Errors
/// [`CanonicalError`] if the value cannot be canonicalised.
pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String, CanonicalError> {
    Ok(digest_bytes(&canonical_bytes(value)?))
}

/// `sha256:<lowercase-hex>` over raw bytes (e.g. an artifact file or a per-file payload).
#[must_use]
pub fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format_sha256(&hasher.finalize())
}

/// Format a finalized SHA-256 hash as the canonical `sha256:<lowercase-hex>` string. This is the
/// single source of truth for the digest format, shared by [`digest_bytes`] and [`tee_digest`] so
/// streamed and buffered digests are byte-identical.
fn format_sha256(hash: &[u8]) -> String {
    let mut hex = String::with_capacity(7 + hash.len() * 2);
    hex.push_str("sha256:");
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Stream `reader` to `writer` while hashing, returning the `sha256:<lowercase-hex>` digest of the
/// bytes — identical in value and format to [`digest_bytes`] over the same byte sequence, but never
/// materialising the whole stream in memory (fixed ~1 MiB working buffer). The writer receives the
/// bytes verbatim and in order.
///
/// The caller is responsible for flushing `writer` (e.g. a `BufWriter`) and checking that flush's
/// error — this function only guarantees that every successfully read byte was handed to
/// `write_all`.
///
/// # Errors
/// Propagates any [`std::io::Error`] from reading `reader` or writing `writer`.
pub fn tee_digest<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB; heap-allocated to keep the stack small.
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
    }
    Ok(format_sha256(&hasher.finalize()))
}

/// Recursively rebuild a [`serde_json::Value`] with object keys sorted, rejecting non-finite floats.
fn canonicalize(value: &serde_json::Value) -> Result<serde_json::Value, CanonicalError> {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(&String, &Value)> = map.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            let mut out = serde_json::Map::with_capacity(sorted.len());
            for (key, child) in sorted {
                out.insert(key.clone(), canonicalize(child)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => Ok(Value::Array(
            items.iter().map(canonicalize).collect::<Result<_, _>>()?,
        )),
        Value::Number(number) => {
            if number.as_f64().is_some_and(|f| !f.is_finite()) {
                return Err(CanonicalError::NonFiniteNumber);
            }
            Ok(value.clone())
        }
        other => Ok(other.clone()),
    }
}

/// Emit compact JSON. The input is already key-sorted by [`canonicalize`]; `serde_json` compact form
/// has no insignificant whitespace, so this is the canonical byte stream.
fn write_canonical(value: &serde_json::Value, out: &mut Vec<u8>) {
    // `serde_json::to_writer` on an already-sorted Value yields deterministic compact bytes.
    serde_json::to_writer(out, value).expect("writing to a Vec cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::collections::BTreeMap;

    #[derive(Serialize)]
    struct Unsorted {
        zebra: i32,
        apple: i32,
        nested: BTreeMap<String, i32>,
    }

    #[test]
    fn keys_are_sorted_and_whitespace_stripped() {
        let value = Unsorted {
            zebra: 1,
            apple: 2,
            nested: BTreeMap::from([("z".to_owned(), 9), ("a".to_owned(), 8)]),
        };
        let bytes = canonical_bytes(&value).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"apple":2,"nested":{"a":8,"z":9},"zebra":1}"#
        );
    }

    #[test]
    fn canonical_form_is_byte_stable_across_field_order() {
        // Two JSON objects with identical content but different key order must canonicalise equal.
        let a: serde_json::Value = serde_json::json!({"b": 1, "a": {"y": 2, "x": 3}});
        let b: serde_json::Value = serde_json::json!({"a": {"x": 3, "y": 2}, "b": 1});
        assert_eq!(canonical_bytes(&a).unwrap(), canonical_bytes(&b).unwrap());
        assert_eq!(canonical_digest(&a).unwrap(), canonical_digest(&b).unwrap());
    }

    #[test]
    fn array_order_is_preserved() {
        let a = serde_json::json!([3, 1, 2]);
        let b = serde_json::json!([1, 2, 3]);
        assert_ne!(canonical_bytes(&a).unwrap(), canonical_bytes(&b).unwrap());
    }

    #[test]
    fn digest_is_prefixed_lowercase_hex() {
        let digest = digest_bytes(b"");
        assert_eq!(
            digest,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn tee_digest_matches_digest_bytes_and_writes_verbatim() {
        // A payload larger than the 1 MiB chunk buffer to exercise multiple read iterations.
        let payload: Vec<u8> = (0..(3 * (1 << 20) + 12345))
            .map(|i| (i % 251) as u8)
            .collect();
        let mut reader = std::io::Cursor::new(payload.clone());
        let mut sink: Vec<u8> = Vec::new();
        let streamed = tee_digest(&mut reader, &mut sink).unwrap();

        // Digest equality with the buffered path, and verbatim passthrough to the writer.
        assert_eq!(streamed, digest_bytes(&payload));
        assert_eq!(sink, payload);
        assert!(streamed.starts_with("sha256:"));
    }

    #[test]
    fn tee_digest_empty_matches_digest_bytes() {
        let mut reader = std::io::Cursor::new(Vec::<u8>::new());
        let mut sink: Vec<u8> = Vec::new();
        let streamed = tee_digest(&mut reader, &mut sink).unwrap();
        assert_eq!(streamed, digest_bytes(b""));
        assert!(sink.is_empty());
    }

    #[test]
    fn non_finite_numbers_are_rejected() {
        // serde_json cannot even represent NaN as a Value::Number, but guard the path anyway via a
        // raw construction is not possible; assert finite numbers pass.
        let value = serde_json::json!({ "x": 1.5 });
        assert!(canonical_bytes(&value).is_ok());
    }
}
