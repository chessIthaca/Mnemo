+++
title = "Git-heavy hub→node calls time out after 30 s under load (repo.push_end, checkout.prepare, repo.fetch_branch)"
created = "2026-11-26"
+++

Symptom: On a busy machine the remote team tests fail with 'the worker host: timeout: no answer to "repo.push_end" within 30s' (also checkout.prepare). Three tests failed in hostBadge01's check run on 2026-11-26 (agent_retry_after_busy_slots_stays_an_agent, dispatch_agent_fallback_respects_the_waiting_worker, remote_agent_continue_does_not_finish_on_the_old_turn_end) on a frontend-only change. The same limit applies to real worker hosts: git work on a big repository or a node busy with builds can take over 30 s, and the worker then fails to start or sync. · regression test: git_node_calls_use_the_long_timeout

Full record for plan eafd1fee (see .coding/plans/eafd1fee.md for the plan file).

regression test: git_node_calls_use_the_long_timeout · path .coding/plans/eafd1fee.md · branch wt/mnemo @ 948acccc (unmerged — exists only on this branch)
