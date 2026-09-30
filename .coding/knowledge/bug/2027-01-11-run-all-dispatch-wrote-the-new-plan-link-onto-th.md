+++
title = "run-all dispatch wrote the new plan link onto the previous backlog item"
created = "2027-01-11"
+++

Observed 2027-01-11 (run-all batch, repo C:\Mnemo): after the budget item's plan a1047bff reached Complete and the batch dispatched the NEXT backlog item 627ee5d9 (plan 07ff4ea6), the plan link landed on the WRONG record in .coding/backlog.jsonl — item a25a5323 (budget, previously dispatched, plan a1047bff) gained plan_id "07ff4ea6" + plan_title "Close the two holes in the CI workflow guards (backlog 627ee5d9)", while item 627ee5d9 stayed status "pending" with note null. Evidence: `git diff HEAD -- .coding/backlog.jsonl` on wt/mnemo (committed state had a25a5323 pending; working tree had it in_flight with 07ff4ea6).

Impact: the batch's dispatch/completion bookkeeping can (a) re-dispatch an already-finished item or (b) stall because a completed item reads in_flight. Both wrong-item states were corrected with backlog_status (a25a5323 -> done, 627ee5d9 -> in_flight) — the plan_id/plan_title link cannot be cleared with backlog_status (only status/deferred/note), so a25a5323 keeps the stale plan_id.

Fix direction (not yet done): the run-all dispatcher's plan->item linking should match the item it just dispatched (by id/title), not overwrite the previous item's record; and a completion should resolve the item whose plan reached Complete. Repro: run back-to-back run-all items and diff backlog.jsonl after the second create_plan.

Amended 2027-01-11: Second observation, same session: after the CI-guards item 627ee5d9 closed and the next item 6e071b07 was planned (plan f0758c0d), the link landed on a25a5323 AGAIN — its plan_id/plan_title were overwritten from 07ff4ea6 to f0758c0d, while 6e071b07 stayed status pending with no link. So the mis-target is stable, not one-off: the dispatcher overwrites the SAME record (a25a5323) with every new plan link instead of the item actually being worked. Workaround applied each time via backlog_status: a25a5323 -> done, 6e071b07 -> in_flight; the stray plan_id cannot be cleared by backlog_status (it has no plan-id setter). Any dispatcher fix should also stop trusting the stale plan link on a25a5323.
