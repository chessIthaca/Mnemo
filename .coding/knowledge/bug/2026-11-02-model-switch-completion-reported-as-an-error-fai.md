+++
title = "model-switch completion reported as an Error failed healthy workers — pump semantics + the compact-only-when-too-big rule"
created = "2026-11-02"
+++

Root cause of "a model switch marked a working Mnemo2 worker failed" (verified in src-tauri/src/mnemo2/team.rs worker pump + src/agent/turn.rs handle_pending_swap):

- `CompactStarted` sets `worker.compacting = true, busy = true, status = Working`. The deferred compact-then-switch path emits CompactStarted but NO `Compacted` (the only `AgentEvent::Compacted` emission is the regular compaction path in turn.rs), so the swap's paired end was the `Error { retrying: false }` it sent on completion.
- The pump's Error arm branches: `if worker.compacting` → clear badge, busy=false, status Idle/NeedsYou, `error = Some(msg)`; else → `status = Failed`. So a deferral WITHOUT summarization (compacting never set) marked the worker Failed, and one WITH summarization left it Idle with an error recorded — both for a switch that had actually succeeded.
- Correct shape: a completed deferred swap is informational (`AgentEvent::ModelSwapSummarized { model }`, serialized `model_swap_summarized`, mirroring `model_changed`), emitted only when the pre-swap summary actually rewrote the conversation. Informational consumers (Mnemo1 reducer, Mnemo2 worker pump + lead pump, the terminal console) must never map it to failure/`failed`/non-busy; the worker pump may only end the compacting badge.

Deferral rule (one rule, three call sites): compact before switching ONLY when the live conversation exceeds the NEW model's own compaction trigger — `live_token_count > 0 && live_token_count >= new_context_manager.effective_limit()` (messages + tools overhead, exactly what the turn loop's summarize trigger compares). A merely smaller target window is NOT a reason to summarize a conversation that fits. The same rule decides `AgentSession::set_model`'s CompactsFirst vs NextRequest, and `handle_pending_swap`'s summary gate is `pending.context_manager.should_summarize(messages)`.

Test-harness gotchas (Mnemo2 end-to-end swap tests): the worker BRIEF is not part of `messages` for token counting (a 120K-char brief left `count_tokens(messages)` tiny), so the pin can land WITHOUT deferral; a turn ends after 3 consecutive tool ERRORS (MAX_RETRIES), so grow a conversation with SUCCESSFUL calls (e.g. the `message_parent` host tool); and the new model's own no-op compaction pass emits CompactStarted+Compacted, whose pump arm sets busy=false/status Idle — so assert "not Failed, error None" rather than "status == Working".
