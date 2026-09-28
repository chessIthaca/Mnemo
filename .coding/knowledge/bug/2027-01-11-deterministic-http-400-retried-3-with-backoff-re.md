+++
title = "deterministic HTTP 400 retried 3× with backoff — request ladder had no 4xx fail-fast gate"
created = "2027-01-11"
+++

Symptom (2027-01 session): a deterministic HTTP 400 burned the full 3-attempt request ladder — three identical rejections plus ~5s of backoff sleeps — before the error surfaced ("request attempt 1/3 failed ... retrying in 0.7s" ×3).

Root cause: `complete_with_retry` (src/agent/dispatch.rs, request level) retried every provider error that was not serialization-bug / non-retryable / rate-limited. There was no generic 4xx gate, so a payload the provider had already refused was re-sent verbatim.

Fix: `Error::is_deterministic_4xx` (src/error.rs) — scans the HTTP status back out of the provider message's known status-line shapes (`HTTP <status> …`, LiteLLM `Error code: <status>.`, `status: <status>` / `status code: <status>`; exactly-three-digits guard so body numbers can't be read as a status) and is true for 400..=499 except 408 (transient) and 429 (owned by is_rate_limited). The ladder returns immediately on it, placed with the 429 arm BEFORE failure triage. Deliberately NOT is_non_retryable: the turn-level ladder still owns endpoint switching. Commit c19e79b (wt/mnemo). Tests: agent::tests::complete_with_retry_does_not_retry_deterministic_4xx (red: 3 calls; green: 1), plus error.rs unit tests http_4xx_status_is_deterministic / transient_errors_are_not_deterministic_4xx / non_provider_errors_are_not_deterministic_4xx.

Related: plan 428a9f1e finding C.
