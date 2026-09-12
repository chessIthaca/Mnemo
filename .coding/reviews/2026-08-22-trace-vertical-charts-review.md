# Review: Trace-tab rework (vertical charts, splitter, provider in rows)

Branch: `feat/trace-vertical-charts`. Scope reviewed: ALL uncommitted changes
(`git diff HEAD` — 17 modified files + 1 untracked plan file), cross-checked
against the plan's stated goals and the seven focus points in the review task.
Verification was read-only (diff + full-file reads + targeted searches); the
green matrices reported by the implementer (cargo 1292, vitest 481, tsc) were
relied on for compile/test-level facts and are consistent with what the diff
shows.

## Verdict

The implementation is correct and complete against the spec. All focus points
verified clean (details at the bottom). Two findings — one Low (documentation
sync), one cosmetic nit — plus one informational note requiring no action.

---

## Findings

### LOW — `.coding/llm-trace.md` not synced with the change

`.coding/llm-trace.md` is the user-facing doc for the exact feature this plan
reworks, and only README.md was updated.

- **Line 29** ("What each request row shows"): the enumeration "model, prompt
  in / completion out token counts, TTFT in ms" is now stale — rows also show
  the **provider** (`model · provider`, dimmed, LlmTraceView.tsx:171-178).
  Add provider to that bullet list.
- The doc is silent on the stats section entirely (it never described the old
  bottom-half graphs either), so the move to a resizable top strip does not
  contradict it; one sentence in the intro or a short section noting the top
  stats strip (vertical columns, newest rightmost, Fill/Relative toggle,
  drag-to-resize) would close the gap, but the row-contents bullet is the
  stale claim that must be fixed per the constitution's documentation-sync
  rule.
- Pre-existing (NOT caused by this diff — optional same-file touch-up, do not
  treat as a required fix): line 112 says capture happens inside
  `OpenAiClient::complete()` only, but `AnthropicClient::complete()` has
  captured traces since the Anthropic endpoint shipped (anthropic.rs:784-796).

### NIT — RequestRow tooltip order is the reverse of the displayed order

`frontend/src/components/views/LlmTraceView.tsx:174` sets
`title={`${r.provider} · ${r.model}`}` while the row renders
`{r.model} · {r.provider}` (model first). Harmless, but hovering shows the
fields in the opposite order from the row. One-line fix: swap the title to
`` `${r.model} · ${r.provider}` `` (or swap the display — just make them
match).

---

## Informational (no action required)

- **Ghost drag if the ring empties mid-drag** (LlmTraceView.tsx:967): the
  stats section + splitter unmount when `requests.length` hits 0. If that
  happened mid-drag, the window-level listeners live on the (still-mounted)
  view, so the drag would silently continue until pointerup/pointercancel
  (which still fire and clean up) or view unmount (the `statsDragCleanup`
  effect covers it). Practically unreachable — pointer capture on the handle
  prevents clicking the Clear button mid-drag — and self-healing. No leak,
  no fix needed; recorded only for completeness.
- `clampStatsHeight` gives the 140px floor precedence over the 70% cap when
  the tab is extremely short (`total * 0.7 < 140`). Identical to the
  sanctioned GraphView pattern this mirrors (GraphView.tsx:105); not a
  finding.

---

## Focus-point verification (all PASS)

1. **Vertical layout.** `Column` uses `flex-col-reverse` so the FIRST child
   anchors to the bottom: prep in PhaseChart (segments ordered prep → tools,
   TraceStats.tsx:141-151), prompt in TokenChart (:210-228). Series stay
   oldest-first end-to-end (`latestRows` keeps input order, traceStats.ts:122;
   charts receive the unreversed `requests`), so newest is rightmost. Sliver
   guard `Math.max(2, columnHeightPct(...))` cannot exceed 100: fill caps at
   100, relative is `total/max` with `max` drawn from the same non-negative
   series (all inputs are `?? 0`-defaulted u64-derived counts or sums clamped
   ≥ 0, traceStats.ts:128-155). Cached overlay clamped 0-100 with a `prompt>0`
   guard (:201). `heightPct: 0` renders a 0%-height bar inside the
   `overflow-hidden` faint track — track only, as specified.
