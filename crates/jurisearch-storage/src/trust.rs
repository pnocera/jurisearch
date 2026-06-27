//! Client-side trust state (plan P6): producer verifying keys (`trust_anchor`) and installed license
//! tokens (`license_token`) in `jurisearch_control`. `syncd` reads these to build the Ed25519 verifier
//! and to enforce entitlement as an apply precondition. This is local client control state — it is
//! NEVER replicated corpus data and never lives in a generation schema.

use jurisearch_package::crypto::TrustAnchor;
use jurisearch_package::license::LicenseToken;
use jurisearch_package::{KeyEpoch, KeyId};
use postgres::GenericClient;

use crate::runtime::StorageError;

/// Trust-anchor purposes (the `purpose` column). Package-artifact signatures vs license-token
/// signatures can be signed by distinct keys; the verifier is built per purpose.
pub const PACKAGE_PURPOSE: &str = "package";
/// See [`PACKAGE_PURPOSE`].
pub const LICENSE_PURPOSE: &str = "license";

/// Install (or update) a producer verifying key the client trusts, for `purpose`.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn install_trust_anchor<C: GenericClient>(
    client: &mut C,
    anchor: &TrustAnchor,
    purpose: &str,
) -> Result<(), StorageError> {
    let key_epoch = i32::try_from(anchor.key_epoch.0).unwrap_or(i32::MAX);
    client
        .execute(
            "INSERT INTO jurisearch_control.trust_anchor \
                 (key_id, key_epoch, algorithm, public_key_hex, purpose) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (key_id, key_epoch, purpose) DO UPDATE \
                 SET algorithm = EXCLUDED.algorithm, public_key_hex = EXCLUDED.public_key_hex;",
            &[
                &anchor.key_id.0,
                &key_epoch,
                &anchor.algorithm,
                &anchor.public_key_hex,
                &purpose,
            ],
        )
        .map(|_| ())
        .map_err(StorageError::PostgresClient)
}

/// Load the client's configured trust anchors for `purpose` (to build an `Ed25519Verifier`).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn load_trust_anchors<C: GenericClient>(
    client: &mut C,
    purpose: &str,
) -> Result<Vec<TrustAnchor>, StorageError> {
    let rows = client
        .query(
            "SELECT key_id, key_epoch, algorithm, public_key_hex \
             FROM jurisearch_control.trust_anchor WHERE purpose = $1 \
             ORDER BY key_id, key_epoch;",
            &[&purpose],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let key_epoch: i32 = row.get("key_epoch");
            TrustAnchor {
                key_id: KeyId(row.get("key_id")),
                key_epoch: KeyEpoch(u32::try_from(key_epoch).unwrap_or(0)),
                algorithm: row.get("algorithm"),
                public_key_hex: row.get("public_key_hex"),
            }
        })
        .collect())
}

/// Install (or replace) a verified license token. `signed_token_json` is the serialized
/// `Signed<LicenseToken>` — kept verbatim so the consumer can RE-verify its signature on every use.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn install_license_token<C: GenericClient>(
    client: &mut C,
    token: &LicenseToken,
    signed_token_json: &str,
) -> Result<(), StorageError> {
    let license_epoch = i32::try_from(token.license_epoch).unwrap_or(i32::MAX);
    client
        .execute(
            "INSERT INTO jurisearch_control.license_token \
                 (corpus, tier, license_epoch, audience, not_after, token_json) \
             VALUES ($1,$2,$3,$4,$5,$6::text::jsonb) \
             ON CONFLICT (corpus, tier, license_epoch, audience) DO UPDATE \
                 SET not_after = EXCLUDED.not_after, token_json = EXCLUDED.token_json;",
            &[
                &token.entitlement_corpus.as_str(),
                &token.tier,
                &license_epoch,
                &token.audience.clone().unwrap_or_default(),
                &token.not_after,
                &signed_token_json,
            ],
        )
        .map(|_| ())
        .map_err(StorageError::PostgresClient)
}

/// The serialized `Signed<LicenseToken>` blobs of every installed token for `corpus`. The `corpus`
/// column is only a cheap INDEX — every security predicate (tier, epoch, AND expiry) is re-derived from
/// the SIGNED payload by the consumer, NOT from the denormalized `tier`/`license_epoch`/`not_after`
/// columns (plan P6 r1 BLOCKER: those columns are local mutable state and must never gate trust). The
/// consumer re-verifies each blob's signature against a license-purpose anchor before trusting it.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn license_token_blobs<C: GenericClient>(
    client: &mut C,
    corpus: &str,
) -> Result<Vec<String>, StorageError> {
    let rows = client
        .query(
            "SELECT token_json::text FROM jurisearch_control.license_token WHERE corpus = $1;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect())
}

/// Whether `not_after` (an RFC3339 string from a SIGNED token payload) is in the future per the DB
/// clock. `None` = no expiry → valid. A malformed timestamp returns `false` (treat as expired/invalid)
/// rather than erroring — and because this runs on an autocommit client, a bad cast does not poison
/// later queries (plan P6 r1 BLOCKER: payload-derived expiry, never the mutable column).
///
/// # Errors
/// Never returns `Err` for a malformed timestamp (maps to `false`); only an unexpected connection
/// failure surfaces as [`StorageError::PostgresClient`].
pub fn token_not_after_in_future(
    client: &mut postgres::Client,
    not_after: Option<&str>,
) -> Result<bool, StorageError> {
    let Some(ts) = not_after else {
        return Ok(true); // no expiry
    };
    match client.query_one("SELECT ($1::timestamptz > now());", &[&ts]) {
        Ok(row) => Ok(row.get::<_, bool>(0)),
        Err(_) => Ok(false), // unparseable expiry → treat as invalid (autocommit: no txn poisoning)
    }
}
