+++
title = "Steering the main agent during Run-All halts the run (not interleaves)"
created = "2026-08-31"
status = "superseded"
+++

SUPERSEDED 2026-12-06 by the plan-tied contract (backlog 45dcf577, .coding/knowledge/spec/2026-12-06-backlog-status-plan-lifecycle.md): the HALT mechanic stands — steering the main agent during Run-All halts the run (send_suggestion → halt_run_all BEFORE sending; never interleaves) — but both item dispositions changed: the steer path requeues the item via the intervention latch (plan 0a58db61: InFlight→Pending, sha preserved), and the approval halt stamps NOTHING (annotate + keep the run state so the post-approval turn resolution resolves the item under the plan-tied rules). The original spec's "in-flight item is marked Failed" claims are historical (pre-0a58db61/45dcf577).
