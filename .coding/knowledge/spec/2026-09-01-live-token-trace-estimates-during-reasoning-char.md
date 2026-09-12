+++
title = "live token/trace estimates during reasoning (chars/4, stats-safe)"
created = "2026-09-01"
status = "superseded"
+++

SPEC: live token/trace updates during reasoning — (1) Toolbar: the inflight bar's chars/4 streaming estimate is split per bucket — liveCompletionTokens (TextDelta) / liveReasoningTokens (ReasoningDelta) in agentEventReducer — so both counters (↓ completion, 🧠 reasoning) climb during a stream and snap to authoritative counts on the usage event. (2) Trace: provider stream loops (openai.rs + anthropic.rs) push throttled (>=500ms) chars/4 estimates onto the in-flight record via LlmRequestLog::update_streaming_usage (no-ops on terminal records, preserves prompt/cached, final_usage_seen flag stops the overlay once authoritative usage landed — protects against non-conforming servers sending usage mid-stream); the final set_usage overwrites. LlmRequestSummary carries is_complete (list rows show "—" for prompt while streaming); UsageCard shows an "est. while streaming" badge. Invariant: estimates NEVER touch RequestStats/memory.db — stats record only on the final LlmEvent::Usage. Shipped 45637a0 + cfea2a3 on wt/agenticcoder (not yet merged to main); reviews .coding/reviews/275d5568-live-token-trace-updates-review{,-round2}.md (r2 PASS).
