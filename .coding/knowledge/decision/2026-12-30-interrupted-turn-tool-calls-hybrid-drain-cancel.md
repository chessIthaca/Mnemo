+++
title = "interrupted-turn tool calls — hybrid drain/cancel (steers drain, hard stops cancel visibly, mid-execution grace-drain)"
created = "2026-12-30"
+++

DECISION (2026-12-30, plan edfff8d9, user request): how interrupted turns treat in-flight tool calls — HYBRID drain/cancel, split by signal type and execution phase.

1) Steers (Prompt/Suggestion) NEVER cancel a tool batch: a mid-stream steer DRAINS the batch (turn.rs post-stream block ends the turn pre-batch only for hard stops; a bare Steer falls through to tool-call execution, the steer staying queued in stop_reason and driving the follow-up turn exactly as a mid-batch steer does today). Rationale: removes the sub-second timing asymmetry that made execution nondeterministic (root cause A); matches the long-standing mid-batch steer behavior ("a Steer sets stop_reason but does NOT break — the batch completes"); the Stop button remains the hard cancel.

2) Hard stops (Interrupt/InterruptWithSteers/Cancel/Compact/CompactWithSteers/Clear) cancel every not-yet-run call with an explicit cancellation tool_result recorded in the CONVERSATION HISTORY (not UI-only): assistant message pushed WITH the accumulated tool_calls + one "interrupted: not run (turn stopped)" tool_result per call id, keeping the existing UI synthetic events. Truncated-args calls reuse the bad-JSON sanitize pattern (turn.rs:1515-1548: invalid args → "{}", raw: None). Rationale: acceptance criterion "the agent's context must reflect what actually ran"; also fixes raw-echo hazard D (dangling tool_calls → provider 400).

3) Mid-execution hard stops GRACE-DRAIN the in-flight call for DRAIN_GRACE_MS = 2000 (dispatch.rs dispatch_with_interrupt): the pinned future is awaited bounded by the window — completion returns the REAL result (stop_signal still set, the turn stops after this call); timeout drops the future + returns the existing synthetic "interrupted during execution" result. Rationale: acceptance criterion "bookkeeping mutations (memory_*, backlog_*, complete_step, file_write) either complete or visibly fail" — fast bookkeeping calls (ms) complete instead of being dropped mid-write; slow calls (shell, browser, search, LLM-backed) still cancel promptly.

Invariant tested: after ANY interruption, every tool_call id the model emitted has exactly one tool_result (success, error, or recorded cancellation) in both the UI and messages.
