//! The producer remote-manifest builder (plan P9, design §6.2.1 / §9.4).
//!
//! Builds + signs a `Signed<RemoteManifest>` for one corpus from the producer `package_catalog`
//! (chain identity/order/status) AND the PUBLISHED artifacts (sizes, compatibility, the embedded
//! signature). The catalog is not a publish catalog — it has no sizes/URIs — so the builder reads each
//! artifact's `Signed<EmbeddedManifest>` from the deterministic published root and binds the catalog
//! `package_digest` to the embedded `integrity.artifact_sha256`. The retention model (§9.4): the
//! `active_baseline` is the newest media root; `packages` lists ONLY the retained incremental chain
//! after it (re-baselines are media roots, never `RemotePackageEntry`).

use std::collections::BTreeMap;
use std::path::Path;

use jurisearch_package::canonical::canonical_digest;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::manifest::remote::{
    BaselineRef, CatchupMode, CatchupPolicy, CatchupRange, EntitlementListing, EntitlementTier,
    RemoteManifest, RemotePackageEntry, SigningInfo,
};
use jurisearch_package::sequence::PackageSequence;
use jurisearch_package::signed::Signed;
use jurisearch_package::{Corpus, KeyId, PackageKind, Signer};

use jurisearch_storage::package_catalog::{
    CatalogRow, acquire_corpus_build_lock, catalog_rows_for_corpus, release_corpus_build_lock,
};
use jurisearch_storage::runtime::{ManagedPostgres, StorageError};

use crate::error::BuildError;
use crate::publish::{artifact_dir_size_bytes, published_package_dir};

/// Producer-supplied inputs for the remote manifest (the policy + presentation the catalog/artifacts
/// don't carry). Estimates are producer-declared (calibration is P10).
#[derive(Debug, Clone)]
pub struct RemoteManifestParams {
    pub publisher: String,
    pub environment: String,
    /// RFC3339 build timestamp (passed in so the builder stays clock-free + deterministic).
    pub generated_at: String,
    pub catchup_policy: CatchupPolicy,
    pub entitlement_tier: EntitlementTier,
    pub license_epoch: u32,
    pub audience: Option<String>,
    pub signing_key_id: KeyId,
    /// Base for `artifact_uri` (e.g. `media://` or `https://dist/`); the per-corpus/package path is
    /// appended. The client's `DirectoryCatchupSource` resolves these to local dirs under its root.
    pub uri_base: String,
    /// Retention window — keep at most this many of the newest incrementals after the active media root.
    pub max_retained_incrementals: usize,
    /// Producer-declared reference-client estimates (P10 calibrates these from measured sizes).
    pub default_apply_seconds: u32,
    pub default_load_seconds: u32,
}

/// Build + sign the remote manifest for `corpus` from the catalog + the published artifacts under
/// `published_root`. Serialized against package build/publish via the per-corpus build lock so it never
/// observes a half-built chain.
///
/// # Errors
/// [`BuildError`] if no media root is published, an artifact is missing, a catalog/embedded digest
/// disagree, the retained chain has a gap, or on a DB/IO/signing failure.
pub fn build_remote_manifest(
    producer: &ManagedPostgres,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    params: &RemoteManifestParams,
) -> Result<Signed<RemoteManifest>, BuildError> {
    let corpus_typed = Corpus::new(corpus.to_owned())?;
    let mut db = producer.client()?;
    acquire_corpus_build_lock(&mut db, corpus)?;
    let result = build_inner(
        &mut db,
        corpus,
        &corpus_typed,
        published_root,
        signer,
        params,
    );
    let _ = release_corpus_build_lock(&mut db, corpus);
    result
}

