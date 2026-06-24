# Codex Review R2: Zone Ops Legislation Enrich Loop

## Findings

### WARN: Retry pass can still start before pending is proven drained

`run_pass pending` can stop for three different non-drained outcomes, but the caller treats all of them as success before starting `run_pass retry --retry-errors`:

- CLI failure returns non-zero at `work/03-implementation/04-zones/ops/03-legislation-enrich-loop.sh:40-41`, but line 63 does not check that status; without `set -e`, the script continues into retry and then exits successfully after the final `echo`.
- An all-error pending batch breaks at lines 53-55. That is explicitly the auth/quota stop case, and pending rows sorted after the selected prefix can still exist.
- Hitting `MAX_BATCHES` exits the loop at line 34/57 without any drained signal, so pending may still remain.

In the all-error or max-batch cases, line 65 then starts the combined retry selection (`pending`, `upstream_error`, `parse_error`) before pending is drained. That violates the r2 invariant documented at lines 7-14 ("only when none remain does pass 2 retry") and can recreate the original mixed pending+error prefix hazard: the newly-created early `upstream_error` rows can dominate the retry batch, trip `err==considered`, and leave later pending rows stranded.

Concrete fix: make `run_pass` report why it stopped, and only run the retry pass when the pending pass stopped because `considered == 0`. Also propagate real CLI failures to the script exit status. For example, have `run_pass pending` return `0` only for drained, return non-zero for CLI failure/all-error/max-batches, and gate the caller as:

```bash
run_pass pending || exit $?
if [ "$RETRY_ERRORS" = "1" ]; then
  run_pass retry --retry-errors || exit $?
fi
```

If the retry pass should treat "only persistent errors remain" as an acceptable stop, give `run_pass` an explicit mode or global stop reason so `pending:all_error` is fatal/skip-retry while `retry:all_error` is acceptable.

## Requested Confirmations

- Pending-first ordering is the right design and removes the stranding hazard only if pass 2 is actually gated on pass 1 draining the pending queue. The current script does not enforce that gate.
- The retry pass is bounded by `MAX_BATCHES`, and all persistent errors can terminate via `err==considered`; that part is structurally finite.
- `local extra=( "$@" )` and `"${extra[@]}"` are safe under Bash 5.3 with `set -u`; an empty array expands to no command arguments in this command position.
- I did not find another clone-safety issue in the reviewed script path.

VERDICT: FIXES_REQUIRED
