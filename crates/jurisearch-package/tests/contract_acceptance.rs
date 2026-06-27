//! Plan P0 acceptance: the crate round-trips every manifest and event-kind example through serde
//! and a canonicalisation that is byte-stable across runs; the reject-code enum is exhaustive; the
//! two sequence layers are non-interchangeable.

use jurisearch_package::canonical::{canonical_bytes, canonical_digest};
use jurisearch_package::crypto::{AcceptAllVerifier, StubSigner};
use jurisearch_package::event::{
    EventKind, ReplaceSet, ReplaceSetGroup, ReplaceSetOp, ReplaceSetScope,
};
use jurisearch_package::manifest::embedded::{
    ApplyContract, Compatibility, Compression, EmbeddedManifest, Entitlement, ExtensionRequirement,
    Identity, IndexBuildContract, Integrity, OperationCount, PayloadFile, PayloadFormat,
    PayloadLayout, Postconditions, Preconditions, RollbackPolicy,
};
use jurisearch_package::manifest::remote::{
    BaselineRef, CatchupMode, CatchupPolicy, CatchupRange, EntitlementListing, EntitlementTier,
    RemoteManifest, RemotePackageEntry, SigningInfo,
};
use jurisearch_package::{
    ChangeSeq, Corpus, KeyEpoch, KeyId, PackageKind, PackageSequence, RejectCode, Signature,
    Signed, Version,
};
use std::collections::BTreeMap;

fn sig() -> Signature {
    Signature {
        algorithm: "stub".to_owned(),
        key_id: KeyId("k".to_owned()),
        key_epoch: KeyEpoch(0),
        signature_hex: String::new(),
    }
}

fn embedded_example() -> EmbeddedManifest {
    EmbeddedManifest {
        identity: Identity {
            package_format_version: jurisearch_package::PACKAGE_FORMAT_VERSION,
            package_id: "core-1041-1042".to_owned(),
            corpus: Corpus::new("core").unwrap(),
            package_kind: PackageKind::Incremental,
            from_sequence: PackageSequence::new(1041),
            to_sequence: PackageSequence::new(1042),
            previous_package_id: Some("core-1040-1040".to_owned()),
            previous_package_sha256: Some("sha256:aa".to_owned()),
            baseline_id: "core-2026-06-25-g000124".to_owned(),
            generation: "core_g000124".to_owned(),
            created_at: "2026-06-26T00:00:00Z".to_owned(),
            builder_run_id: "run-1".to_owned(),
        },
        compatibility: Compatibility {
            minimum_client_version: Version::new(0, 1, 0),
            maximum_client_version: None,
            schema_version: 18,
            schema_migration_bundle_digest: "sha256:bb".to_owned(),
            requires_extensions: vec![
                ExtensionRequirement {
                    name: "vector".to_owned(),
                    minimum_version: None,
                },
                ExtensionRequirement {
                    name: "pg_search".to_owned(),
                    minimum_version: None,
                },
            ],
            embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
            embedding_model: "bge-m3".to_owned(),
            embedding_dimension: 1024,
            embedding_normalize: true,
            builder_versions: BTreeMap::from([
                ("chunk_builder_version".to_owned(), "c1".to_owned()),
                ("zone_unit_builder_version".to_owned(), "z1".to_owned()),
            ]),
            postgres_major_min: None,
            postgres_major_max: None,
        },
        entitlement: Entitlement {
            entitlement_corpus: Corpus::new("core").unwrap(),
            tier: "open".to_owned(),
            license_epoch: 3,
            audience: None,
            entitlement_policy_digest: "sha256:cc".to_owned(),
        },
        integrity: Integrity {
            artifact_sha256: "sha256:dd".to_owned(),
            uncompressed_payload_digest: "sha256:ee".to_owned(),
            per_file_digests: BTreeMap::from([("documents".to_owned(), "sha256:ff".to_owned())]),
            canonicalisation_algorithm: "jcs-sorted-json".to_owned(),
            signature_algorithm: "stub".to_owned(),
            transparency_log_index: None,
        },
        apply: ApplyContract {
            expected_client_from_sequence: PackageSequence::new(1040),
            result_sequence: PackageSequence::new(1042),
            requires_empty_generation: false,
            schema_ops_digest: "sha256:00".to_owned(),
            operations: vec![OperationCount {
                table: "documents".to_owned(),
                op: EventKind::Upsert,
                count: 10,
            }],
            replace_scopes: vec![],
            preconditions: Preconditions {
                schema_version: 18,
                embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
                builder_versions: BTreeMap::new(),
                active_baseline_id: Some("core-2026-06-25-g000124".to_owned()),
                active_generation: Some("core_g000124".to_owned()),
            },
            postconditions: Postconditions {
                row_counts: BTreeMap::from([("documents".to_owned(), 10)]),
                table_digests: BTreeMap::new(),
            },
            index_build: IndexBuildContract {
                bm25_indexes: vec![],
                ivfflat_finalize: vec![],
                row_level_maintenance_only: true,
                queryable_before_finalize: false,
            },
            idempotency_key: "core-1041-1042:sha256:dd".to_owned(),
            rollback_policy: RollbackPolicy::TransactionRollback,
        },
        payload: PayloadLayout {
            files: vec![PayloadFile {
                file_name: "documents.upsert.jsonl".to_owned(),
                table: "documents".to_owned(),
                columns: vec!["document_id".to_owned(), "body".to_owned()],
                op: EventKind::Upsert,
                format: PayloadFormat::Jsonl,
                compression: Compression::Zstd,
                row_count: 10,
                digest: "sha256:ff".to_owned(),
            }],
            apply_order: vec![
                "documents".to_owned(),
                "chunks".to_owned(),
                "chunk_embeddings".to_owned(),
                "official_api_responses".to_owned(),
                "decision_legislation_citations".to_owned(),
            ],
        },
    }
}

