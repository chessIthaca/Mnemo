# Plan: Team tests flake under load: 20 s eventually budget and a 10 s age window

## Goal
The team tests' positive waits and age assertions hold on a loaded machine without weakening what they prove.

## Kind
bug_fixing

## Context
Same class as the closed plans 707ad97e and f96d40f3 (HOW 'wait on the event, not on the clock'). FIX (test-only, src-tauri/src/mnemo2/team/tests.rs): (1) `eventually` (:5931): replace the 800-iteration loop with an Instant deadline of 120 s, polling every 25 ms, with the same panic text plus ' within 120 s'. All 67 callers stay unchanged. Check that no caller uses eventually for a NEGATIVE wait (expecting it to give up); if one does, report it and leave it on its own short loop. (2) team_status_shows_host (:20694): the lower bound 30 is what proves the age is computed from offline_since_ms; the upper bound is only slack. Change it to `assert!(offline >= 30 && offline < 30 + 120, "{entry}")`. Do not change other assertions or production code. Also grep team/tests.rs for other `(N..M).contains(` age or elapsed windows narrower than 60 s and widen their upper bound the same way, listing each. REGRESSION TEST: team_status_shows_host (record with plan_update regression_test). VERIFY: `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team::tests` and clippy; not cargo test --workspace.

## Acceptance scenarios
- eventually waits up to 120 s by deadline; team_status_shows_host accepts any age from 30 s up to 150 s.
- mnemo2::team::tests passes in full on a loaded machine.

## Steps
- [x] 1. **Give team tests' eventually a 120 s deadline and widen the host-away age window** — files: src-tauri/src/mnemo2/team/tests.rs only. Implement exactly the FIX in the plan context. DONE: the team test module passes, team_status_shows_host and the two mutation-notice tests pass, and clippy is clean; commit once and report the hash and every window widened.

## Bug
In a loaded check run (2026-11-26, hostScenes01 at 398090f7, a frontend-only change) three team tests failed: failed_mutation_run_with_clean_checkout_is_a_notice and missing_mutation_tool_is_a_notice_not_a_worker_error with 'the polled condition never became true' (the eventually helper, team/tests.rs:5931-5942, gives up after 800 × 25 ms = 20 s), and team_status_shows_host with host_offline_s = 40 failing `assert!((30..40).contains(&offline))` (team/tests.rs:20694), because more than 10 s passed between setting offline_since_ms = now − 30 s and the status call.

## Regression test
team_status_shows_host

## Decisions
- 2026-11-26: no spec impact: Test-only timing changes.

## Progress
- 2026-11-26 step 1 done: step 1 merged: teamWaits01 (teamWaits01) on deepseek-v4.1-flash:1017, merge commit 44f0d053; review: Combined tip 7dc42155: the team-test load fix (80246f61: eventually gets a 120 s deadline, team_status_shows_host accepts 30-150 s) plus phase 5 steps 8 (preview scenes), 9 (CI asserts mnemo2-node) and 10 (copy fixes), each reviewed on its own branch.
