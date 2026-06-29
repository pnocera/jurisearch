# Claude review prompt - 10-next-plans macro implementation plan

Please review the current implementation planning artifacts in:

- `/home/pierre/Work/jurisearch/work/10-next-plans/00-macro-implementation-plan.md`
- `/home/pierre/Work/jurisearch/work/10-next-plans/01-makeitsimpletodeploy.md`
- `/home/pierre/Work/jurisearch/work/10-next-plans/02-auto-update-server-crons.md`

## User intent

The user asked for a macro implementation plan covering the site/client deployment plan and the
producer/update-server automation plan. Since then, the user resolved every previously open planning
decision:

- Producer orchestration is library-first in v1, not shell-out.
- DILA LEGI/CASS/CAPP/INCA/JADE stay in the single `core` corpus for v1.
- Future restricted add-ons such as INPI are separate subscription corpora.
- Add-on downloads should be subscription-aware before artifact download, while apply-time entitlement
  remains the hard security gate.
- Baseline refresh is automatic via `auto-on-new-baseline`.
- Judilibre freshness accelerator is deferred from v1.
- Storebox retains every accepted official archive for v1.
- First release artifacts are Linux `x86_64-unknown-linux-gnu` `.tar.zst` role bundles; Debian
  packages are deferred.
- Producer install/admin commands stay under `jurisearch-producer`; `jurisearchctl` remains
  site/customer deployment focused.
- CI/demo smoke uses a tiny signed fixture with a documented stable fixture id; operated bear
  acceptance uses a real DILA id after real producer packages exist.

## Non-negotiable constraints to check

- `./dist.sh` must write only to repository-local `./dist/`, not filesystem `/dist`.
- Release artifacts must be distinct for `update-server`, `site-server`, and `cli`.
- Release artifacts must exclude huge/runtime assets: databases, vector indexes, downloaded legal
  archives, runtime corpus packages/manifests, model weights, tokenizer files, and credentials.
- Site/customer query embeddings must be local-only for confidentiality.
- Producer document embeddings may use OpenRouter/OpenAI-compatible bge-m3 because the inputs are
  public official legal texts.
- Update-server CT is lightweight and targets the external PostgreSQL CT for DB-heavy work.
- Storebox is the archive/package/manifest storage location for update-server runtime outputs.
- Automatic rebaseline must be explicit/recorded and must not silently mutate cursors or apply deltas
  across a baseline boundary.
- Subscription add-ons must not be represented as hidden URLs only; entitlement remains signed
  package apply logic.

## Validation already run

From `/home/pierre/Work/jurisearch`:

```sh
git diff --check -- work/10-next-plans/00-macro-implementation-plan.md \
  work/10-next-plans/01-makeitsimpletodeploy.md \
  work/10-next-plans/02-auto-update-server-crons.md
```

This passed.

## Review request

Please review the three artifacts for planning quality and implementation readiness. Focus on:

- contradictions between the macro plan and the detailed plans;
- unresolved questions that still materially block implementation;
- missing dependency ordering or milestones;
- hidden ambiguity around producer/site boundaries, subscriptions, release assets, rebaseline, or
  embedding confidentiality;
- acceptance gates that are too vague to implement or verify;
- places where the resolved user decisions were only partially propagated.

Do not edit files. Write findings first, ordered by severity. For each finding, include:

- severity: BLOCKER, WARN, or NIT;
- concrete file/line reference;
- why it matters;
- a recommended fix.

Then include open questions/risks, verification notes, and finish with exactly one final verdict line:

`VERDICT: GO`

or

`VERDICT: FIXES_REQUIRED`
