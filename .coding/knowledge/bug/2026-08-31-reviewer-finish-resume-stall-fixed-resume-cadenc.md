+++
title = "reviewer-finish resume stall — FIXED (resume-cadence clause shipped)"
supersedes = "2026-08-31-reviewer-finish-resume-stall-continuation-gap-no"
created = "2026-08-31"
+++

FIXED (commit 5f4c08c, 2026-12-04). The third continuation gap — main agent stalling when the reviewer-finish notification resumed it — is now covered by a resume-cadence clause appended to the reviewer-spawn bullet in APP_RULES (src/agent/prompt.rs): "When it resumes you, immediately continue the closing sequence — read the report, fix findings, re-run tests, commit, finish — don't stall or wait for the user." Co-located test assertion added. Review: PASS.

Original observation: when the reviewer finished, the main agent produced (no output) and waited for the user to say "ok" before reading the report. The chaining bullet (mitigation ①, commit 8ad3d79) framed continuations around tool results, not resume notifications, so it didn't cover this trigger. This clause closes that gap — same stable-head pattern as the retry-cadence clause (mitigation ④).

All three shipped mitigations now in main-pending branch wt/agenticcoding:
- ① chain-instruction (commit 8ad3d79) — tool-result continuations
- ④ retry-cadence (commit 8ad3d79) — error-retry cadence
- resume-cadence (commit 5f4c08c) — reviewer-resume cadence
Deferred: ② per-session throughput signal.
