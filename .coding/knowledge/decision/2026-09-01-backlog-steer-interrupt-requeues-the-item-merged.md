+++
title = "backlog steer/interrupt requeues the item — MERGED into main"
supersedes = "2026-09-01-backlog-steer-interrupt-requeues-the-item-inflig"
created = "2026-09-01"
status = "superseded"
+++

DECISION: backlog intervention semantics — steer/interrupt is never a failure; MERGED into main at ff7a633e (2026-12-05). (1) steer/interrupt on the main agent mid-item records a user_intervention latch; resolution requeues the item non-terminally (Pending stays, InFlight→Pending, checkpoint sha preserved), never dispatches next / auto-feeds, ends only the identity-matched run. (2) Items leave Pending ONLY when the workflow enters Executing. (3) A steer absorbed into a closed loop still commits + marks Done. (4) AMENDED 2026-12-06 (backlog 45dcf577): approval halts stamp NOTHING — annotate + keep the run state so the post-approval turn resolution resolves the item under the plan-tied rules (the original "keep stamping Failed (terminal, load-bearing)" claim was reversed; halt_run_all still takes stamp_failed, now selecting the annotate+keep arm vs the steer arm's latch+end_run). Current contract: .coding/knowledge/spec/2026-12-06-backlog-status-plan-lifecycle.md.
