+++
title = "shell streaming doc claims that overstate the emit-rate cap (3 sites, non-blocking)"
created = "2027-01-11"
+++

The "~16 emits/s" emit-rate claim is IMPRECISE and lives at three sites: src/tool/agent/shell.rs:48-52 (the `STREAM_FLUSH_INTERVAL` const doc, pre-existing), frontend/src/lib/types.ts:182-184 (the `tool_output_delta` variant doc, authored by this changeset), and src-tauri/src/ipc/events.rs:156-159 (pre-existing, outside this changeset). Why: the byte rule (`STREAM_FLUSH_BYTES` 8 KiB) flushes sooner than the 60 ms interval, so a fast producer emits back-to-back — the cap tests push ~400 KB through tens of flushes, far above 16/s instantaneously.

The design conclusion the sentence supports still HOLDS: per-call delta volume is bounded (byte path ≤ STREAM_CAP/8 KiB ≈ 33 flushes; timer path ≤ one per interval), so "the forwarder needs no special batching" stays true, and no test or invariant keys on 16/s.

Flagged by the round-3 reviewer as an explicitly NON-BLOCKING "Flagged doc note" (.coding/reviews/2026-09-18-live-shell-output-streaming-review-round3.md), and deliberately NOT fixed after that PASS so the reviewed tree is exactly what shipped (plan 0d2c1221, branch wt/mnemo).

Exact fix when a future change touches these sites — name the exception: "the TIMER path emits at most once per interval; `STREAM_FLUSH_BYTES` may flush sooner, so the instantaneous rate is bounded by output volume, not ~16/s".
