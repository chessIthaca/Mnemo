+++
title = "config-key changes must sweep docs/CONFIGURATION.md — THE config reference"
created = "2027-01-11"
+++

docs/CONFIGURATION.md is this repo's user-facing configuration reference (README.md is the marketing front door). Any change that adds/removes/renames a config key — config.toml [context]/[general]/[memory]/[ui] dials, Settings-UI-backed keys, endpoints/mcp keys — must update docs/CONFIGURATION.md in the same change. Plan 842c1a70 (proxy-cache ceiling dial removal) gated its docs sweep on PLAN.md + README.md only, missed docs/CONFIGURATION.md:7, and review round 1 flagged it H1 (.coding/reviews/2026-10-15-proxy-cache-ceiling-removal-review.md; fixed in the round-1 fix commit on wt/mnemo). Docs-sync sweep for config-facing changes: docs/CONFIGURATION.md, PLAN.md, README.md, module doc comments.