fn build_inner(
    db: &mut postgres::Client,
    corpus: &str,
    corpus_typed: &Corpus,
    published_root: &Path,
    signer: &dyn Signer,
    params: &RemoteManifestParams,
) -> Result<Signed<RemoteManifest>, BuildError> {
    let rows = catalog_rows_for_corpus(db, corpus)?;
    let media = rows
        .iter()
        .rfind(|r| r.package_kind == "baseline" || r.package_kind == "rebaseline")
        .ok_or_else(|| missing(format!("no media root cataloged for corpus `{corpus}`")))?;

    // The active baseline: read its embedded manifest from the published root.
    let media_signed = read_published_manifest(published_root, corpus, &media.package_id)?;
    verify_catalog_identity(media, &media_signed, false)?;
    let media_dir = published_package_dir(published_root, corpus, &media.package_id);
    let media_size = artifact_dir_size_bytes(&media_dir)?;
    let media_id = media.package_sequence;
    let active_baseline = BaselineRef {
        baseline_id: media_signed.payload.identity.baseline_id.clone(),
        generation: media_signed.payload.identity.generation.clone(),
        package_kind: media_signed.payload.identity.package_kind,
        sequence: media_signed.payload.identity.to_sequence,
        schema_version: media_signed.payload.compatibility.schema_version,
        minimum_client_version: media_signed.payload.compatibility.minimum_client_version,
        artifact_uri: artifact_uri(params, corpus, &media.package_id),
        compressed_size_bytes: media_size,
        uncompressed_size_bytes: media_size,
        estimated_load_seconds: params.default_load_seconds,
        sha256: media_signed.payload.integrity.artifact_sha256.clone(),
        signature: media_signed.signature.clone(),
    };

    // Retained incrementals AFTER the media root (newest `max_retained_incrementals`), in order.
    let mut incrementals: Vec<&CatalogRow> = rows
        .iter()
        .filter(|r| r.package_kind == "incremental" && r.package_sequence > media_id)
        .collect();
    incrementals.sort_by_key(|r| r.package_sequence);
    if incrementals.len() > params.max_retained_incrementals {
        let drop = incrementals.len() - params.max_retained_incrementals;
        incrementals.drain(0..drop);
    }

    let mut packages = Vec::with_capacity(incrementals.len());
    for row in &incrementals {
        let signed = read_published_manifest(published_root, corpus, &row.package_id)?;
        verify_catalog_identity(row, &signed, true)?;
        let dir = published_package_dir(published_root, corpus, &row.package_id);
        let size = artifact_dir_size_bytes(&dir)?;
        packages.push(RemotePackageEntry {
            package_id: row.package_id.clone(),
            from_sequence: signed.payload.identity.from_sequence,
            to_sequence: signed.payload.identity.to_sequence,
            artifact_uri: artifact_uri(params, corpus, &row.package_id),
            compressed_size_bytes: size,
            uncompressed_size_bytes: size,
            estimated_apply_seconds: params.default_apply_seconds,
            row_counts: row_counts(&signed.payload),
            requires_baseline: false,
            minimum_client_version: signed.payload.compatibility.minimum_client_version,
            schema_version: signed.payload.compatibility.schema_version,
            embedding_fingerprint: signed.payload.compatibility.embedding_fingerprint.clone(),
            builder_versions: signed.payload.compatibility.builder_versions.clone(),
            sha256: signed.payload.integrity.artifact_sha256.clone(),
            signature: signed.signature.clone(),
        });
    }

    // The retained chain must be gap-free from `min_available` to `head`; otherwise the published feed
    // is incoherent (lower `head` to a coherent prefix by publishing a re-baseline — never ship a hole).
    let media_seq = active_baseline.sequence.get();
    let (head_sequence, min_available_sequence) = if packages.is_empty() {
        (media_seq, media_seq)
    } else {
        ensure_gap_free(&packages)?;
        let head = packages.last().expect("non-empty").to_sequence.get();
        let min_avail = packages.first().expect("non-empty").from_sequence.get();
        (head, min_avail)
    };

    // A bounded `RequiresBaseline` range for clients below the retained window (P7 honours bounded
    // ranges; the `min_available_sequence` check already routes those, so this is the explicit signal).
    let catchup_ranges = if min_available_sequence > 1 {
        vec![CatchupRange {
            from_sequence: PackageSequence::new(0),
            to_sequence: Some(PackageSequence::new(min_available_sequence - 1)),
            mode: CatchupMode::RequiresBaseline,
            baseline_id: Some(active_baseline.baseline_id.clone()),
        }]
    } else {
        Vec::new()
    };

    let manifest = RemoteManifest {
        manifest_version: 1,
        generated_at: params.generated_at.clone(),
        publisher: params.publisher.clone(),
        corpus: corpus_typed.clone(),
        environment: params.environment.clone(),
        head_sequence: PackageSequence::new(head_sequence),
        min_available_sequence: PackageSequence::new(min_available_sequence),
        active_baseline,
        packages,
        catchup_ranges,
        catchup_policy: params.catchup_policy.clone(),
        entitlement: EntitlementListing {
            corpus: corpus_typed.clone(),
            tier: params.entitlement_tier,
            license_epoch: params.license_epoch,
            audience: params.audience.clone(),
        },
        signing: SigningInfo {
            key_id: params.signing_key_id.clone(),
            algorithm: signer.algorithm().to_owned(),
        },
    };
    Ok(Signed::seal(manifest, signer)?)
}

