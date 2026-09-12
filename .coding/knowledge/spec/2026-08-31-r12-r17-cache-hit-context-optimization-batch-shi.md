+++
title = "R12-R17 cache-hit + context optimization batch (shipped)"
created = "2026-08-31"
+++

SPEC: R12-R17 cache-hit + context optimization batch — shipped on wt/agenticcoding (commit 9f3f0d7, 2026-12-04). Round-4 analysis (.coding/analysis/cache-hit-4-report.md) drove 5 changes:

- R12 (src/agent/turn.rs): removed the min(prev,curr) cache-hit heuristic — provider's reported cached_tokens is now trusted as-is; 0 means not cached. Removed the last_prompt_tokens field (dead code under deny(warnings)).
- R13 (src/config/general.rs): summarize_at_fill_rate default 0.5 → 0.3 (300K threshold at 1M context, under the litellm proxy's ~340K cache-reset cliff).
- R14 (src/agent/context.rs): compact_old_tool_results(messages, keep=10, summary_chars=500) — truncates old tool-result messages to a 500-char summary + marker, keeping the 10 most recent intact. Called after the tool batch in turn.rs, followed by token_accounting.reset(). Idempotent; preserves tool_call_id/name.
- R15 (src/tool/agent/{read_files,file_read}.rs): DEFAULT_MAX_LINES 2000 → 500 in both tools (schema descriptions + tests updated).
- R17 (src/provider/{openai,anthropic}.rs): SANE_MAX_OUTPUT_TOKENS = 32_000 function-local const, applied as .min(SANE_MAX_OUTPUT_TOKENS) before the final .max(1024) floor in both max_completion_tokens/max_tokens formulas. Caps wasteful 128K output budgets on large-context endpoints where the R9 context-window cap never triggers.

Review: .coding/reviews/2026-12-04-r15-r17-cache-optimization-review.md (PASS, 0 findings). All 1703 lib tests pass, zero warnings under deny(warnings).
