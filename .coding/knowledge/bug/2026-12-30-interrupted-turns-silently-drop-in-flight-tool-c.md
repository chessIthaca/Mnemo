+++
title = "interrupted turns silently drop in-flight tool calls — root causes A-D confirmed (audit 2026-12-30)"
created = "2026-12-30"
status = "superseded"
+++

BUG: user-interrupted turns silently drop in-flight tool calls (2026-12-30, user-reported "we ran into this bug again … remember pls"; evidence file .coding/knowledge/bug/2026-12-30-interrupted-turns-silently-drop-in-flight-tool-c.md, commit f556a3d). Symptom: a user message arriving while a turn's tool calls are in flight silently drops some/all calls — no result block, no error; execution nondeterministic (instance 2: shell ran, sibling backlog_add didn't persist).

AUDIT (plan edfff8d9, 2026-12-30) — root causes confirmed:
A) PRIMARY/nondeterminism: src/agent/turn.rs post-stream block (~:1336) ends the turn for ANY stop_reason incl. bare Steer BEFORE the batch executes. A mid-stream user message (Prompt→Steer via StopReason::fold, loop_impl.rs:409) silently cancels the complete batch; the same message arriving after stream-end drains it (between-call safe point ~:1680 breaks only for hard stops). Sub-second timing decides whether calls run.
B) Context invisibility: the same block gives orphaned calls UI-ONLY synthetic "not run" events (backlog 63cbc20f) and pushes the assistant message with tool_calls: vec![] — the model's context never records the cancelled calls, so it cannot re-issue them.
C) Mid-execution drops: src/agent/dispatch.rs dispatch_with_interrupt (~:481-530) DROPS the tool future on Interrupt/Cancel — bookkeeping mutations (backlog_add, complete_step, file_write) end half-applied/unapplied; spawn_blocking work completes untracked.
D) Latent raw-echo hazard (= instance-4 echo mechanism, model-side): the partial assistant message keeps raw: acc.take_raw(). A COMPLETED batch's raw (valid tool_calls) is echoed (raw_is_usable openai.rs:2276) with NO following tool results → provider 400/model confusion on the follow-up request (validate_request_messages openai.rs:2403 checks tool-result→tool-call only, not the reverse). A CUT batch's raw (invalid args) falls through to field construction → calls vanish from the request entirely. NOT a frontend bug: flushStreamingText (agentState.ts:441) always creates a separate assistant entry, pushTranscriptEntry (:458) flushes before appending, reduceFinished sweeps running cards "(interrupted)" (agentEventReducer.ts:1233).

Verified non-implicated: TurnResolveLatch (src/runtime/turn_resolve.rs) resolves backlog items, not tool results. Steer injection (src/runtime/agent.rs:185-208) pushes steers as user messages + follow-up turn, no extra LLM round-trip — a draining mid-stream steer lands cleanly. finalize() keeps truncated-args calls; the normal path's bad-JSON sanitize pattern (turn.rs:1515-1548: invalid args → "{}", raw: None, assistant message with tool_calls + per-call tool_results) is the exact precedent the hard-stop fix reuses.

NOTE: .coding/knowledge/ is protected from the file tools — this memory record is the audit addendum of record (the planned knowledge-file append was refused; the evidence file keeps the original symptom instances). Fix design: see DECISION memory "interrupted-turn tool calls — hybrid drain/cancel".
