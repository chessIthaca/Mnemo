+++
title = "Dashboard savings rows show % savings (fmtSavingsPct) + levers default ON"
created = "2027-01-11"
status = "superseded"
+++

Landed 2027-01-25 (plan 4bab94c1, commit 12a65d1 on wt/mnemo). Two durable contracts for this change; (1) the levers themselves are recorded in the DECISION memory "[general.optimizer] levers default ON, not off":
1. `src/config/general.rs` — `OptimizerConfig::default()` = all six levers `true`; `is_default()` ANDs the six flags + compares all four knobs + `compress_extra_commands.is_empty()`, so a default (all-ON) config still writes NO `[general.optimizer]` section, and the section appears only once a lever is opted out or a knob moves. Stored explicit `false` values are honoured (deliberately no migration).
2. `frontend/src/components/views/DashboardView.tsx` (`dashboard-recent` card) — the recent-events row renders `fmtSavingsPct(e.tokens_saved, e.tokens_before)` where it used to print `before → after`, alongside the unchanged emerald `fmtTokens(e.tokens_saved)`; the raw `before → after` sizes stay reachable in the cell's `title` (hover). `fmtSavingsPct` (frontend/src/lib/format.ts) yields "—" when `before <= 0` (zero/negative/NaN before → never NaN%) and otherwise delegates to the project-wide `${fmtPct(x)}%` rule (≤1 decimal; negative savings stay signed, never clamped).
Pinned by: DashboardView.test.tsx (the % form, the hover title, `tokens_before: 0` → "—") and format.test.ts (9 cases). Copy updated to the new default: Dashboard empty state, Settings → Savings ("All on by default; uncheck a lever to opt out"), README/PLAN/docs/{CONFIGURATION,FEATURES}.md. Review: .coding/reviews/2026-09-26-optimizer-levers-default-on-review.md (PASS).
