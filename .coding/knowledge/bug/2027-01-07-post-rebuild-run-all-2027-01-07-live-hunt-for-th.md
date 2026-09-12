+++
title = "post-rebuild Run-All (2027-01-07) — live hunt for the park recurrence"
created = "2027-01-07"
+++

The Run-All the user started on 2027-01-07, immediately after recompiling + restarting the app, is a BUG HUNT, not normal backlog work — treat any stop/park/halt it hits as evidence, not routine.

Bug hunted (recurred last session in two stops, despite the fix being in main): Stop 1 = reviewer-finish park — the closing sequence's resume Suggestion arrived on an exhausted auto-continue budget, the None-arm gate failed 12 < 12, and the turn parked until a manual 'c'. Fixed in theory by 6aeb085 (merged a4a0d18, 'Suggestion resets auto-continue budget'; also added AgentEvent::Parked → InputBar banner). Stop 2 = the amplifier — during the park, the run-all gate saw workflow=Reviewing with no descendants running, stamped the item 'plan loop did not close', left it in_flight, and halted the run; when the plan later finished, nothing dispatched → idle until a second 'c' (escape hatch stamped done). Fingerprint: that disposition note (last seen on backlog item 207dc316).

Why it recurred: leading explanation is a stale binary — the fix is Rust-backend (src/runtime/agent.rs), live only after rebuild + restart, which the user has NOW done. This run-all is the verification run.

Read on the current build: Parked banner in the InputBar on budget exhaustion = fix live and working (visible, resumable with 'c') — not a bug. Silent hang again = NEW uncovered site; leading suspects: (1) the Suggestion-arm streak reset missing the reviewer-finish resume path specifically, (2) the run-all gate halting permanently on a parked Reviewing turn instead of re-checking at the next plan finish. If a silent park recurs → bug plan against the reviewer-finish Suggestion path. Gate hardening (re-check at next finish instead of halting) was offered as a backlog item — not yet queued. Queue at run start: 8 pending items, head 13058579 (parallel run-all).
