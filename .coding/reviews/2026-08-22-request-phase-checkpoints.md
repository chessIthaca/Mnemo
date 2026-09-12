# Review: Split the sending black box — prep/compact checkpoints, retry visibility, trace + graph

**Branch:** `feat/request-phase-checkpoints` · **Plan:** 2e8e5beb · **Date:** 2026-08-22
**Scope reviewed:** all uncommitted changes (`git diff HEAD`, 19 files, +585/−25) plus surrounding context in `src/agent/turn.rs`, `src/agent/dispatch.rs`, `src/provider/{openai,anthropic,trace,mod}.rs`, `src/runtime/channels.rs`, and the frontend trace/inflight/reducer files.

## Verdict

The change is well-built: the retry-event semantics are exactly right, the Compacting phase cannot strand the inflight bar, the stamp/clear protocol mirrors the established `last_record_id` discipline, wire types are symmetric (no `skip_serializing_if` asymmetry — `trace.rs` has no serde field attributes, so `Option` always serializes as `null`, matching `number | null`), and docs (README + all touched module/type doc comments) match behavior. **Two Low-severity findings**, both display-only trace-attribution issues. No correctness, security, or constitution violations. Test results (cargo 1263 green, vitest 442, tsc, vite build, src-tauri check) were taken from the plan handoff — this reviewer is read-only and did not re-run them; static inspection found no unused imports, dead code, or undocumented public items, so `#![deny(warnings)]` compliance rests on that reported green run.

## Verified (no findings)

- **Retry event** (`src/agent/dispatch.rs:500-526`): 1-based display (`attempt + 1`) over the 0-based loop; fires exactly once per failed attempt inside `if attempt < 2`, never on the final failure; message delays (`1s`/`2s`) match the actual `tokio::time::sleep(1000/2000ms)`; `format!(…{e}…)` borrows before `last_err = Some(e)` moves. Doc comment's "up to 3s of retry sleeps" = 1s+2s ✓. Pinned by `complete_with_retry_emits_one_retrying_error_per_failure` (`start_paused`) and the updated exhaustion test.
- **Compacting phase** (`src/agent/turn.rs:224-362`): `Sending` is re-emitted unconditionally (lines 253-260) *before* the `match` on the summarize result — there is no early return between the `Compacting` emission and the re-emission, so neither a summarize error (line 261-286) nor an interrupt (`summarize_stop` → `Finished` at 350-362 → frontend sets `idle`) can leave the bar stuck on "compacting context…". Wire form `compacting` pinned by the `channels.rs` serde test and the `useAgentStore.test.ts` phase loop.
- **Measurement windows match the docs**: `prep_started` at loop top (turn.rs:172) → `record_prep_ms` immediately before `complete_with_retry` (turn.rs:635) — deliberately includes the compact window; `compact_started`→`record_compact_ms` brackets only the summarization call (turn.rs:237-251). Both parked *after* the summarization request's own `complete()`, so the compaction call's own trace record is never stamped with the values it generated ✓.
- **Stamp/clear protocol**: validation (`openai.rs:599`, body-build `anthropic.rs:750`) precedes record creation, so the "validation failure leaves them parked" comment is accurate for the retry case; `.take()` clears on stamp; poisoned-lock `.expect(...)` matches existing house style (`last_record_id`, trace lock); `as u32` casts match the existing pattern (turn.rs:1560, openai.rs:872-883). `set_prep_ms`/`set_compact_ms` route through `with_record` (trace.rs:565) so the file-writer mirror sees mutations; no-op for unknown/evicted ids — all pinned by `set_prep_and_compact_ms_target_the_record_by_id` and `pending_prep_compact_ms_stamp_the_next_record` (StubServer/SSE_OK).
- **Frontend reducer claim confirmed**: `reduceError` (agentEventReducer.ts:897-920) already appends retrying errors to transcript + activity log and keeps `running = true`/phase untouched — no frontend change was indeed needed. `InflightBar.phaseLabel` switch is exhaustive over the extended `TurnPhaseKind` (no default arm, so tsc enforces it).
- **Blast radius**: all other `impl LlmClient for …` (21 sites) are test mocks correctly served by the documented no-op defaults; `src-tauri` has one `PhaseKind` fixture reference (`ipc/events.rs:984`, `Streaming`) unaffected by the additive enum variant; `ipc-contract.test.ts` does not enumerate summary fields; no platform-specific APIs introduced (multi-platform neutral ✓).

## Findings

### 1. [Low] Stale `pending_compact_ms` can leak across turns when the request it was parked for never creates a record

**Where:** `src/agent/turn.rs:251` (park compact — only when compaction fired) vs `:635` (park prep — every iteration); consumption at `src/provider/openai.rs:642-659` / `src/provider/anthropic.rs:786-803`.

