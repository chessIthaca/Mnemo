+++
title = "InflightBar 🧠 counter renders on the live estimate during reasoning (gate admits liveReasoningTokens)"
created = "2027-01-04"
+++

SPEC: InflightBar 🧠 reasoning-counter visibility — the counter renders whenever there is anything to show: `(tokenUsage.reasoning > 0 || liveReasoningTokens > 0)` (InflightBar.tsx, was gated on landed usage alone). During every turn's first reasoning phase (`started` resets tokenUsage) the counter appears with the first reasoning delta and ticks continuously on the chars/4 live estimate (prior art 4173dda); after Usage lands it stays on the authoritative count, and hides again only if the provider reports reasoning_tokens = 0 (liveReasoningTokens resets on usage). Detail: .coding/knowledge/bug/2027-01-04-reasoning-bar-stats-frozen-during-reasoning-rend.md, commit 8dd2d0d.
