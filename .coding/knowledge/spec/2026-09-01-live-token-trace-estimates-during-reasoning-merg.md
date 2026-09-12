+++
title = "live token/trace estimates during reasoning — MERGED into main"
supersedes = "2026-09-01-live-token-trace-estimates-during-reasoning-char"
created = "2026-09-01"
+++

SPEC: live token/trace estimates during reasoning (chars/4, stats-safe) — MERGED into main at 4173dda (2026-09-19), branch wt/agenticcoder deleted. Gist unchanged: liveCompletionTokens/liveReasoningTokens per-bucket FE estimates; trace update_streaming_usage overlay (500ms throttle, terminal-record guard, final_usage_seen); estimates never reach RequestStats — stats record only on the final LlmEvent::Usage.