2. **Toggle.** `columnHeightPct` implements fill→100-for-non-zero and
   relative→`total/max*100` exactly as specified, with the max=0 guard; the
   vitest table pins the 2-min-vs-30-s 4× acceptance example (100 vs 25).
   CacheChart takes no `mode` (fixed 0-100 scale) ✓. Default is "relative"
   (`storedScaleMode`, :78-80), persisted under `tracestats.scaleMode` only on
   click. Hooks rules: `useState` (:296) is the ONLY hook in `TraceStats` and
   runs unconditionally before the `rows.length === 0` early return (:297) —
   hook order is stable whether or not the early return fires.
3. **Splitter.** Drag DOWN grows the top section: `startHeight +
   (ev.clientY - startY)` (LlmTraceView.tsx:~891) — sign verified against the
   mirrored GraphView bottom-pane pattern, which correctly uses MINUS for a
   bottom pane (GraphView.tsx:506). Clamp bounds pinned by the new vitest
   table (140 floor, 70% cap, rounding). localStorage is written only in
   `endDrag`; `pointercancel` routes to the same cleanup with a try/catch
   around `releasePointerCapture`; the `statsDragCleanup` ref + unmount effect
   (:694-699) tears down an in-flight drag; the mount re-clamp (:703-706)
   re-clamps a persisted-too-tall height against the live tab height, exactly
   mirroring GraphView review N2 (GraphView.tsx:286-293).
4. **Provider threading.** `client_factory.rs` sets `provider:
   endpoint.name.clone()` on BOTH config builders (:90, :113). Production call
   sites openai.rs:647-656 and anthropic.rs:784-796 clone
   `self.config.provider` into the `spawn_blocking` closure and pass it as the
   THIRD argument to `start(model, base_url, provider, json)`. All ~46
   `start(` call sites (searched) pass provider in the correct slot — no
   base_url/provider swap anywhere. `From<&LlmRequestRecord> for
   LlmRequestSummary` clones provider (trace.rs:253). TS types gained
   `provider: string` on both Detail and Summary with matching doc comments.
   The `main.rs:1038` no-endpoint fallback uses `provider: "dummy"`, which is
   honest — that branch is explicitly the dummy provider
   (`eprintln!("warning: no endpoint configured; using a dummy provider")`,
   main.rs:1027).
5. **Regex-edited test literals (openai.rs).** All ~24 inserted lines are
   well-formed `provider: "test".into(),` lines correctly indented inside
   their config literals (diff context verified line-by-line); a search for
   orphan `^\s*:` garbage lines in openai.rs found none; the 1292-test green
   run is compile-level proof nothing was mangled.
6. **Constitution.** Doc comments on every new public item (`clampStatsHeight`,
   `columnHeightPct`, `tokenStackTotal`, `ScaleMode`, the `provider` fields on
   both Rust configs + record/summary, TS interfaces); `LlmRequestLog::start`
   keeps its existing doc comment (it never enumerated params — existing
   style). New pure helpers are unit-tested, including the acceptance example
   and both zero guards. No `#[allow(...)]`, `@ts-ignore`, `@ts-expect-error`,
   or `eslint-disable` added (searched; remaining hits are pre-existing prose
   in `.coding/`). Warning-free under `#![deny(warnings)]` per the green
   matrix. Docs: README bullet rewritten and accurate against the
   implementation; PLAN.md contains zero trace-chart claims (searched — no
   staleness there); module doc comments on both touched views updated.
   Multi-platform neutrality: pure TS/CSS (`writingMode: vertical-rl` is
   standard CSS) + portable Rust; no `cfg(windows)`, no OS-specific paths or
   shell syntax in the new code. The `.coding/` changes are plan/bookkeeping
   state and appropriate to include in the commit.
7. **Security.** `provider` is the endpoint NAME label from endpoints.toml
   (e.g. "openai", "ollama-local"), never a credential; it rides the same
   serialization path as `base_url` and lands in traces.jsonl as a plain
   label. Existing secret-redaction tests (request body, response body, error
   log) are untouched and green.

## Files reviewed

`src/provider/trace.rs`, `src/provider/openai.rs`, `src/provider/anthropic.rs`,
`src/provider/client_factory.rs`, `src/provider/vision.rs`,
`src-tauri/src/main.rs`, `tests/provider_integration.rs`,
`frontend/src/lib/types.ts`, `frontend/src/lib/traceStats.ts` (+test),
`frontend/src/components/views/LlmTraceView.tsx` (+test),
`frontend/src/components/views/TraceStats.tsx`, `README.md`,
`.coding/llm-trace.md` (found stale — see findings), `PLAN.md` (checked clean),
plus `.coding/` bookkeeping.
