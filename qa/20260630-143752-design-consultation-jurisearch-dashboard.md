# Q&A — 20260630-143752 (PANE FALLBACK — may be truncated)

## Question

# Design consultation — `jurisearch-dashboard` (Phase 1, simple read-only update-server dashboard)

Repo: `/home/pierre/Work/jurisearch`. READ THE CODE; verify against real source and push back where I'm wrong.

## Goal / scope (locked by the operator)
A SIMPLE, read-only operational dashboard for the **producer** (update-server), hosted on **CT 111**, built by
`dist.sh` + installed by `deploy.sh`, **Tailscale-only / NO auth**, **producer-only**, **NO database in
Phase 1**, **no Grafana/Prometheus**. Full analysis: `work/11-dashboard/00-update-server-dashboard-analysis.md`.
It must show, from ON-BOX sources only:
- **Ingestion status** per fetch group (legislation, jurisprudence) — last/current run, outcome, exact
  `exit_class`, kind (incremental/rebaseline), start/end, freshness, next timer, lock held.
- **Packages produced** — from the served `corpora_dir` `core/manifest.json` + package dirs (NO DB).
- **Errors** — failed runs (class + message) from the durable `RunRecord`s.
- **Logs** — a small ring-buffer / recent time-window of the producer services' journald output (no redaction).
Config: **bind addr + port configurable** (default the tailnet addr; never 0.0.0.0). Title "Juridia — Update Server".

## Proposed design (critique it)
- A new small crate **`jurisearch-dashboard`** (a 6th produced binary) — OR a `jurisearch-producer dashboard`
  subcommand — that **reuses the producer's own types**: `build_status`/`ProducerStatus`
  (`crates/jurisearch-producer/src/status.rs`), `RunRecord` (`runrecord.rs`), the exit-class helpers
  (`exit.rs`), and the signed remote-manifest struct that parses `core/manifest.json` (in
  `jurisearch-package-build`/`jurisearch-syncd` — `RemoteManifest`).
- A **minimal synchronous HTTP server** (this codebase is sync — `ureq` client, sync `postgres`, no tokio):
  e.g. `tiny_http`, or hand-rolled over `std::net::TcpListener`. Server-rendered HTML, `<meta refresh>`, no JS
  framework, no async runtime.
- **Logs via `journalctl`** subprocess (`journalctl -u jurisearch-producer-<group>.service -n <N> --no-pager
  -o short-iso`) — run as `jurisearch` + the `systemd-journal` group so no root is needed.
- **Packages** by reading + parsing the served `core/manifest.json` (sequence chain, active baseline) and
  `ls`/`stat` of `corpora_dir/core/packages/*` for sizes.
- **Config**: a `[dashboard]` block in `producer.toml` (or a separate small toml / CLI flags).
- **Deploy**: `dist.sh` builds it into the update-server bundle (same `--version` stamping); `deploy.sh`
  installs the binary + a `jurisearch-dashboard.service` systemd unit (bind to the tailnet addr, run as
  jurisearch, Restart=always), enabled + verified (active, bound to tailnet) the same fail-closed way.

## Questions (answer numbered; verify against source)
1. **Crate vs subcommand?** Given the dashboard must reuse `build_status`/`RunRecord`/`exit`/`RemoteManifest`,
   is a separate `jurisearch-dashboard` crate (depending on `jurisearch-producer` + the manifest crate)
   cleaner, or a `jurisearch-producer dashboard` subcommand (direct reuse, one fewer crate/binary)? Any
   dependency-direction or layering concern (does `jurisearch-producer` already expose these as a public API,
   or would a subcommand be the lower-friction reuse)?
2. **HTTP server**: what's the right minimal sync server here without dragging in tokio/axum? Is `tiny_http`
   already in the dependency tree or a sane add, or should it be hand-rolled on `std::net`? Any existing
   server code in the repo to reuse (e.g. does the site-server `serve` use a server lib I should match)?
3. **Which exact struct/function parses `core/manifest.json`**, and is reading it on disk (vs verifying its
   signature) the right call for a read-only dashboard? Should the dashboard verify the manifest signature, or
   just display it? What's the authoritative way to enumerate the package chain + sizes from `corpora_dir`?
4. **`build_status` reuse**: does it take a `ProducerConfig` + read only on-disk state (no DB/network), and is
   calling it from a separate process safe/cheap to poll every few seconds? Any locking concern reading the
   `RunRecord`/`last.json` files while a run writes them (atomic rename?)?
5. **journald access**: is the `journalctl` subprocess approach sound + are there permission subtleties (the
   `systemd-journal` group; does running the service as `User=jurisearch` + `SupplementaryGroups=systemd-journal`
   suffice)? Or is the `sd-journal`/`systemd` crate worth it? How to implement the "ring buffer / time window"
   cheaply (`-n N` + `--since`)?
6. **Deploy integration**: how should the dashboard's systemd unit be delivered — a static unit written by
   `deploy.sh` (like the secrets/config), or rendered by a producer `install`-style step? How does `deploy.sh`
   verify it (active + bound to the tailnet addr)? Any concern binding to the tailscale interface at service
   start (ordering vs `tailscaled`)?
7. **Config shape**: `[dashboard]` in `producer.toml` vs a separate `dashboard.toml` vs CLI flags — which fits
   the existing config patterns best?
8. **SCOPE the minimal Phase-1 slice** vs what to defer: the smallest correct first version (which pages/data)
   that's worth shipping + Codex-reviewing, and what to push to a follow-up.
9. Anything I'm missing or getting wrong (security of no-auth tailnet bind, read-only guarantees, a simpler
   path I've overlooked).

End with a clear verdict ("GO with adjustments" + numbered adjustments) and the recommended minimal scope.

## Answer (pane tail)


