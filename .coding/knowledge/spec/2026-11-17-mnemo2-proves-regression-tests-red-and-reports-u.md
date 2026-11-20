+++
title = "Mnemo2 proves regression tests red and reports uncaught mutants by itself"
created = "2026-11-17"
evidence = "plan 970d8d66; tests in src-tauri/src/mnemo2/team/tests.rs (claim_refuses_a_busy_worker, turn_end_runs_both_owed_proofs, waiver_is_scoped_to_its_tip, empty_mutation_run_closes_previous_mutants) and proof.rs (dropped_proof_restores_the_checkout, timed_out_proof_kills_child_tree)"
+++

Code: src-tauri/src/mnemo2/team/proof.rs (engine) and proving.rs (team wiring); config in `.coding/checks.toml` `[regression]` and `[mutation]` tables.

Red check:
- When a bug_fixing worker's turn ends Completed and its diff adds the plan's regression_test, Mnemo2 claims the worker and reverse-applies only the change's source hunks in the worker's own checkout. Test files, inline #[cfg(test)] hunks and binaries are excluded from that patch.
- It then runs the test. Red gives the check 'regression test fails without the fix'. NotRed opens high finding regression-not-red; Inconclusive opens low regression-proof-inconclusive.
- It always restores the checkout: a drop guard covers cancellation, a kill-on-close process tree covers timeouts, and a marker in .coding/mnemo2/proving plus a startup repair covers a hard kill.

Mutation (high-risk workers, or team_mutation_check):
- Runs cargo-mutants --in-diff; uncaught changes become low mutant-<fnv hash> findings, capped at 20 plus mutant-more.
- Gone mutants close as fixed; a wontfix is never reopened.

Safety and the gate:
- The claim takes merge_lock with an idle/busy check. Every turn start, merge, delete, cleanup, compact and model change checks `proving` after merge_lock.
- Proof findings carry `tip <sha>` on their first detail line; a new tip re-proves.
- The merge gate requires the red check at the tip, or a wontfix proof finding AT THAT TIP. A 'fixed' resolution never waives it.

Lead tools: team_prove_regression, team_mutation_check.
