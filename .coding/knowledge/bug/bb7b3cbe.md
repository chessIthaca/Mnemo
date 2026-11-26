+++
title = "Team tests flake under load: 20 s eventually budget and a 10 s age window"
created = "2026-11-26"
+++

Symptom: In a loaded check run (2026-11-26, hostScenes01 at 398090f7, a frontend-only change) three team tests failed: failed_mutation_run_with_clean_checkout_is_a_notice and missing_mutation_tool_is_a_notice_not_a_worker_error with 'the polled condition never became true' (the eventually helper, team/tests.rs:5931-5942, gives up after 800 × 25 ms = 20 s), and team_status_shows_host with host_offline_s = 40 failing `assert!((30..40).contains(&offline))` (team/tests.rs:20694), because more than 10 s passed between setting offline_since_ms = now − 30 s and the status call. · regression test: team_status_shows_host

Full record for plan bb7b3cbe (see .coding/plans/bb7b3cbe.md for the plan file).

regression test: team_status_shows_host · path .coding/plans/bb7b3cbe.md · branch wt/mnemo @ 44f0d053 (unmerged — exists only on this branch)
