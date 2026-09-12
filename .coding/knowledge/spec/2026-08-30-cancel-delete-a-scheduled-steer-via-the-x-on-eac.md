+++
title = "cancel/delete a scheduled steer via the \"x\" on each pending steer"
created = "2026-08-30"
+++

SPEC: A pending steer (mid-work guidance queued while the agent runs) can be cancelled via an "x" button on its backlog bubble in the InputBar (plan 7da676df, commit 5d9e253 on wt/agenticcoding). The cancel is FULL-STACK: the frontend removes the bubble (removeSteer store action) AND sends a backend `cancel_suggestion` Tauri command → `AgentCommand::CancelSuggestion(text)` that drops the queued steer before injection.

Why full-stack: steers are sent to the backend immediately (sendSuggestion → AgentCommand::Suggestion(SteerPayload { text, images }) — payloads since 2027-01-07, spec 2027-01-07-steered-images-ride-the-steer-payload-end-to-e), buffered mid-turn, folded into StopReason::Steer(Vec<SteerPayload>) at the next safe point, then injected as user messages. A frontend-only removal would hide the bubble but the backend would still inject the steer.

The clean lever: StopReason::fold (src/agent/loop_impl.rs) is the single accumulation rule, called from the streaming select!, the between-tool-call drain, and the approval re-injection. The CancelSuggestion arm removes matching text from any Steer/InterruptWithSteers/CompactWithSteers list and collapses empty lists (Steer→None, InterruptWithSteers→Interrupt, CompactWithSteers→Compact). drop_cancelled_steers() helper filters the summarization re-injection buffers (order-independent, HashSet-based). A pre-inject drain (try_recv + fold) in run_turn_with_retry closes the window between turn-end and injection. Matching is by TEXT (the backend queue is text-based, not id-based) — two pending steers with identical text are both cancelled, even if their images differ (documented limitation, unchanged by the payload widening).

Files: src/runtime/channels.rs (variant), src/agent/loop_impl.rs (fold arm + helper + unit tests), src/agent/turn.rs (provider-request arm + 3 summarization filters + widened approval fold arm), src/runtime/agent.rs (pre-inject drain + between-turns no-op + integration test), src-tauri/src/ipc/agent.rs (command) + main.rs (registration), frontend InputBar.tsx (X button) + useAgentStore.ts (removeSteer) + lib/tauri.ts (cancelSuggestion).