**What:** `pending_prep_ms` self-heals across turn boundaries because the turn loop overwrites it unconditionally every iteration (turn.rs:635) before any `complete()` runs. `pending_compact_ms` has no such guarantee — it is only re-parked when auto-compaction fires. If the turn ends after parking but before any trace record is created, the value survives and is stamped onto a later, unrelated request's record. Reachable via two real paths:

- **(a) Sticky validation failure.** All 3 retry attempts fail before record creation (`validate_request_messages`, openai.rs:599; `build_request_json`, anthropic.rs:750 — deterministic for the same messages), the take/stamp block never runs, and the turn aborts via `stream_result?` (turn.rs:743) with both values still parked. Next turn re-parks prep (overwrite) but not compact → the next successful request shows a `compact_ms` from the aborted turn.
- **(b) Interrupt/Cancel in the pre-record window.** The `select!` (turn.rs:641-699) can drop `complete_fut` while it is suspended at the `spawn_blocking` await (openai.rs:622-624 / anthropic.rs:766-768 — potentially slow, it serializes a possibly-MiB body), i.e. before the `.take()` lines execute; the turn then returns at turn.rs:714-733 with compact still parked.

(A third, currently-unreachable path: values parked while `trace` is `None` are never consumed and would land on the first record created after a hypothetical runtime `set_trace` — that setter is presently dead API, so noted only for completeness.)

**Impact:** display-only — one later trace record shows a bogus `c …` time. Never a correctness bug (consistent with the documented shared-client caveat, but *these* paths are single-agent and not the documented one).

**Suggested fix:** consume the parked values unconditionally at `complete()` entry — `take()` both right after validation/`build_request_json` (before record creation), hold in locals, and stamp onto the record if one is created. This bounds a parked value's lifetime to the very next `complete()` call and closes all paths. It intentionally changes the documented "validation failure leaves them parked for the retry's record" behavior — but validation-failing turns create *no records at all* (validation fails identically on every retry), so nothing real is lost; update the comments at openai.rs:636-641 and anthropic.rs:780-785 accordingly.

### 2. [Low] TraceStats stacked phase bar double-counts the compact window

**Where:** `frontend/src/lib/traceStats.ts:101-108` (`phaseSeries` segments + `totalMs`); rendered at `frontend/src/components/views/TraceStats.tsx:78-100`.

**What:** `prep_ms` deliberately *includes* `compact_ms` (documented at `src/provider/trace.rs:150-158` and `frontend/src/lib/types.ts:354-359`), but the chart stacks `prep` and `compact` as disjoint segments and sums both into `totalMs`. Every compacted request's bar therefore overstates wall-clock time by `compactMs` (the compact window is drawn twice — once inside `prep`'s width, once as its own segment), which undercuts the chart's "where did the latency actually go" purpose. (The row-level `PhaseTimes` line in `LlmTraceView.tsx` is a labeled list, not a sum — acceptable as-is; the Σ strip shows separate totals — fine.)

**Suggested fix:** in `phaseSeries`, emit the prep segment as `Math.max(0, prepMs - compactMs)` so the six segments are disjoint and `totalMs` equals honest wall time (keep the raw inclusive `prep_ms` on the wire/detail views); update `traceStats.test.ts` to pin the subtraction. Alternatively, keep raw values and disclose the overlap in the bar tooltip/legend — but the subtraction is one line and makes the stack truthful.

## Constitution checks

- **Documentation sync** ✓ — README.md phase-cycle + trace-bucket bullets updated and accurate; doc comments updated in `channels.rs` (Phase/PhaseKind), `trace.rs` (field + setter docs), `dispatch.rs`, `mod.rs` (trait method docs), `turn.rs`, `traceStats.ts`, `TraceStats.tsx`, `InflightBar.tsx`, `agentState.ts`, `types.ts` (wire comments). PLAN.md's "phase" hits are architecture-phase headings — correctly identified as not applicable.
- **Multi-platform neutrality** ✓ — no Windows-only APIs, paths, or shell syntax; `std::time::Instant`/`tokio::time::sleep` are cross-platform; nothing added near the WebView2 gate.
- **Warning-free build** ✓ (by inspection + reported green run) — no new unused imports/dead code; all new public items carry doc comments; no `#[allow(...)]` introduced.
- **Regression tests** ✓ — 4 new tests cover the setters, the park/stamp/clear protocol, the retry event cadence, and the phase sequence; existing fixtures updated (`LlmTraceView.test.ts`, `traceStats.test.ts`, `useAgentStore.test.ts`, `channels.rs` wire test).
