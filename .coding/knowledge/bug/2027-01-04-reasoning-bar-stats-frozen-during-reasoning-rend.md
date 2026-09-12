+++
title = "reasoning bar stats frozen during reasoning — 🧠 render gate gated on landed usage"
created = "2027-01-04"
+++

BUG: reasoning bar stats frozen during reasoning (user report 2027-01-04). Symptom: InflightBar stats row sat frozen during a long reasoning phase while the reasoning panel streamed; everything refreshed only when reasoning completed. Root cause: the 🧠 counter's render gate `tokenUsage.reasoning > 0` (InflightBar.tsx:286) — tokenUsage.reasoning is only set by the Usage event AFTER a response completes, so during the first reasoning phase of every turn (`started` resets tokenUsage to zeros each turn) the counter was hidden even though its target already rode liveReasoningTokens (chars/4 per reasoning_delta, prior art 4173dda). Fix: gate admits the live estimate — `(tokenUsage.reasoning > 0 || liveReasoningTokens > 0)`. Regression test: InflightBar.test.ts "shows the 🧠 counter during the first reasoning phase — the gate admits the live estimate, not just landed usage".
