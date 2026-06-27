//! work/09 P4 (4B) — `serve-site` argument-ordering contract: transport validation + bind happen
//! BEFORE the embedding stack is probed, so a malformed invocation (or a bind conflict) fails with the
//! expected `bad_input` about the listener, never a tokenizer/endpoint error. (Regression guard for the
//! 4B codex review finding: the eager service embedder must not mask listener-argument errors.)

mod support;
use support::*;

/// A deliberately-broken embedding configuration: if `serve-site` probed the embedder before validating
/// the listener arguments, `PreparedQueryEmbedder::from_env()` would fail here (missing tokenizer), and
/// the command would surface that instead of the listener `bad_input`.
fn with_broken_embedder(mut command: Command) -> Command {
    command
        .env("JURISEARCH_EMBED_PROVIDER", "openai_compatible")
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:1")
        .env("JURISEARCH_EMBED_TOKENIZER_JSON", "/definitely/not/here");
    command
}

#[test]
fn serve_site_rejects_missing_listener_before_probing_the_embedder() {
    let mut command = with_broken_embedder(jurisearch_command_without_embedding_env());
    command.args([
        "serve-site",
        "--db-name",
        "jurisearch",
        "--db-user",
        "jurisearch_read",
    ]);
    // Neither `--tcp` nor `--socket`: the listener bad_input must surface, NOT a tokenizer/endpoint error.
    command
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "serve-site requires exactly one of --tcp or --socket",
        ))
        .stdout(predicate::str::contains("tokenizer").not());
}

#[test]
fn serve_site_rejects_a_non_loopback_bind_before_probing_the_embedder() {
    let mut command = with_broken_embedder(jurisearch_command_without_embedding_env());
    command.args([
        "serve-site",
        "--tcp",
        "8.8.8.8:9099",
        "--db-name",
        "jurisearch",
        "--db-user",
        "jurisearch_read",
    ]);
    // A non-loopback bind is refused (4B is loopback/UDS-only) BEFORE the embedder probe.
    command
        .assert()
        .failure()
        .stdout(predicate::str::contains("loopback"))
        .stdout(predicate::str::contains("tokenizer").not());
}