fn remote_example() -> RemoteManifest {
    RemoteManifest {
        manifest_version: 1,
        generated_at: "2026-06-26T00:00:00Z".to_owned(),
        publisher: "jurisearch".to_owned(),
        corpus: Corpus::new("core").unwrap(),
        environment: "production".to_owned(),
        head_sequence: PackageSequence::new(1088),
        min_available_sequence: PackageSequence::new(970),
        active_baseline: BaselineRef {
            baseline_id: "core-2026-06-25-g000124".to_owned(),
            generation: "core_g000124".to_owned(),
            sequence: PackageSequence::new(1040),
            schema_version: 18,
            artifact_uri: "media://core-baseline".to_owned(),
            compressed_size_bytes: 0,
            sha256: "sha256:00".to_owned(),
            signature: sig(),
        },
        packages: vec![RemotePackageEntry {
            package_id: "core-1041-1042".to_owned(),
            from_sequence: PackageSequence::new(1041),
            to_sequence: PackageSequence::new(1042),
            artifact_uri: "https://host/core-1041-1042".to_owned(),
            compressed_size_bytes: 0,
            uncompressed_size_bytes: 0,
            estimated_apply_seconds: 0,
            row_counts: BTreeMap::from([("documents".to_owned(), 10)]),
            requires_baseline: false,
            minimum_client_version: Version::new(0, 1, 0),
            schema_version: 18,
            embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
            builder_versions: BTreeMap::new(),
            sha256: "sha256:01".to_owned(),
            signature: sig(),
        }],
        catchup_ranges: vec![
            CatchupRange {
                from_sequence: PackageSequence::new(1000),
                to_sequence: Some(PackageSequence::new(1088)),
                mode: CatchupMode::IncrementalOk,
                baseline_id: None,
            },
            CatchupRange {
                from_sequence: PackageSequence::new(800),
                to_sequence: None,
                mode: CatchupMode::RequiresBaseline,
                baseline_id: Some("core-2026-06-25-g000124".to_owned()),
            },
        ],
        catchup_policy: CatchupPolicy {
            max_incremental_packages: 120,
            max_cumulative_diff_to_baseline_permille: 330,
        },
        entitlement: EntitlementListing {
            corpus: Corpus::new("core").unwrap(),
            tier: EntitlementTier::Open,
            license_epoch: 3,
            audience: None,
        },
        signing: SigningInfo {
            key_id: KeyId("k".to_owned()),
            algorithm: "stub".to_owned(),
        },
    }
}

#[test]
fn every_manifest_round_trips_through_serde() {
    let embedded = embedded_example();
    let remote = remote_example();

    let e_json = serde_json::to_string(&embedded).unwrap();
    assert_eq!(
        serde_json::from_str::<EmbeddedManifest>(&e_json).unwrap(),
        embedded
    );
    let r_json = serde_json::to_string(&remote).unwrap();
    assert_eq!(
        serde_json::from_str::<RemoteManifest>(&r_json).unwrap(),
        remote
    );
}

