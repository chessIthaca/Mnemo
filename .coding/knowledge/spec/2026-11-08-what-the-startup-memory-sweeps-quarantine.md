+++
title = "what the startup memory sweeps quarantine"
created = "2026-11-08"
evidence = "src/memory/integrity.rs tests; first real run quarantined 477 git-state facts and 141 duplicate digests; a hand sample of 30 git-state candidates found 0 wrong calls"
+++

src/host/brain.rs runs these before the reconcile at each open, all from src/memory/integrity.rs, all quarantine rather than delete (recoverable with include_quarantined), and idempotent. `quarantine_git_state_facts`: untyped, authored, semantic facts never returned by an explicit search and with no how_known, whose text matches `restates_repo_state` (a commit, branch, plan id or step progress restated); the plan-id shape is broad, which is why the first run was sampled by hand. `quarantine_old_synthetic`: `Session involved N tool calls` rows older than 30 days. `quarantine_duplicate_digests`: an authored PLAN or REVIEW digest beside a file-derived twin with the same normalized title, or the older of two authored twins; derived rows always stay. Synthetic rows are also excluded from ranked recall at any age. To audit a matcher change, examples/memory_git_state_sweep.rs runs the git-state sweep on a store copy and writes its candidates. Clearing rows can raise noise@5 slightly, because freed slots go to untyped facts.
