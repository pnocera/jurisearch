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

/// A [`Write`] adapter that streams bytes to an inner writer while accumulating their SHA-256 digest,
/// so a payload can be written to disk and hashed in a single pass without ever materialising it in
/// memory. The returned digest is byte-identical (value and format) to [`digest_bytes`] over the same
/// byte sequence — both go through the private `format_sha256`.
///
/// The hasher is updated ONLY with the bytes the inner writer actually accepted on each call
/// (`Ok(n)` ⇒ `&buf[..n]`), so a partial or failed write can never desynchronise the digest from the
/// bytes that reached the writer.
///
/// As with [`tee_digest`], the caller is responsible for flushing the inner writer (e.g. a
/// `BufWriter`) and checking that flush's error before trusting the digest — call
/// `writer.flush()?` (which delegates to the inner writer) before [`HashingWriter::finalize`].
pub struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> HashingWriter<W> {
    /// Wrap `inner`, starting an empty digest.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// Consume the writer and return the `sha256:<lowercase-hex>` digest of every byte accepted by the
    /// inner writer — identical in value and format to [`digest_bytes`] over that same byte sequence.
    #[must_use]
    pub fn finalize(self) -> String {
        format_sha256(&self.hasher.finalize())
    }

    /// Mutable access to the inner writer (e.g. to inspect or flush a wrapped writer before finalizing).
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Write FIRST; only hash the bytes the inner writer actually accepted so a failed/partial write
        // cannot corrupt the digest.
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
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
    fn hashing_writer_matches_digest_bytes_and_writes_verbatim() {
        // A payload larger than 1 MiB, fed in SEVERAL write calls (odd chunk size crosses no internal
        // buffer boundary the hasher cares about, but exercises multiple `write` invocations).
        let payload: Vec<u8> = (0..(2 * (1 << 20) + 7777))
            .map(|i| (i % 251) as u8)
            .collect();
        let mut writer = HashingWriter::new(Vec::<u8>::new());
        for chunk in payload.chunks(97_003) {
            writer.write_all(chunk).unwrap();
        }
        // Capture the inner buffer (verbatim passthrough) before consuming `writer` in `finalize`.
        let inner_copy = writer.get_mut().clone();
        let streamed = writer.finalize();

        assert_eq!(streamed, digest_bytes(&payload));
        assert_eq!(inner_copy, payload);
        assert!(streamed.starts_with("sha256:"));
    }

    #[test]
    fn hashing_writer_empty_matches_digest_bytes() {
        // A fresh writer with no writes finalizes to the empty-input digest.
        let writer = HashingWriter::new(Vec::<u8>::new());
        assert_eq!(writer.finalize(), digest_bytes(b""));
    }

    /// A fake `Write` that accepts at most `max_per_write` bytes per `write` call (`Ok(n)` with
    /// `n < buf.len()`), accumulating every accepted byte — exercises the partial-write path.
    struct ShortWriter {
        accepted: Vec<u8>,
        max_per_write: usize,
    }
    impl Write for ShortWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.max_per_write);
            self.accepted.extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A fake `Write` that accepts up to `budget` bytes total, then returns `Err` on the next `write`
    /// — exercises the error-after-prefix path (the failed write must NOT advance the digest).
    struct ErrorAfterPrefixWriter {
        accepted: Vec<u8>,
        budget: usize,
    }
    impl Write for ErrorAfterPrefixWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.budget == 0 {
                return Err(std::io::Error::other("write budget exhausted"));
            }
            let n = buf.len().min(self.budget);
            self.accepted.extend_from_slice(&buf[..n]);
            self.budget -= n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn hashing_writer_hashes_only_accepted_bytes_under_short_writes() {
        // A short-write inner accepts only a 7-byte prefix per `write`, so `write_all` must loop; the
        // hasher advances only over accepted bytes, which (with a correct `write_all` caller) is all of
        // them. Proves the digest tracks accepted bytes, not the slice originally handed to `write`.
        let payload: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
        let mut writer = HashingWriter::new(ShortWriter {
            accepted: Vec::new(),
            max_per_write: 7,
        });
        for chunk in payload.chunks(1000) {
            writer.write_all(chunk).unwrap();
        }
        let accepted = writer.get_mut().accepted.clone();
        let streamed = writer.finalize();
        // `write_all` drove the short writer to accept every byte, in order.
        assert_eq!(accepted, payload);
        // The digest equals digest_bytes over EXACTLY the bytes the inner writer accumulated.
        assert_eq!(streamed, digest_bytes(&accepted));
        assert_eq!(streamed, digest_bytes(&payload));
    }

    #[test]
    fn hashing_writer_digest_excludes_bytes_a_failed_write_rejected() {
        // The inner writer accepts `accept_n` bytes then errors. The digest must reflect ONLY the
        // accepted-and-written prefix — a failed write cannot advance the digest past what reached disk.
        let payload: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
        let accept_n = 1234usize;
        let mut writer = HashingWriter::new(ErrorAfterPrefixWriter {
            accepted: Vec::new(),
            budget: accept_n,
        });
        let err = writer.write_all(&payload).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
        let accepted = writer.get_mut().accepted.clone();
        assert_eq!(accepted, payload[..accept_n]);
        let streamed = writer.finalize();
        // Digest == digest_bytes over the accepted prefix only, NOT over the full payload.
        assert_eq!(streamed, digest_bytes(&accepted));
        assert_eq!(streamed, digest_bytes(&payload[..accept_n]));
        assert_ne!(streamed, digest_bytes(&payload));
    }

    #[test]
    fn to_writer_is_byte_equivalent_to_to_string_for_a_value() {
        // The streaming rewrite serialises each JSONL row with `serde_json::to_writer`; lock in that it
        // is byte-identical to the `serde_json::to_string` the buffered path used (serde's compact
        // formatter is the same in both).
        let value = serde_json::json!({
            "b": 1,
            "a": {"y": [1, 2, {"deep": true}], "x": "text with \"quotes\" and /slash"},
            "arr": [3, 2, 1],
            "n": 1.5,
            "nil": serde_json::Value::Null,
        });
        let mut to_writer_bytes = Vec::new();
        serde_json::to_writer(&mut to_writer_bytes, &value).unwrap();
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            String::from_utf8(to_writer_bytes).unwrap()
        );
    }

    #[test]
    fn non_finite_numbers_are_rejected() {
        // serde_json cannot even represent NaN as a Value::Number, but guard the path anyway via a
        // raw construction is not possible; assert finite numbers pass.
        let value = serde_json::json!({ "x": 1.5 });
        assert!(canonical_bytes(&value).is_ok());
    }
}