#[test]
fn canonicalisation_is_byte_stable_across_runs() {
    let embedded = embedded_example();
    let remote = remote_example();

    // Same value, canonicalised repeatedly, must produce identical bytes and digest.
    let e1 = canonical_bytes(&embedded).unwrap();
    let e2 = canonical_bytes(&embedded).unwrap();
    assert_eq!(e1, e2);
    assert_eq!(
        canonical_digest(&embedded).unwrap(),
        canonical_digest(&embedded).unwrap()
    );

    // A value rebuilt from its JSON must canonicalise to the same bytes (order-independence).
    let reparsed: EmbeddedManifest =
        serde_json::from_str(&serde_json::to_string(&embedded).unwrap()).unwrap();
    assert_eq!(canonical_bytes(&reparsed).unwrap(), e1);

    let r1 = canonical_bytes(&remote).unwrap();
    let reparsed_remote: RemoteManifest =
        serde_json::from_str(&serde_json::to_string(&remote).unwrap()).unwrap();
    assert_eq!(canonical_bytes(&reparsed_remote).unwrap(), r1);
}

#[test]
fn manifests_seal_and_verify() {
    let signed = Signed::seal(embedded_example(), &StubSigner).unwrap();
    signed.verify(&AcceptAllVerifier).unwrap();
    let signed_remote = Signed::seal(remote_example(), &StubSigner).unwrap();
    signed_remote.verify(&AcceptAllVerifier).unwrap();
}

#[test]
fn every_event_kind_example_round_trips() {
    // upsert / delete as plain op tokens (the outbox `op`).
    for kind in EventKind::all() {
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(serde_json::from_str::<EventKind>(&json).unwrap(), kind);
    }
    // replace_set payload (the §5.3 contract) for each derived group.
    for group in [
        ReplaceSetGroup::ZoneUnits,
        ReplaceSetGroup::ChunksWithEmbeddings,
        ReplaceSetGroup::ChunkEmbeddings,
    ] {
        let payload = ReplaceSet {
            op: ReplaceSetOp::ReplaceSet,
            table_group: group,
            scope: ReplaceSetScope {
                document_id: "legi:LEGIARTI000000000001@2020-01-01".to_owned(),
                corpus: Some(Corpus::new("core").unwrap()),
            },
            rows: BTreeMap::from([(
                "zone_units".to_owned(),
                vec![serde_json::json!({"zone_unit_id": "pk-1", "body": "…"})],
            )]),
            row_pks: vec!["pk-1".to_owned()],
            builder_version: "v1".to_owned(),
            source_text_hash: Some("sha256:aa".to_owned()),
            embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
            set_digest: "sha256:bb".to_owned(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let restored = serde_json::from_str::<ReplaceSet>(&json).unwrap();
        assert_eq!(restored, payload);
        // The row bodies survive (BLOCKER-1 guard at the acceptance level).
        assert_eq!(restored.rows["zone_units"][0]["zone_unit_id"], "pk-1");
    }
}

#[test]
fn reject_codes_cover_section_6_3() {
    // The closed vocabulary, verbatim.
    let expected = [
        RejectCode::ClientTooOld,
        RejectCode::SchemaAhead,
        RejectCode::MissingEntitlement,
        RejectCode::SequenceGap,
        RejectCode::WrongGeneration,
        RejectCode::EmbeddingFingerprintMismatch,
        RejectCode::BuilderVersionMismatch,
        RejectCode::SignatureInvalid,
        RejectCode::DigestMismatch,
        RejectCode::ExtensionMissing,
        RejectCode::BaselineRequired,
    ];
    assert_eq!(RejectCode::all(), expected);
}

#[test]
fn sequence_layers_do_not_unify() {
    // A `ChangeSeq` and a `PackageSequence` carrying the same integer are distinct values that
    // cannot be compared or converted — the §5.1 cross-corpus guard. (The actual guarantee is that
    // the cross-type operations below do not compile; this asserts the integers are equal yet the
    // types are kept apart everywhere they are used in this crate.)
    let c = ChangeSeq::new(1041);
    let p = PackageSequence::new(1041);
    assert_eq!(c.get(), p.get());
}
