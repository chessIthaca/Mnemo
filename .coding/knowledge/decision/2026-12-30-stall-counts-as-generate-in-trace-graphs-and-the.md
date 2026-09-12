+++
title = "stall counts as generate in trace graphs AND the status bar (stall keeps the streaming phase)"
supersedes = "2026-12-30-trace-graphs-stall-counts-as-generate-with-a-dit"
created = "2026-12-30"
+++

DECISION (2026-12-31, user request — amends plan f2b62d85's scope): stall_ms counts as GENERATE in the trace graphs AND the live status bar now matches — the turn loop's STALL_PHASE_AFTER=10s mid-stream Waiting flip (plan 6e734ccb step 7) was REMOVED from src/agent/turn.rs, so a mid-stream stall keeps the current streaming phase (Reasoning/Streaming; the bar keeps showing "reasoning…"/"answering…"). Waiting is pre-first-byte only (connect/POST/TTFT/backoff). Stall stays visible post-hoc as the dithered red hatch overlay in the trace graphs (StallTracker 2s threshold, src/provider/mod.rs — the recorded stall_ms metric is untouched). This supersedes f2b62d85's point "the inflight timeline is INTENTIONALLY unchanged (stall stays a live waiting state there)". Regression test: mid_stream_stall_keeps_streaming_phase (src/agent/tests.rs, paused clock — the 11s silence emits NO phase event; renamed from mid_stream_stall_flips_phase_to_waiting). The on-disk knowledge files are tool-protected; this record is the supersession of record. Durable detail: .coding/knowledge/decision/2026-12-30-trace-graphs-stall-counts-as-generate-with-a-dit.md.
