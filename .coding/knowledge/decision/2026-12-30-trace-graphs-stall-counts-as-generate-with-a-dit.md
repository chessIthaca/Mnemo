+++
title = "trace graphs — stall counts as generate with a dithered red hatch overlay"
created = "2026-12-30"
status = "superseded"
+++

DECISION (2026-12-30, plan f2b62d85, user request — follow-up to the stall-mechanics Q&A): in the trace graphs, stall_ms counts as GENERATE, not as a separate waiting segment.

Rationale: stall_ms is a subset of generation_ms by construction (StallTracker measures byte-silence gaps > 2s INSIDE the streaming window — provider/mod.rs:903; the pre-first-chunk wait is ttft, connect/backoff are separate buckets). Carving it out as its own red segment (the pre-f2b62d85 rendering) double-carved the generate window; the user wants the generate segment to show the full generation window with the stalled share visible as an overlay.

The rendering (mirrors the cached-token pattern exactly):
- generate segment value = generation_ms − reasoning_ms (stall INCLUDED; the dead `answerMs` field was removed from PhaseSeriesRow).
- The stalled share renders as a dithered RED hatch inside the generate segment (stall's identity color, rgba(248,113,113,0.22) + repeating-linear-gradient(45deg, rgba(248,113,113,0.85) 0 2px, transparent 2px 5px)), bottom-anchored, coverage = stallSharePct = min(100, stall / (gen − reason) × 100). The clamp matters: a stall during the reasoning window is counted in BOTH reasoning_ms and stall_ms, so stall can exceed the generate remainder.
- The `stall` legend chip toggles ONLY the overlay (height-neutral, like `cached`): new phaseHeightVisibility excludes stall from the height sum (mirrors tokenHeightVisibility); PhaseChart stacks PHASE_SEGMENTS minus stall.
- LlmTraceView PhaseTimes + UsageCard are TEXT lines (no bars), so the hatch lives only in TraceStats; there the merged text treatment applies: `g X (s Y)` — genShown = gen − reason, the stalled time shown parenthetically, no separate disjoint `s` part.
- The inflight timeline is INTENTIONALLY unchanged: it is a live Phase state machine (stall stays a live waiting state there) — a different surface from the recorded-ms trace graphs.

Files: frontend/src/lib/traceStats.ts (phaseMsByKey, phaseHeightVisibility, stallSharePct, PHASE_SEGMENTS doc), frontend/src/components/views/TraceStats.tsx (PhaseChart), frontend/src/components/views/LlmTraceView.tsx (PhaseTimes, UsageCard). Tests: traceStats.test.ts (generate-includes-stall, height-neutral stall chip, stallSharePct incl. the clamp, tooltip 6.4s).
