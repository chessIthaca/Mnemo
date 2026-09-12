## Verdict: FINDINGS (0 high, 4 low)

Live token + trace updates during reasoning (plan 275d5568, backlog b5503915): correct on the normal paths, well-tested, safe guards; 4 low findings (one narrow defensive hardening, two doc-sync nits, one UI-honesty nit).


## Scope reviewed

All uncommitted changes on `wt/agenticcoder` for plan 275d5568: `src/provider/trace.rs` (update_streaming_usage + 2 tests), `src/provider/openai.rs` + `src/provider/anthropic.rs` (stream-loop overlays), `frontend/src/hooks/agentState.ts`, `agentEventReducer.ts`, `InflightBar.tsx`, `LlmTraceView.tsx`, and both test files. Read from source (not just the diff), plus the surrounding infrastructure the change plugs into (`with_record`/`finish`/`fail` semantics, `parse` order, `traceStats.ts` aggregation, `turn.rs` RequestStats recording).

## What was verified correct

- **Terminal-record guard is airtight.** `with_record` recomputes `r.is_complete = r.finish_reason.is_some() || r.error.is_some()` after *every* mutation under the same lock, so the `if r.is_complete { return }` check in `update_streaming_usage` (trace.rs:488) can never race a concurrent `finish`/`fail`. Finished/failed records cannot be resurrected — pinned by `streaming_usage_overlay_lands_and_is_overwritten_by_final_usage` (trace.rs:1566-1569).
- **Ring-eviction safety.** Unknown/evicted ids take the same silent no-op path as every other mutator (pinned by `streaming_usage_overlay_is_a_no_op_for_unknown_ids`, and by the pre-existing `with_record(9_999, …)` pattern at trace.rs:1520).
- **Authoritative overwrite.** Both providers' `set_usage` (openai.rs Usage-enrichment arm; anthropic.rs message_stop-emitted Usage) replaces the whole `usage` struct — no merge with the overlay. `..prev` only preserves `prompt`/`cached`, which are 0 until the final usage in both protocols (Anthropic captures input tokens at `message_start` but emits Usage only at `message_stop`), so the preservation is defensively correct.
- **Normal-path ordering is safe.** OpenAI: `finish_reason` chunk precedes the usage chunk, so the record is terminal before `set_usage`; any post-usage chunk's overlay push hits the guard. Anthropic: Usage **and** Finish are both emitted from `message_stop`, the last SSE event of the stream — back-to-back in the same chunk, then the stream ends (`Ok(None)` → break, no overlay path).
- **Throttle logic.** `now` is stamped once per `Ok(bytes)` (openai.rs:933) and reused consistently for first-chunk/first-content/throttle — same convention as the pre-existing timestamps. `last_usage_push` only advances when a push fires; first chunk always pushes (map_or(true)). 500ms cadence bounds file-mirror churn well below the pre-existing per-chunk `append_response` dirtying.
- **Provider parity.** openai.rs:940-955 and anthropic.rs:1031-1055 are exact mirrors (same three locals, same accumulation arms, same throttle); the TextDelta/first_content_chunk semantics are preserved by the split match arms (openai.rs:983-998).
- **Stats isolation.** `RequestStats` (turn.rs:1119-1166) is built from the `LlmEvent::Usage` fields — estimates never reach memory.db. `SessionTiming` (FE) likewise only feeds on Usage events.
- **FE bucket split is complete.** All five reset sites zero both fields (started 214-215, usage 793-794, finished 1215-1216, final error 1284-1285, child-terminal 1323-1324); delta arms increment the right bucket (273-274, 315-316). No `liveTokens` references remain in source (only a test *title* string). Mid-turn multi-request display semantics ("last request's final + current request's live") are consistent with the bar's pre-existing behavior.
- **UI honesty.** `cacheHitPct` returns null for prompt=0 (traceStats.test.ts:150), so the cache bar/badge and cache-hit averages stay hidden/unchanged during overlays. The amber "est. while streaming" marker + tooltip accurately describe chars/4 and authoritative replacement. Polling untouched (`shouldStopPolling` keys on `is_complete`, which overlays never set — pinned by the new test's `assert!(!d.is_complete)`).
- **Multi-platform neutrality.** No platform-specific code anywhere in the change. ✅
- **Style.** Doc comments on the new public fn and the FE props match house style; no `#[allow]`; deny(warnings) build clean per the stated test status.

## Findings

### LOW 1 — Overlay can clobber authoritative usage on a narrow non-conforming-server window

`src/provider/openai.rs:946-955` — the throttled overlay push runs in the `Ok(bytes)` arm **before** parsing, gated only on the record not being terminal. Sequence that breaks: a server that emits the Usage event *without* a prior `finish_reason` chunk (non-conforming OpenAI-compatible gateway/proxy), then sends another non-empty chunk (e.g. `data: [DONE]` on its own, or trailing bytes) ≥500ms after the last push. Then: usage chunk → `set_usage` lands authoritative values on a still-non-terminal record → next chunk's Ok(bytes) arm fires the overlay (≥500ms elapsed, `is_complete` still false) → chars/4 estimates **permanently overwrite** the authoritative usage (in-memory + file mirror) → fallback `finish` only afterwards. Both mainstream protocols are immune (finish_reason-before-usage; message_stop emits Usage+Finish back-to-back), and the ≥500ms inter-chunk gap makes it rare even for odd servers — hence LOW, but the damage (silent wrong data in the Trace record) is permanent.

*Suggested fix (one line):* stop pushing once the authoritative usage has been sent — e.g. set a `final_usage_seen = true` local in the Usage arm and add `&& !final_usage_seen` to the push condition (both providers).

### LOW 2 — Stale FE field name in the new doc comment

`src/provider/trace.rs:477-478` — the doc comment for `update_streaming_usage` says "(the same convention the frontend toolbar's `liveTokens` estimate uses)". `liveTokens` no longer exists — it was split into `liveCompletionTokens`/`liveReasoningTokens` by this very change. Say "the frontend toolbar's live token estimates" or name the new fields.

### LOW 3 — `usage` field docs no longer tell the whole truth

`src/provider/trace.rs:139-140` (`LlmRequestRecord.usage`: "Parsed token usage from the stream's final chunk, if any.") and the `LlmRequestSummary.usage` doc (~line 211) predate the overlay; the field can now hold live chars/4 estimates on in-flight records. Add a half-sentence ("…or live chars/4 streaming estimates until the stream finishes") so readers of the wire type aren't misled. (Optional: a one-line note in the trace module docs would be nice-to-have, not required.)

### LOW 4 — Trace list row shows a literal "0 in" while streaming

`frontend/src/components/views/LlmTraceView.tsx:182-189` — the row switches from "…" to `{prompt} in · {completion} out` as soon as the first overlay lands, but prompt stays 0 for the whole stream (only `set_usage` sets it), so in-flight rows read "0 in · 123 out". "0 in" is never a real value and the list row has no estimate marker (only the detail view does). Cosmetic; e.g. render "—" for prompt while `!r.is_complete` (the summary would need to expose `is_complete`, which it already does per shouldStopPolling's use of detail — summary carries it too).

### Note (no action required) — session totals in TraceStats briefly include estimates

`frontend/src/lib/traceStats.ts:173-177` — `traceSummary`/`tokenSeries` now include in-flight rows' estimated completion/reasoning until the authoritative usage lands (prompt=0 rows stay out of cache-hit averages via the null pct). This is transient, self-correcting, and arguably the point of the feature (live charts); noted only so it's a known consequence, not an accident.

## Docs sync check

No PLAN.md/README section describes per-field trace usage semantics; the README "full request tracing" bullet remains accurate. The only doc debt is findings 2–3 (code-level doc comments). ✅ otherwise.

## Tests

Rust: both new tests exercise the real path (overlay lands monotonic, final usage overwrites, terminal no-op, unknown id no-op) — trace.rs:1526-1577. FE: reducer test asserts both buckets independently, the usage snap-to-zero, and finished-reset (useAgentStore.test.ts:106-144); InflightBar source-contract pins both counter targets. Stated status (cargo test 1724/0, deny(warnings) build clean, vitest 703/703, tsc clean) is consistent with everything read; reviewer is read-only and did not re-execute.
