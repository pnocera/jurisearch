# Review: work/09-jurisearch-cli/03-deployment-design.md

## Findings

### BLOCKER: `QueryStore` is specified as both a `dyn` trait and a non-object-safe trait

The design makes `QueryStore` the central dependency-inversion seam, but its sketch cannot be used the way the same document uses it. `QueryStore::in_snapshot<T>(&self, f: impl FnOnce(...))` is generic and takes `impl Trait` (`03-deployment-design.md:110-113`), while `ServerContext` stores `pub store: &'a dyn QueryStore` (`03-deployment-design.md:205-207`). In Rust, a trait with a generic method is not object-safe, so the `dyn QueryStore` handler context cannot compile as drawn. Because this is the core read-side abstraction, implementers would have to invent a second interface or abandon the dispatcher shape, undermining the stated DRY/DIP design.

Recommended fix: choose one concrete shape and make the document internally consistent. Either make the dispatcher and handlers generic over `S: QueryStore`, or make `QueryStore` object-safe, for example by exposing `fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, QueryError>` or another non-generic snapshot handle API. Then update `ServerContext` and `OperationHandler` to use that same shape.

### BLOCKER: `ServerContext::corpora` creates a second active-topology path outside the read snapshot

The design says every read happens inside one snapshot and that active corpora are exposed by `ReadSnapshot::active_corpora()` (`03-deployment-design.md:110-118`), and the multi-corpus algorithm explicitly resolves active corpora from `self.active_corpora()` inside `ReadSnapshot::search` (`03-deployment-design.md:241-245`). But the dispatcher context also carries `pub corpora: &'a [ActiveCorpus]` (`03-deployment-design.md:205-209`). That gives handlers a request path that can use a corpus list resolved outside the snapshot that also reads readiness and runs retrieval. During activation, that is exactly the kind of split topology the architecture is trying to rule out. It also weakens the "one authority" claim for active-generation resolution.

Recommended fix: remove resolved corpora from `ServerContext`. Keep only long-lived server dependencies there, such as the store and embedder. Require each operation handler to run inside the `QueryStore` snapshot API and obtain active corpora/readiness/search through `ReadSnapshot` only. If a request needs corpus selection, pass the requested filter into the snapshot-scoped query method, not a pre-resolved topology.

### WARN: The JSONL framing authority still points at the heavy CLI module

Section 3.8 says the one JSONL framing authority wraps the existing framing in `crates/jurisearch-cli/src/serve.rs` and that "the server side keeps that loop, the client side gets the symmetric counterpart" (`03-deployment-design.md:220-231`). The DRY ledger repeats `serve.rs` as the JSONL authority (`03-deployment-design.md:296-300`). But the current `serve.rs` is not a dependency-free codec: it imports CLI args/output/session dispatch, injects `index_dir`, owns listener binding, and delegates to the local session dispatcher (`crates/jurisearch-cli/src/serve.rs:16-18`, `:20-32`, `:75-103`, `:125-201`). That contradicts the target crate graph where the thin client depends only on the contract plus transport and excludes the heavy stack (`03-deployment-design.md:273-278`). If the thin client cannot depend on `serve.rs`, it will either duplicate the framing rules or pull in the wrong crate.

Recommended fix: move the authority boundary in the design. Name a dependency-light transport/codec module or crate, such as `jurisearch-transport` or `jurisearch-contract::jsonl`, that owns request/response encode/decode, newline framing, max-line behavior, protocol-version rejection, and canonical transport errors. Keep listener setup, timeouts, server-owned context binding, and `index_dir` stripping in the server composition layer. Both `JsonlClient` and the server accept loop should call the shared codec.

### WARN: The renderer extraction is underspecified and can easily violate the crate boundary

The design says response rendering lives in `jurisearch-contract` so the thin client and one-shot CLI render identically without storage/embed dependencies (`03-deployment-design.md:92-93`, `:273-275`, `:296-298`). Current code does not yet have that abstraction: `SessionResponse` carries an untyped `serde_json::Value` result in `jurisearch-core`, the session response writer only serializes JSONL, and command output is produced by CLI-local payload/output functions (`crates/jurisearch-core/src/session.rs:17`, `crates/jurisearch-cli/src/output.rs:69-76`, `crates/jurisearch-cli/src/retrieval/search.rs:5-8`). As written, "renderer in contract" could mean either a JSON-only writer, which does not satisfy the human/`--json` parity claim, or operation-specific formatting in the lowest crate, which would turn the contract crate into a high-churn command-format crate.

Recommended fix: specify the extracted rendering boundary explicitly. Either introduce typed query-operation response DTOs plus dependency-free renderers in a shared no-heavy crate, or split the design into `jurisearch-contract` for wire DTOs and a separate `jurisearch-render` crate for human/JSON formatting that both the heavy CLI and thin client use. Avoid putting storage, embedding, or handler logic behind the renderer boundary.

### NIT: The OCP wording overstates what the closed `Operation` enum can provide

The design says new query operations are added by registering a handler, "not by editing a match in the core loop" (`03-deployment-design.md:198-214`), but the operation vocabulary itself is a closed enum in the shared wire contract (`03-deployment-design.md:84-90`). Adding an operation will still require a contract change and client/server version negotiation even if the dispatcher lookup is registry-based.

Recommended fix: tighten the wording: the dispatcher loop is closed to modification once an `Operation` exists, but adding a new operation remains an explicit wire-contract change. That keeps the OCP claim accurate without hiding the protocol-version impact.

VERDICT: FIXES_REQUIRED
