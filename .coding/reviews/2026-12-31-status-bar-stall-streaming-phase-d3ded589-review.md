## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (git diff HEAD + untracked) for plan d3ded589 "Status bar keeps the streaming status through mid-stream stalls (match the trace graph)". The code change is correct and complete: the STALL_PHASE_AFTER timer is fully removed with no dead remnants, the emit-on-change phase logic is untouched and correct, the regression test exercises the changed path and would fail on pre-fix code, StallTracker/stall_ms is untouched, and the frontend correctly needs no change. One low finding: a stale doc comment in the very file that was edited still claims the old stall→Waiting flip.

### F1 (LOW) — channels.rs `AgentEvent::Phase` event doc still claims mid-stream stalls flip back to Waiting

`src/runtime/channels.rs:260-264` (the `AgentEvent::Phase` variant doc — NOT part of the diff):

> with [`PhaseKind::Waiting`] (network-bound: TCP/TLS connect, POST upload, time-to-first-token, retry backoff sleeps, AND mid-stream stalls — everything where the turn is blocked on the network with nothing arriving; a streaming phase that goes silent flips back here)

This documents the old behavior (plan 6e734ccb step 7) and now contradicts both the shipped code and the `PhaseKind::Waiting` variant doc 15 lines below it (channels.rs:377-380, added by this change): "A mid-stream stall (byte-silence after streaming began) deliberately does NOT flip back to Waiting". The docs-sync step updated the variant doc but missed the event-level doc that describes the same phase cycle — exactly the "verify no OTHER live doc still claims the stall→Waiting flip" check (focus point 5).

Suggested fix — edit channels.rs:260-264 to drop "AND mid-stream stalls" and the "a streaming phase that goes silent flips back here" clause, mirroring the variant doc's wording, e.g.:

```
/// with [`PhaseKind::Waiting`]
/// (network-bound: TCP/TLS connect, POST upload, time-to-first-token,
/// AND retry backoff sleeps — everything from handing off to the
/// network until the first content byte arrives; a mid-stream stall
/// keeps the current streaming phase), [`PhaseKind::Reasoning`]
```

Doc-only; no behavioral impact.

### Verified — focus points

**1. Timer removal completeness — PASS.** Repo-wide search for `STALL_PHASE_AFTER|stall_timer`: zero matches in `src/` or `src-tauri/`; the only hits are the two new successor knowledge files (which correctly describe the removal), the superseded predecessor spec (frontmatter `status = "superseded"` — the sanctioned path), and the plan files (historical records). The turn.rs stream select (read in context, ~1070-1286) now has exactly two arms — stream events + `cmd_rx` — with no dangling references or orphaned comments; the rewritten comment block above `stream_phase` (~1059-1067) accurately documents the new rationale, and its StallTracker pointer is correct (2s threshold, provider/mod.rs:912). Build is warning-free per the reported run (cargo test 1946 passed / 0 failed, exit=0, `#![deny(warnings)]`).

**2. stream_phase emit-on-change logic — PASS.** Untouched by the diff: TextDelta/ToolCallStart/ToolCallArgumentDelta → Streaming on change; ReasoningDelta → Reasoning on change. With the stall arm gone, the first delta after the silence finds `stream_phase == Some(Streaming)` (or Reasoning) and emits nothing — no double emission, no missed Reasoning↔Streaming flip. A stall before the first delta is unaffected (phase is already Waiting; the removed arm only mattered once streaming began).

**3. Regression test — PASS.** `mid_stream_stall_keeps_streaming_phase` (src/agent/tests.rs:1019) drives StallingProvider (delta "a", 11s silence, delta "b", Finish) under a paused clock and asserts the emitted phases are exactly [Sending, Waiting, Streaming]. Pre-fix, the 10s timer fires during the 11s silence (paused clock auto-advances), emits Waiting and takes stream_phase, and delta "b" re-emits Streaming → [Sending, Waiting, Streaming, Waiting, Streaming] ≠ the new assertion — the test fails on the pre-fix code. StallingStream/StallingProvider doc comments updated to match. Post-fix the silence emits no event (both select arms pending; auto-advance fires the 11s sleep).

**4. StallTracker/stall_ms untouched — PASS.** No `src/provider/` file in the diff (stat: turn.rs, tests.rs, channels.rs, README.md, 2 knowledge files + supersession markers). StallTracker (provider/mod.rs:888+, THRESHOLD 2s) and its wiring (anthropic.rs `stall_tracker.on_chunk`/`finalize`/`set_stall_ms`) are outside the diff; the frontend traceStats.ts stall overlay is untouched.

**5. Docs sync — FINDING F1 above; otherwise PASS.** README inflight bullet updated accurately (waiting pre-first-byte only; a mid-stream stall keeps reasoning…/answering…, matching the trace graphs' red hatch overlay). `PhaseKind::Waiting` variant doc updated. Knowledge supersession done via the sanctioned path: both 2026-12-30 predecessors marked superseded in frontmatter (bodies untouched), two new successors written — both read in full; they accurately describe the removal, the pre-first-byte-only Waiting scope, the untouched StallTracker metric, and the renamed regression test. PLAN.md has no trace-phase prose (its 3 "stall" matches are unrelated: async-runtime stall, hang watchdog, per-install). Frontend comments (LlmTraceView.tsx, TraceStats.tsx, traceStats.ts) all describe stall⊂generate — the trace-graph side, consistent with this change; no update needed.

**6. Multi-platform neutrality — PASS.** Pure Rust logic removal; no platform APIs, paths, or shell syntax anywhere in the diff. (All src-tauri "stall" matches are pre-existing and unrelated — the watchdog's main-thread StallTracker, embedding-install prose.)

**7. Frontend — PASS (no change expected, none present).** No frontend file in the diff. InflightBar.tsx `phaseLabel` is a pure TurnPhase→label function; "waiting…" still exists for the pre-first-byte window. No frontend code or test pins the old flip: the only frontend "Waiting" text is LlmTraceView.tsx:568 ("Waiting for response bytes…" — a trace-record streaming status, unrelated); traceStats.test.ts pins "stall counts as generate" — the consistent attribution. frontend/src/lib/types.ts TurnPhase/phase-event docs are generic and clean.

### Non-findings (checked, no action)

- **Historical artifacts still describing the old flip** — `.coding/backlog.jsonl` item 40bc1a65's done-note ("inflight bar flips to Waiting on 10s mid-stream stalls") and `.coding/plans/6e734ccb.md` step 7. Both are point-in-time completion records of what plan 6e734ccb shipped, not live docs; the supersession knowledge layer is the sanctioned correction path. Not findings.
- **Security** — nothing to review: pure logic removal, no input handling, no unsafe, no secrets, no new dependencies.
- **Stall-then-error path** — a stall that ends in the provider read timeout still surfaces via the provider-layer 90s timeout ("stream stalled — no data for 90s") and the unchanged turn error paths; only the live label differs. Correct per the plan's intent.
