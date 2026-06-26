# Q&A — 20260624-142131

## Question

Design question for the jurisearch repo (`/home/pierre/Work/jurisearch`).

I'm implementing `ingest enrich-zones`: an eager backfill that resolves ~494,701 Cour de cassation
decisions on the Judilibre/PISTE API (~2 HTTP calls each) and upserts official zones into the
`decision_zones` table, via the existing `enrich_decision_from_judilibre(&ManagedPostgres, &str)` helper
in `crates/jurisearch-cli/src/main.rs`. Sequential, this is network-latency-bound at ~3 decisions/s ≈
~40h; PISTE allows ~20 req/s burst.

Constraint I hit: `ManagedPostgres` is NOT `Send`/`Sync` — its advisory `DataDirLock` holds a live
`postgres::Connection` (a `Pin<Box<dyn Stream + Send>>` that is not `Sync`), so I cannot share
`&ManagedPostgres` across `std::thread::scope` workers.

Which approach is best for jurisearch (not just expedient)? Candidates I see:
- (a) Sequential — simplest/correct, ~40h background job.
- (b) Worker threads that each build their own `postgres::Client` from `postgres.connection_string()`
  (a `String`), via a thread-safe enrichment variant that takes connection coordinates instead of
  `&ManagedPostgres` (refactors `enrich_decision_from_judilibre` / its storage callees).
- (c) Producer/consumer: worker threads do only the Judilibre HTTP; DB reads/writes funnel to the main
  thread that owns `&ManagedPostgres`.
- (d) Process sharding: a `--shard-index/--shard-count` predicate (`hashtext(document_id) % n`) so N
  separate `enrich-zones` processes cover disjoint decisions, no shared in-process state.

Consider: correctness/resumability, blast radius on the shipped `fetch --part --online` path (which also
calls `enrich_decision_from_judilibre`), the single-writer/flock model of this embedded Postgres, review
surface, and operability. Recommend ONE and say why, plus the main risk. Concise.

## Answer

# Decision: concurrency model for `ingest enrich-zones`

## Recommendation: (b)

Use **worker threads where each worker opens its own `postgres::Client` from `postgres.connection_string()`**, and refactor the enrichment core so it does not require `&ManagedPostgres`.

Keep one main `ManagedPostgres` in the command to:

- start/own the embedded Postgres lifecycle;
- hold the startup lock and advisory data-dir lock;
- page candidate `document_id`s with `enrich_zone_candidates_json`;
- compute final coverage/status.

Then pass only owned, `Send` data into workers:

```rust
let connection_string = postgres.connection_string();
let doc_ids: Vec<String> = ...;
```

Each worker should create:

```rust
let mut db = postgres::Client::connect(&connection_string, postgres::NoTls)?;
let piste = PisteClient::new(OfficialApiConfig::from_env());
```

and call a thread-safe core:

```rust
fn enrich_decision_from_judilibre_with_client<C: postgres::GenericClient>(
    db: &mut C,
    piste: &PisteClient,
    document_id: &str,
) -> Result<Option<Value>, ErrorObject>
```

Keep the shipped fetch path as a thin wrapper:

```rust
fn enrich_decision_from_judilibre(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<Option<Value>, ErrorObject> {
    let mut db = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(storage_error_object)?;
    let piste = PisteClient::new(OfficialApiConfig::from_env());
    enrich_decision_from_judilibre_with_client(&mut db, &piste, document_id)
}
```

## Why this is the best fit

### It respects the embedded-Postgres ownership model

`ManagedPostgres` is correctly not `Send`/`Sync`: it owns lifecycle, startup lock, advisory lock, and `Drop` stops the server. Do not fight that.

But the lock model is not a “single DB client only” model. Once one `ManagedPostgres` owns the running server, multiple ordinary `postgres::Client` connections are fine. Postgres handles concurrent writes to `decision_zones`; the table is keyed by `document_id` and upsert is idempotent.

### It keeps resumability simple

Every worker still writes the same per-decision cache row:

- `ok`
- `not_found`
- `unsupported`
- `invalid_offsets`
- `upstream_error`

Re-runs skip fresh cache rows via the existing candidate query. If the process dies, completed rows remain completed. No extra queue table is needed.

### It has the smallest honest review surface

The storage layer already has the key primitive:

```rust
upsert_decision_zones_with_client<C: GenericClient>(...)
```

Add client-based read equivalents for the current `execute_sql` helpers:

```rust
fn decision_resolution_metadata_with_client<C: GenericClient>(
    client: &mut C,
    document_id: &str,
) -> Result<Option<DecisionResolutionMetadata>, StorageError>
```

Optionally add a typed cache read helper if the backfill needs it. For eager backfill, workers mostly need resolution metadata + upsert; the main thread already selected stale/missing candidates.

The existing `fetch --part --online` behavior stays stable through the wrapper. That is safer than rewriting fetch around channels.

### It gives enough throughput without process sharding

At ~2 HTTP calls per decision, the bottleneck is Judilibre latency, not local DB writes. A bounded worker pool, e.g. `--concurrency 6..10`, should move from ~3 decisions/s toward the PISTE limit without needing multiple OS processes.

Use conservative defaults and let the operator tune:

```text
--concurrency 6   # or 8, not 32
```

Add rate-limit-aware handling:

- keep existing retry/backoff from `PisteClient`;
- cache `upstream_error`/429 outcomes with short TTL or count them clearly;
- report `errors` separately from `fallback`.

## Why not the others

### (a) Sequential

Correct but wastes the API allowance and turns a mechanical backfill into a ~40h job. Fine as a fallback mode (`--concurrency 1`), not the primary design.

### (c) Producer/consumer with DB on main thread

This avoids worker DB clients, but it forces a bigger split of the current helper:

- main thread reads metadata;
- workers fetch Judilibre;
- main thread validates/upserts;
- channels carry success/error/cache payloads.

That is more moving parts than needed. It also makes the shipped `fetch --part --online` path diverge from the eager path unless you factor even more aggressively.

### (d) Process sharding

Bad fit for the current embedded index contract.

`open_index()` / `ManagedPostgres::start_durable()` acquires:

- filesystem startup lock: `jurisearch-storage.lock`;
- Postgres advisory data-dir lock;
- lifecycle ownership, including stopping Postgres on `Drop`.

N shard processes all trying to own the same embedded data dir will either block/fail on locks or require a new “attach to already-running server without ownership” mode. That is a separate architecture change and a worse operational story than one owner process with worker DB connections.

## Main risk

The main risk is **overdriving Judilibre / PISTE** and creating noisy transient `upstream_error` cache rows.

Mitigation:

- default concurrency conservatively (`6` or `8`);
- make `--concurrency 1` the deterministic fallback;
- bound page size;
- reuse one `PisteClient` and one DB client per worker;
- keep per-decision upserts idempotent;
- log/report status counts by `decision_zones.status`;
- do not mark transient API failures as permanent forever: use short TTL for `upstream_error`.

## Bottom line

Choose **(b)**: one owning `ManagedPostgres` in the main command, plus bounded worker threads with their own `postgres::Client`s and `PisteClient`s. It is the best balance of correctness, throughput, reviewability, and compatibility with the existing embedded-Postgres contract.
