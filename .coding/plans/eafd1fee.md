# Plan: Git-heavy hub→node calls time out after 30 s under load (repo.push_end, checkout.prepare, repo.fetch_branch)

## Goal
Hub→node calls that do real git work wait up to the existing LONG_TIMEOUT (180 s) instead of CALL_TIMEOUT (30 s). Quick control calls keep 30 s.

## Kind
bug_fixing

## Context
ROOT CAUSE (lead-verified): src-tauri/src/mnemo2/team/remote.rs passes `CALL_TIMEOUT` (30 s, src/remote/hub.rs:79) to every node call. That includes three calls that run git on the node: REPO_PUSH_END (remote.rs ~:693, the node verifies the uploaded bundle and git-fetches its refs into the bare repo, src/remote/repo.rs:483), CHECKOUT_PREPARE (~:731, creates or resets a checkout of the project tree) and REPO_FETCH_BRANCH (~:972 via call_stream, the node writes a bundle of the worker branch). hub.rs already has `LONG_TIMEOUT` = 180 s (hub.rs:82), used for SESSION_SPAWN (hub.rs:558) and one more call (:634). Under load, or for a large repo, these git operations exceed 30 s; the call fails although the node finishes the work.

FIX: in team/remote.rs add `const GIT_CALL_TIMEOUT: Duration = mnemo::remote::hub::LONG_TIMEOUT;` with a doc comment ("node calls that run git on the project — fetch a bundle, create a checkout, write a bundle — can take minutes on a large repository or a busy node"). Use it for the REPO_PUSH_END call_host, the CHECKOUT_PREPARE call_host and the REPO_FETCH_BRANCH call_stream. Leave REPO_HAVE, REPO_PUSH_BEGIN, REPO_PUSH_CHUNK (one chunk upload each), REPO_TIP, CLAUDE_*, NODE_STATUS and the other control calls on CALL_TIMEOUT. To make this testable, add `pub(crate) fn node_call_timeout(method: &str) -> Duration` in team/remote.rs: GIT_CALL_TIMEOUT for those three method constants, CALL_TIMEOUT otherwise. Route the call sites in this file that use CALL_TIMEOUT through it (`node_call_timeout(mnemo::remote::repo::REPO_PUSH_END)` and so on). Do not change src/remote/hub.rs or the node side. Check src-tauri/src/mnemo2/team/check_run.rs and proving.rs for other git-heavy node calls on CALL_TIMEOUT (grep CALL_TIMEOUT under src-tauri/src/mnemo2) and route them through node_call_timeout too if they run git (checkout reset, bundle). Report what you found.

REGRESSION TEST (team/remote.rs tests module): `git_node_calls_use_the_long_timeout` asserts node_call_timeout returns LONG_TIMEOUT for REPO_PUSH_END, CHECKOUT_PREPARE and REPO_FETCH_BRANCH, and CALL_TIMEOUT for REPO_HAVE, REPO_PUSH_CHUNK and NODE_STATUS (or whatever control constants exist). It does not compile before the fix (no node_call_timeout), so it fails before the fix. Record it with plan_update regression_test git_node_calls_use_the_long_timeout.

TRAPS: team code compiles only with --features mnemo2; src/remote only with the remote feature. No #[allow], no cfg(windows). VERIFY: `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team::remote` plus the three tests named above, and `cargo clippy --workspace --all-targets`. Do not run `cargo test --workspace`.

## Acceptance scenarios
- node_call_timeout(REPO_PUSH_END / CHECKOUT_PREPARE / REPO_FETCH_BRANCH) is 180 s and quick control calls stay at 30 s (git_node_calls_use_the_long_timeout).
- A loaded check run no longer fails the remote team tests with 'no answer to "repo.push_end" within 30s'.

## Steps
- [x] 1. **Give git-heavy node calls the long timeout** — files: src-tauri/src/mnemo2/team/remote.rs (GIT_CALL_TIMEOUT, node_call_timeout, the call sites, the regression test); src-tauri/src/mnemo2/team/check_run.rs and src-tauri/src/mnemo2/team/proving.rs only if they make git-heavy node calls on CALL_TIMEOUT. Implement exactly the FIX and REGRESSION TEST in the plan context. DONE: the regression test, the team::remote tests, agent_retry_after_busy_slots_stays_an_agent, dispatch_agent_fallback_respects_the_waiting_worker, remote_agent_continue_does_not_finish_on_the_old_turn_end and clippy pass; commit once and report the hash and every call site you changed or left at 30 s with its reason.

## Bug
On a busy machine the remote team tests fail with 'the worker host: timeout: no answer to "repo.push_end" within 30s' (also checkout.prepare). Three tests failed in hostBadge01's check run on 2026-11-26 (agent_retry_after_busy_slots_stays_an_agent, dispatch_agent_fallback_respects_the_waiting_worker, remote_agent_continue_does_not_finish_on_the_old_turn_end) on a frontend-only change. The same limit applies to real worker hosts: git work on a big repository or a node busy with builds can take over 30 s, and the worker then fails to start or sync.

## Regression test
git_node_calls_use_the_long_timeout

## Decisions
- 2026-11-26: no spec impact: Timeout tuning for existing node calls; no specified behavior changes.

## Progress
- 2026-11-26 step 1 done: step 1 merged: nodeTimeout01 (nodeTimeout01) on deepseek-v4.1-flash:1017, merge commit 948acccc; review: Git-heavy node calls (push_end, checkout.prepare, fetch_branch) now wait LONG_TIMEOUT through node_call_timeout, and control calls keep 30 s. There is a regression test.
