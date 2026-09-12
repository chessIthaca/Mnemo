# Follow-up review: L1/L2/I1/I2/I4 fixes applied after the 2026-08-18 review

**Scope:** verification pass on branch `fix/n1-n6-trace-limits` @ `4d07b9a` (clean tree — `git diff HEAD` empty, as expected). Verifies the five fixes applied in response to `.coding/reviews/2026-08-18-trace-limits-n1-n6-fixes-review.md` (2 Low + 4 Info). Full `cargo test --workspace` (warning-free under `deny(warnings)`) and `npm run build` already pass at this state; not re-run.

**Verdict: all five fixes are correctly in place and clean. No blocking findings.** 1 Low (constitution) + 2 Info below; neither blocks the merge.

---

## Fix verification

**L1 — poll stops on null — VERIFIED** (`frontend/src/components/views/LlmTraceView.tsx:622-697`)
Detail effect stops on `d === null || d.is_complete` (:643); compare effect on `p === null || p.is_complete` (:685). Both `stopPolling` helpers clear the interval and null the timer (:632-637, :670-675); both cleanups still call `stopPolling()` on unmount/re-select (:651-654, :693-696); both catch paths only `setError` and keep polling (:645-647, :687-689). The null-can't-become-non-null invariant holds (ids assigned monotonically by atomic `fetch_add` in `start`, records only ever removed by ring eviction/`clear`, never re-added), and `selectedId - 1` underflow (`0-1 === -1`) just yields null→stop. The list-poll fallback (:597-600) still reselects the newest id after eviction, so the UI doesn't strand on a vanished record. Comments updated to document the invariant.

**L2 — `list()` doc — VERIFIED** (`src/provider/trace.rs:452-455`)
Now states payload-evicted records ARE listed (flagged `response_evicted`, no body) and only ring-evicted records are absent. Matches actual behavior (`list` maps every record; `get`/ring eviction are the only absence sources).

**I1 — `start` moved to blocking pool — VERIFIED** (`src/provider/openai.rs:485-506`)
`log.start(&model, &base_url, body_for_trace)` runs inside `tokio::task::spawn_blocking` (:501) with `model`/`base_url`/`body_for_trace` cloned into the closure (:498-500); the network POST still sends the untouched original `body` (:518). The `let trace = self.trace.read()...clone()` binding (:492-496) precedes the block, the read-guard is a temporary dropped at the semicolon (no lock held across the `.await`), and `trace` is only consumed at :573 (`trace_ctx`), after the three `(&trace, rec_id)` guards at :524/:536/:553. Join error → `.ok()` → `rec_id = None`: every `if let (Some(log), Some(id))` guard then skips trace writes while the caller still gets the real error (:528, :549) — recording degrades, request handling does not. Awaiting the handle before the POST preserves record-before-stream ordering (no `append_response` race). No other provider call site exists (single `.start(` in non-test code). Adjacent code intact: URL construction (:508-511), transport-failure handler (:523-529), status/401/403 handler (:532-552), `set_status` (:553-555).

**I2 — `corpus_digest` on the blocking pool — VERIFIED** (`src/runtime/agent.rs:444-501`)
`corpus_digest(&plans_dir, &reviews_dir)` runs in `spawn_blocking` *inside* the already-spawned consolidation task (:468-470), with owned `plans_dir`/`reviews_dir` moved into the closure (:457-461, consumed by the `move`). Signatures match (`consolidation.rs:57`). The `Err(e)` join arm logs via `eprintln!` and yields `String::new()` (:474-479) — `end_session` (:481-485) and `consolidate_session` (:486-497) then run unconditionally, so consolidation always executes. Exit-path wiring intact: the single post-loop `spawn_consolidation()` (:431) covers both the Cancel `break` (:401-418, with an explicit why-not-double-fire comment) and natural termination; `store`/`provider`/`sid` are `Arc`/owned clones, safe past `AgentTask` drop.

**I4 — dirty gating + invalid revert — VERIFIED** (`frontend/src/components/settings/sections/AdvancedSection.tsx:82-145`)
`budgetValid`/`capValid` (`Number.isFinite && >= 1`, :86-87) gate both knobs in `dirty` (:91-92), so a cleared field (`Number("") === 0`) or `NaN` cannot latch the section dirty. `handleSave` patches only valid+changed knobs (:109-114, unchanged guard) and on success reverts an invalid field to its saved snap instead of latching it (:129-132: snap written only when valid; field state reset to snap when invalid). No React anti-patterns: every setter is inside the async `load`/`handleSave` handlers or effects — none in render phase — and the revert converges (`traceBudget === traceBudgetSnap` → dirty false), so no update loop. Inputs use `Number(e.target.value)` (:373, :391), which produces exactly the 0/NaN cases the gate covers.

## Findings

### Low (constitution)

**F1 — L1 and I4 are behavioral defect fixes with no regression tests** — `frontend/src/components/views/LlmTraceView.tsx:643,685` and `frontend/src/components/settings/sections/AdvancedSection.tsx:86-94,129-132`.
The constitution requires a test for every fixed defect that "fails without the fix and passes with it." The frontend has vitest infra, and this repo already follows the extract-a-pure-helper pattern precisely for this purpose (`GraphView.tsx:63` / `GraphView.test.ts:78` "review F1 regression"). L1's stop condition (`d === null || d.is_complete`) and I4's dirty-gate/revert are pure predicates still inlined in the components — no `LlmTraceView` or `AdvancedSection` test exists. Low because both defects were Low/Info severity and the logic is small, but the repo's own bar says these should be pinned. Cheap follow-up: extract e.g. `shouldStopPolling(d)` / `traceKnobDirty(value, snap)` into testable helpers + table-driven tests.

### Info

**F2 — stale comment implies a second Cancel-path spawn** — `src/runtime/agent.rs:427-430`.
"On natural termination (inbox closed, not Cancel) … same best-effort background spawn as Cancel … not just on cancel" reads as if Cancel has its own spawn. It doesn't — the single post-loop call at :431 covers both, as the (newer, correct) comment at :401-408 states. Cosmetic wording only; suggest "on every termination (inbox closed or Cancel break)".

**F3 — I4 fix covers the two trace knobs only; `fillRate` keeps the pre-existing edge** — `frontend/src/components/settings/sections/AdvancedSection.tsx:89-90,106-108,125`.
Clearing the summarize-rate field yields 0, which still latches dirty and is patched unconditionally, with `setFillSnap(fillRate)` latching 0 locally while the backend clamps — the field shows 0 until the next reload. Pre-existing (explicitly noted as out of scope in the original review) and harmless (`Math.abs(NaN - x) > 1e-9` is false, so the NaN case never latches). Same extract-and-gate treatment as F1 would close it.

## Adjacent spot-checks (clean)

- `openai.rs:508-576` — URL construction, send/status error handlers, `set_status`, `trace_ctx` construction: unchanged semantics, correct guard structure.
- `agent.rs:401-432` — Cancel path + post-loop consolidation: single-fire, `Exited` ordering preserved.
- `src/tool/memory/mod.rs:294-298` — the remaining sync `corpus_digest` call is the memory-tool manual path (explicitly the mirror of the automatic path, bounded by the same ~20-file corpus). The I2 finding only ever named the exit path; not a regression.
- `trace.rs:335-349` — `start` unchanged apart from being called off-worker; `cap_request_json` tests intact; all other `log.start` call sites are unit tests.
- I3 (`src-tauri/Cargo.toml` CRLF churn): working tree is clean; nothing to include.