fn artifact_uri(params: &RemoteManifestParams, corpus: &str, package_id: &str) -> String {
    format!("{}{corpus}/packages/{package_id}", params.uri_base)
}

fn read_published_manifest(
    root: &Path,
    corpus: &str,
    package_id: &str,
) -> Result<Signed<EmbeddedManifest>, BuildError> {
    let path = jurisearch_package::artifact::manifest_path(&published_package_dir(
        root, corpus, package_id,
    ));
    let bytes = std::fs::read(&path).map_err(|error| {
        BuildError::Storage(StorageError::Generations {
            message: format!("published artifact `{package_id}` is missing ({error})"),
        })
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Bind a published embedded manifest to its catalog row's FULL identity (plan P9 r1 BLOCKER) — not
/// just the payload aggregate (`artifact_sha256` would let a changed package id / kind / sequence /
/// compatibility / postconditions slip through with the same payload bytes). Recomputes the canonical
/// EMBEDDED-manifest digest and checks every identity field; for a retained incremental, also enforces
/// the incremental shape (`to == from + 1`).
fn verify_catalog_identity(
    row: &CatalogRow,
    signed: &Signed<EmbeddedManifest>,
    expect_incremental: bool,
) -> Result<(), BuildError> {
    let manifest = &signed.payload;
    if row.package_digest.as_deref() != Some(manifest.integrity.artifact_sha256.as_str()) {
        return Err(missing(format!(
            "published artifact `{}` payload digest {} != cataloged {:?}",
            row.package_id, manifest.integrity.artifact_sha256, row.package_digest
        )));
    }
    let canonical = canonical_digest(manifest).map_err(|error| missing(error.to_string()))?;
    if row.manifest_digest.as_deref() != Some(canonical.as_str()) {
        return Err(missing(format!(
            "published artifact `{}` embedded-manifest digest {canonical} != cataloged {:?}",
            row.package_id, row.manifest_digest
        )));
    }
    let mismatch = |field: &str, found: String| {
        missing(format!(
            "published artifact `{}` {field} `{found}` != cataloged row",
            row.package_id
        ))
    };
    if manifest.identity.package_id != row.package_id {
        return Err(mismatch("package_id", manifest.identity.package_id.clone()));
    }
    if manifest.identity.package_kind.as_str() != row.package_kind {
        return Err(mismatch(
            "package_kind",
            manifest.identity.package_kind.as_str().to_owned(),
        ));
    }
    if manifest.identity.baseline_id != row.baseline_id {
        return Err(mismatch(
            "baseline_id",
            manifest.identity.baseline_id.clone(),
        ));
    }
    if manifest.identity.generation != row.generation {
        return Err(mismatch("generation", manifest.identity.generation.clone()));
    }
    if i64::try_from(manifest.identity.to_sequence.get()).unwrap_or(i64::MAX)
        != row.package_sequence
    {
        return Err(mismatch(
            "to_sequence",
            manifest.identity.to_sequence.get().to_string(),
        ));
    }
    if expect_incremental {
        if manifest.identity.package_kind != PackageKind::Incremental {
            return Err(mismatch(
                "package_kind (expected incremental)",
                manifest.identity.package_kind.as_str().to_owned(),
            ));
        }
        if manifest.identity.to_sequence.get() != manifest.identity.from_sequence.next().get() {
            return Err(missing(format!(
                "incremental `{}` is not a +1 link ({} -> {})",
                row.package_id,
                manifest.identity.from_sequence.get(),
                manifest.identity.to_sequence.get()
            )));
        }
    }
    Ok(())
}

fn ensure_gap_free(packages: &[RemotePackageEntry]) -> Result<(), BuildError> {
    for window in packages.windows(2) {
        if window[0].to_sequence.get() != window[1].from_sequence.get() {
            return Err(missing(format!(
                "retained incremental chain has a gap: {} -> {} then {} -> {}",
                window[0].from_sequence.get(),
                window[0].to_sequence.get(),
                window[1].from_sequence.get(),
                window[1].to_sequence.get(),
            )));
        }
    }
    Ok(())
}

fn row_counts(manifest: &EmbeddedManifest) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for op in &manifest.apply.operations {
        *counts.entry(op.table.clone()).or_insert(0) += op.count;
    }
    counts
}

fn missing(message: String) -> BuildError {
    BuildError::Storage(StorageError::Generations { message })
}
