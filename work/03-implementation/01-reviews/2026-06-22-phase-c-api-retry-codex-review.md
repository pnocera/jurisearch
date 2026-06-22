Phase C API retry/backoff review

Findings

BLOCKER crates/jurisearch-official-api/src/lib.rs:516 - `retry_after_from_error` only reads `Retry-After` from HTTP 429 responses. The requested behavior says `Retry-After` wins for the retryable HTTP failures, and 503 is a normal status for that header. Today a `503 Retry-After: 30` response is retried after the exponential base delay instead of the upstream-directed delay, so the cap/honor behavior is incomplete and tests would not catch it because the only `Retry-After` retry test is 429. Fix by reading `Retry-After` from every retryable `ureq::Error::Status` before sleeping, for example matching `Status(code, response)` where `code == 429 || (500..=599).contains(code)`, and add a 503-with-Retry-After pure/helper or integration test that proves the header beats exponential backoff and is capped.

Reviewed notes

- Attempt counting is otherwise correct: `attempt` starts at 0, retries while `attempt < max_retries`, and increments only after sleeping, so total sends are exactly `max_retries + 1`.
- Retry selection is conservative and matches the stated policy apart from the 5xx `Retry-After` issue: 429 and 5xx are retried, non-retryable 4xx and transport errors return immediately.
- Exponential backoff is capped and uses `checked_shl` plus `checked_mul`, so the default policy has no overflow path. `RetryPolicy::immediate(n)` correctly disables sleeps, including `Retry-After`, by setting `max_delay` to zero.
- The request closures rebuild the `ureq` request builder on each attempt. The GET `KeyId`, Legifrance search bearer header, JSON body, OAuth form body, and client-secret form field are resent from stable borrowed values rather than reusing a consumed builder/body.
- Retrying the Legifrance search POST is acceptable for this client because it is a read-style search operation. Retrying OAuth `client_credentials` POST on 429/5xx is also acceptable: it can mint another token, but it has no application-side write effect and the cached token is only updated after a successful response parse.
- Not retrying transport errors is a conservative safety call. It may reduce resilience for dropped connections, but it avoids replaying POSTs when the client cannot know whether the upstream processed the request.
- I did not find retry-specific logging, so `KeyId`, `Authorization`, client id, and client secret are not newly exposed by the retry loop. Existing debug redaction for config/token remains intact.
- The `legifrance_bearer_token` borrow shape is sound: borrowed config credentials and agent use are confined to the retried send, and `self.legifrance_token` is mutated only after `send_with_retry` returns and the response is parsed.
- The two one-request error-mapping tests correctly opt out with `RetryPolicy::immediate(0)`. The other one-request tests either receive success, missing credentials, or a non-retryable 404, so they should not accidentally retry into a closed listener.
- The `Connection: close` stabilization in test responses is sound and test-only. The hand-rolled server accepts one request per connection and then stops writing; telling `ureq` not to pool those sockets makes the test match the server's capabilities instead of relying on keep-alive behavior. This does not mask a client-side retry bug.
- Default behavior does add possible wall-clock delay for real API users: with default exponential failures it is about 3.5 seconds of sleep across three retries, and with repeated capped `Retry-After` it can add up to 90 seconds plus request timeouts. That follows the chosen default policy; callers can opt out or tune via `with_retry_policy`.

VERDICT: FIXES_REQUIRED
