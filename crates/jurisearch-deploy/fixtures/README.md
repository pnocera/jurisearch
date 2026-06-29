# jurisearch-deploy demo/smoke FIXTURE corpus (M5-B)

The tiny SIGNED fixture corpus that `jurisearchctl demo smoke` and `jurisearchctl site smoke` exercise.
Per macro-plan resolved decision #10: CI/demo acceptance uses this fixture with a DOCUMENTED STABLE
document id; OPERATED single-host acceptance uses a REAL DILA document id after the producer has published
real packages.

## Stable fixture contract (the single source of truth)

These constants are defined ONCE in `crates/jurisearch-deploy/src/ops/fixture.rs` and reused by both
smoke surfaces and the runbooks — never re-typed:

| Constant              | Value                          | Used by                                            |
|-----------------------|--------------------------------|----------------------------------------------------|
| `FIXTURE_CORPUS`      | `core`                         | the single v1 package corpus                       |
| `FIXTURE_DOC_ID`      | `cass:FIXTURE-0000000001`      | `fetch` known-id leg (`--fetch-id` default)        |
| `FIXTURE_QUERY_TERM`  | `responsabilite`               | BM25 + hybrid search legs                          |
| `FIXTURE_MISSING_ID`  | `cass:FIXTURE-DOES-NOT-EXIST`  | NEGATIVE not-found leg (guaranteed absent)         |

`FIXTURE_DOC_ID` is a synthetic but well-formed id reserved for the fixture ONLY; it is never minted by a
real DILA ingest, so the negative `FIXTURE_MISSING_ID` leg is stable across environments.

## Published layout

The static signed artifact is published under the deterministic served root
`crates/jurisearch-deploy/fixtures/published/` (constant `FIXTURE_RELATIVE_ROOT`):

```
published/core/manifest.json                       # the signed RemoteManifest (head + active baseline)
published/core/packages/<package-id>/manifest.json # the signed EmbeddedManifest
published/core/packages/<package-id>/payload/...    # per-table COPY payload
```

A demo `site.toml` points `sync.source_root` at this `published/` directory so the site's
`DirectoryCatchupSource` applies the fixture corpus with the REAL verify/apply path.

## Status: the committed bytes are DEFERRED (live fixture demo currently UNAVAILABLE)

Generating the fixture bytes requires a populated producer PostgreSQL plus pgvector/`pg_search` assets
(the `generate_fixture` step below), which are an authorized-only environment — so `published/` is NOT
committed here yet. What ships now and is fully tested WITHOUT live infra:

- the stable fixture CONSTANTS (`FIXTURE_DOC_ID` / `FIXTURE_QUERY_TERM` / `FIXTURE_MISSING_ID`, above),
- the documented GENERATOR helper `ops::fixture::generate_fixture` (drives the real package-build APIs),
- the smoke/watchdog/demo DECISION-LOGIC unit tests (leg classification, the negative-leg contract
  assertions, no-silent-skip, stalled-vs-no-new-packages), proven with synthetic responses.

Because the bytes are deferred, the LIVE fixture demo (`jurisearchctl demo up` / `demo smoke`) is
explicitly **UNAVAILABLE until the artifact is generated and committed**. This is enforced, not silent:

- `demo up` and `demo smoke` run a PREFLIGHT GUARD (`ops::fixture::ensure_published_artifacts`) that
  FAILS FAST with one actionable diagnostic naming the exact missing path and this generator when
  `published/<corpus>/manifest.json` is absent — never an obscure catch-up/readiness failure.
- the operated acceptance script (`scripts/single-host-demo-acceptance.sh`) SKIPS the fixture demo legs
  WITH AN EXPLICIT RECORDED REASON when the artifact is absent (consistent with no-silent-skip).

## Regenerating the artifact (authorized-only — needs live infra)

The artifact is built by the documented generator helper `ops::fixture::generate_fixture`, which drives
the REAL `jurisearch-package-build` library APIs:

```
build_baseline → publish_package → build_remote_manifest → publish_remote_manifest
```

`generate_fixture` requires a PRODUCER PostgreSQL already populated with the fixture document
(`FIXTURE_DOC_ID`) and a real `Signer` (the fixture signing key, whose public anchor the demo `site.toml`
trusts). This is an authorized-only step that needs live infra — CI does NOT run it. CI instead:

1. unit-tests the smoke/watchdog/demo DECISION logic with synthetic responses (no DB), so the gates are
   proven without standing up a database (this is what runs today, while the bytes are deferred), and
2. once the artifact has been generated and committed under `published/`, the live fixture demo legs
   consume that committed tree (until then the preflight guard keeps them honestly unavailable).

To (re)generate after the producer publishes a fixture baseline:

```rust
use jurisearch_deploy::ops::fixture::generate_fixture;
// `producer` is a DbClientSource for the populated producer DB; `signer` is the fixture signing key.
let artifacts = generate_fixture(
    &producer, &signer,
    Path::new("crates/jurisearch-deploy/fixtures/published"),
    &scratch_dir, &baseline_params, &remote_manifest_params,
)?;
```

Then commit the refreshed `published/` tree. Published package ids are immutable: a content change must
use a new id (see `publish_package`).
