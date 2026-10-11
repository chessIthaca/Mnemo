+++
title = "headless-free-agent job 1 anchors — f82ecfe unresolvable in repo, but text-fresh (probed 2026-10-12)"
created = "2027-01-11"
+++

The mnemo-kernel master spec (.coding/specs/headless-free-agent.md, rev 6 APPROVED 2026-10-11) drives nine sequential jobs (1 → 2a/2b/2c → 3a/3b/3c → 4 → 5) extracting the host/kernel/headless API from the app crate. Step files live as .coding/specs/job<N>-<slug>.md — NOT in a headless-free-agent/ subdir as the spec's §14.1 says (naming drift, known deviation). Job 1's cited anchor commit f82ecfe does NOT resolve in this repo, but every quoted anchor matches the current tree text-exactly (probed 2026-10-12: build_brain_inner src-tauri/src/main.rs:1166, spawn_agent_shared src-tauri/src/ipc/spawn.rs:234, RECONCILE_CHANNEL memory_maintenance.rs:48 — all equal the spec-cited positions). Rule for all nine jobs: match quoted text, not line numbers; stop-and-report on mismatch. claim_busy's app callers (main.rs:1549/1576) read only `e.message`, so the MaintenanceBusy { pub message: String } design keeps them compiling with no main.rs edit in step 1.1. Step 1.1 executed on branch wt/mnemo, plan a9ddfaee; specs committed f21edf0.
