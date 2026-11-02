+++
title = "Mnemo2 Cost tab negative levers render as cost rows, no dollars"
created = "2026-11-02"
+++

DECISION: Mnemo2 Cost tab — a net-negative savings lever (tokens_saved < 0, e.g. archive_expand) renders as a COST row, never a saving: meta cell "+120,000 tokens re-added · 6 events", dollars cell dropped entirely (regardless of what the wire claims), row muted. Ground: the backend prices a net-negative lever at $0 (src-tauri/src/mnemo2/insights.rs, savings pricing clamps at tokens.max(0)), so a dollar figure there is impossible backend output; a "≈ $0.04 saved" next to a minus reads as a saving either way. Implemented in mnemo2/src/lib/cost.ts leverRows (readded flag; costLabel "" for negatives) + CostView.svelte; tests in cost.test.ts (cost row + wire-claims-dollars-anyway) and CostView.test.ts. Seed data must keep negative levers priced 0 (or null) to stay backend-consistent. Review fix by the lead, 2026-11-01.
