+++
title = "emission-artifact guard — per-op delta attribution + artifact_check opt-out (plan 72d28347)"
created = "2027-01-11"
+++

Landed in commits 0dbfa41 (feature) + d862d1c (round-1 LOW-1 fix) on wt/mnemo; reviews .coding/reviews/2026-09-27-72d28347-emission-guard-attribution-review.md (round 1: FINDINGS 0 high/1 low) and ...-round2-delta-review.md (PASS).

Behavior: both file_edit and multi_edit route through `apply_ops` (src/tool/agent/edit_ops.rs) — the guard was ALREADY shared; the asymmetry reported in backlog 08d2125d was the call's net delimiter delta plus an unactionable message. `apply_ops` now tracks a running `rust_brace_deficit` trail (.rs only) and rejects only when the COMBINED deficit grows, naming each offending op as `ops[i] (label) grows it by +k` with the call's before → after deficit and a remedy line (rebalance in the SAME call / split / artifact_check:false after verifying).

Message rules (pinned by tests): keep the substrings "emission artifact" and "unbalanced delimiters"; the named-op shape must NOT assert truncation — it ends "either a truncated payload or a deliberately unbalanced restructure"; the single-path shape keeps "the truncation artifact: the payload was cut off"; `usize::MAX` (unterminated raw string) is NAMED, never printed as a number.

Escape hatch: `artifact_check: Option<bool>` on FileEditArgs + MultiEditArgs (call-level for multi_edit; default = validate); both tools stay NeedsApproval. The schema property must stay DECLARED (strict providers validate args against the schema), so it cannot live only in the rejection message.

Budget: two measured tools-array ceiling raises in src/agent/factory.rs — Executing 37_300 → 37_900 (measured 37_600) and Reviewing 32_700 → 33_100 (measured 32_864) — re-measure before trimming anything in those arrays.
