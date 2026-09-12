## Verdict: FINDINGS (0 high, 0 medium, 2 low)

**Two fresh LOW findings; all six prior findings verified.** The auto-continuation & context-safety feature (commits 74402dc, 2891c05, bdd3b42) is performance-clean: the streak counter is a `u32` increment, the `hard_ceiling()` check is a single `saturating_sub`, the workflow-Mutex lock in `is_workflow_executing()` fires once per *turn* (not per tool-call iteration), and mid-session consolidation is fire-and-forget on the blocking pool. The codegraph mtime fast-path (prior M1) and the R10 buffer bound (prior L5) and the cached stable head (prior L3) are all correctly in place. The two fresh findings are incremental: the Anthropic provider lacks the R10 repetition guard that OpenAI has, and `spawn_consolidation` has no dedup guard against overlapping mid-session runs.

---

# Performance & Resource-Efficiency Review — Mnemo at HEAD (`wt/agenticcoding`)

**Perspective:** Performance / resource efficiency (read-only)
**Reviewer:** read-only review subagent
**Scope:** whole codebase at HEAD — `src/` (mnemo crate), `src-tauri/` (app shell + IPC), `frontend/` (React/TS). No code executed; findings reasoned from source with file:line evidence.

## Prior findings — verification

| # | Finding (2026-12-15 perf review) | Status | Evidence (HEAD) |
|---|----------------------------------|--------|------------------|
| M1 | Codegraph `index()` re-reads every file every pass (no mtime pre-filter) | **RESOLVED** | `src/codegraph/mod.rs:271-294` — the mtime fast-path is in place. `disk_mtime = mtime_of(path)` (`:278`), then `if stored_mtime == Some(disk_mtime) && has_content && stored_hash.is_some()` (`:287`) skips the `std::fs::read` (`:297`) entirely, `continue`-ing with progress accounting only. The content hash (`:301-303`) remains the authoritative signal when mtime *did* advance (`:312-313`). Correct: steady-state passes are now stat-only. |
| L1 | Constitution reload does a blocking `stat` on the async runtime every loop iteration | **STILL-OPEN (accepted trade-off)** | `src/agent/loop_impl.rs:277` `stable_head()` → `src/project/agent_md.rs:84` `reload_if_changed()` issues `mtime_of` (a `stat`) on both agent.md files every turn. The stat is microseconds and OS-cached; the trade-off is documented inline (`agent_md.rs:75-83`). Accepted. |
| L2 | Safety-rules reload does a blocking `stat` per tool call | **STILL-OPEN (accepted trade-off)** | `src/safety_rules.rs:264-265` `is_safe()` → `reload_if_changed()` stats the rules file on every tool-call approval check. Same micro-cost as L1; accepted. |
| L3 | `build_stable_head` rebuilds the system-prompt head String every turn | **RESOLVED** | `src/agent/loop_impl.rs:273-289` `stable_head()` now caches the head in a `cached_head: Mutex<Option<String>>` and rebuilds only when `reload_if_changed()` reports a constitution change. The per-turn cost is now the L1 stat + a cache hit, not a full head rebuild. |
| L4 | Heavy native deps (fastembed/ONNX, chromiumoxide, tree-sitter) not feature-gated | **STILL-OPEN (deferred, documented)** | `Cargo.toml:52-59` documents the deferral: the three deps are "deeply integrated into the tool registry (factory.rs), IPC commands, and tool categories — not optional plugins," and feature-gating would require stub modules + cfg-gates at every registration point. Assessed too invasive for the remediation sweep; `debug = "line-tables-only"` mitigates link time. Accepted. |
| L5 | R10 repetition-guard `response_text` grows unbounded within a turn | **RESOLVED** | `src/provider/openai.rs:1137` `response_text = bound_repetition_buffer(response_text, needed, REPETITION_BUFFER_CAP)` truncates the buffer after each `detect_repetition` check. `bound_repetition_buffer` (`:1568-1577`) keeps only the last `needed` bytes (char-boundary aligned) when the buffer exceeds `REPETITION_BUFFER_CAP = 2048` (`:138`). The buffer is now O(1) (~2 KB) instead of O(response length). Semantically safe — `detect_repetition` (`:1590`) is purely suffix-based. |

**Net:** M1, L3, L5 resolved since the last review; L1, L2, L4 remain as documented/accepted trade-offs.


## Fresh findings (code shipped since 2026-12-15)

### F1 — LOW: Anthropic provider has no R10 repetition guard (asymmetric streaming efficiency)

**Evidence:** The OpenAI provider aborts stuck/repeating streams via `detect_repetition` (`src/provider/openai.rs:1590`) checked on each `TextDelta` accumulation (`:1137`, bounded by `bound_repetition_buffer`). The Anthropic provider's streaming loop (`src/provider/anthropic.rs:940` `loop {`) accumulates `TextDelta` events (`:1001`) and parses SSE, but has **no** `detect_repetition` / `response_text` / `bound_repetition_buffer` call — confirmed by a targeted search returning zero matches in `anthropic.rs`. Its only stream-termination guard is the per-chunk read timeout (`:943-975`), which fires only on a *dead* connection (no data for `read_timeout`), not on an *active* connection stuck in a repetition loop.

**Why it matters:** A Claude model that degenerates into a repetition loop (the exact failure mode the R10 guard exists to abort on the OpenAI path) streams indefinitely, wasting output tokens until the model self-terminates or hits `max_output_tokens`. This is a resource-efficiency asymmetry: the guard exists for one provider family but not the other. It is LOW because (a) it is a cost/robustness concern, not a hot-path stall or leak, (b) the `max_output_tokens` cap and the read-timeout bound the worst case, and (c) it may reflect a historical OpenAI-first origin rather than a regression.

**Recommendation:** Lift `detect_repetition` + `bound_repetition_buffer` into a provider-agnostic helper (or a small trait method on the streaming consumer) and apply it in the Anthropic loop's `TextDelta` arm, mirroring the OpenAI call site. The constants (`REPETITION_WINDOW`, `REPETITION_THRESHOLD`, `REPETITION_BUFFER_CAP`) are already `pub const` on the OpenAI client (`openai.rs:129,138`) — reuse them.

### F2 — LOW: `spawn_consolidation` has no dedup guard — overlapping mid-session consolidations can double-spend LLM tokens

**Evidence:** The auto-continuation path fires `spawn_consolidation()` every `CONSOLIDATE_EVERY_N_TURNS` (= 8) auto-continues (`src/runtime/agent.rs:213-215`). `spawn_consolidation` (`:767-824`) is fire-and-forget (`tokio::spawn` at `:785`) with **no** "already running" flag — it unconditionally spawns a new task each call. `consolidate_session` (`src/memory/consolidation.rs:107-121`) re-lists the working tier *live* (not from a snapshot) before delegating to `consolidate_session_with_events` (`:133`), which makes 1–3 LLM calls (synthesize + extract semantic + extract procedural) and then `delete_working_for_session`.

**Why it matters:** Consolidation makes LLM calls that can take 10–60s; 8 auto-continue turns (each a full LLM turn) can complete in a comparable window. If a second consolidation spawns before the first has run `delete_working_for_session`, both list the same working rows, both issue LLM synthesis/extraction calls, and both write an episodic row — duplicate LLM token spend + a duplicate episodic memory. If the first *has* finished, the second's list is empty and it no-ops cheaply (`consolidate_session_with_events` returns `Ok("")` on empty events), so the worst case is bounded to the overlap window. LOW because the consequence is wasted tokens + a redundant memory row, not a stall or data loss, and the probability scales with consolidation latency vs. turn speed.

**Recommendation:** Add an `AtomicBool` (or `tokio::sync::Mutex<()>` try-lock) guard in `AgentTask` / `AgentLoop`: `spawn_consolidation` returns early if a consolidation for this session is already in flight, clearing the flag in the spawned task's conclusion. This mirrors the `BUSY` AtomicBool already used by the IPC maintenance layer (`src-tauri/src/ipc/memory_maintenance.rs`).

## Auto-continuation hot path — explicit assessment (per the review mandate)

1. **Streak counter + cap check:** `auto_continue_streak` is a `u32` field (`src/runtime/agent.rs:42`); the cap check `self.auto_continue_streak < MAX_AUTO_CONTINUE` (`:201`) is a single integer compare. **No per-iteration allocation.** The only allocation per auto-continue is the synthetic "continue" `Message` (`:216-225`) — a small `String` + empty `Vec`, unavoidable (the message must enter the conversation). **No finding.**
2. **`is_workflow_executing()` lock frequency:** `src/agent/loop_impl.rs:1311-1314` locks the workflow `Mutex` (`wf.state() == Executing`) — but it is called **once per turn end** in the `None` arm (`agent.rs:202`), *not* per tool-call iteration within a turn. A turn includes ≥1 LLM call (seconds), so the lock acquisition is negligible relative to turn cost. **No finding.**
3. **`hard_ceiling()` check:** `src/agent/context.rs:125-127` is `max_tokens.saturating_sub(compact_headroom_tokens)` — a single subtraction, recomputed only when read (it is not cached, but the cost is one `usize` op). The aggressive `keep_recent=3` vs normal `6` path differs only in the integer passed to `summarize_with_interrupt`; the summarization work itself is identical (same LLM call, same cut-index logic). **No finding.**
4. **`build_turn_provider` config lock scope:** `src/model_resolver.rs:253-256` reads the config `RwLock` once; the guard is held across the cache lookup (`:263-274`) in the hit path and across `build_provider_for` (`:276`) in the miss path. This is a *read* guard held across another *read* guard (different locks — no deadlock), and the cache lookup is a `HashMap::get` (nanoseconds). The guard is contended only on `set_config` (Settings save — rare). **Negligible; no finding.** (A micro-optimization would `drop(config)` after reading `preflight`/`headroom` at `:258`, but the payoff is unmeasurable.)
5. **Mid-session consolidation blocking:** `spawn_consolidation` (`agent.rs:767`) is fire-and-forget; the corpus digest runs in `spawn_blocking` (`:791`), and `consolidate_session` runs on the spawned task — **never blocks the turn loop.** The only concern is F2 above.

## Strengths (verified — still hold at HEAD)

- **Incremental token counting.** `TokenAccounting::update()` (`src/agent/turn.rs:380`) BPE-encodes only messages appended since the previous count. tiktoken BPE is `OnceLock`-cached (`src/agent/context.rs` `try_tiktoken_bpe`).
- **Memory recall off the async runtime.** `recall_peek` wraps the FTS5 candidate fetch + load in `spawn_blocking` (`src/memory/mod.rs:1311-1336`); ONNX embedding inference in `spawn_blocking` (`src/memory/embedder.rs`); SQLite writes in `spawn_blocking`. The read path uses the *read* connection so recall doesn't serialize behind writers (WAL) — `:1302-1308`.
- **SSE streaming is allocation-light.** Both providers reuse a single buffer with in-place `String::drain()` (OpenAI `parse_sse_buffer`; Anthropic `:988` + `:993`). The stream→consumer bridge is a bounded `mpsc::channel`.
- **IPC delta batching is bounded.** `DeltaBatcher` caps each bucket at `DELTA_BUCKET_CAP` (64 KiB, `src-tauri/src/ipc/events.rs:110`) and flushes on `DELTA_FLUSH_INTERVAL` (16 ms, `:105`). The deadline is armed once per batch (`:365`), never reset — no starvation.
- **Trace log is bounded.** `LlmRequestLog` is a ring buffer (`MAX_RECORDS=32`) with a dedicated writer thread; `append_response` caps the mirrored response.
- **Context window is managed.** `compact_old_tool_results` (`context.rs:414`) truncates old tool results (idempotent via `COMPACTED_MARKER`); `summarize_with_interrupt` (`:258`) summarizes the oldest turns at the fill threshold. The new `hard_ceiling()` aggressive-compaction path extends this without new allocation cost.
- **No lock held across `.await`.** The workflow `Mutex` in `is_workflow_executing` is scoped and dropped before the function returns; `spawn_consolidation` clones `Arc`s out before `tokio::spawn`. The SQLite `Mutex` is held only inside `spawn_blocking`.
- **Frontend hot path is memoized.** `Message` is `React.memo`-wrapped with a custom `arePropsEqual` (`frontend/src/components/chat/Message.tsx:35,435`); the agent-event listener is a module-scope singleton with rAF-batched flushing.
- **Provider cache reuses connection pools.** `ConfigModelResolver` caches built providers by `(endpoint, model)` (`model_resolver.rs:167,284-287`) so the `reqwest::Client` connection pool + TLS session survive across turns; invalidated wholesale on `set_config` (`:193-196`).

## Constitution checks

- **Multi-platform neutrality:** The auto-continuation logic (streak counter, `hard_ceiling`, `spawn_consolidation`) is pure Rust with no platform-specific APIs or paths. The one platform-specific path (`reviews_dir` derivation at `agent.rs:780-784`) uses `PathBuf` parent-join with a portable fallback — neutral. No Windows-only perf paths introduced. ✓
- **Warning-free build:** `#![deny(warnings)]` at both crate roots; the prior review's commit log reports green `cargo test`. No `#[allow(...)]` observed in the examined hot-path files. (Could not independently re-run `cargo test` — read-only reviewer.) ✓

## Summary

The performance posture remains strong and the auto-continuation feature is well-engineered for efficiency: the hot path adds only a `u32` increment, one integer compare, one cheap `Mutex` lock per turn, and one small `Message` allocation per auto-continue — all negligible next to the LLM call each turn entails. The `hard_ceiling()` guard is a single subtraction; mid-session consolidation is properly offloaded to a fire-and-forget blocking task. Three of the six prior findings (M1 codegraph mtime, L3 cached stable head, L5 bounded R10 buffer) are now resolved; L1/L2 (per-turn/per-tool `stat`) and L4 (ungated heavy deps) remain as documented, accepted trade-offs. The two fresh LOWs are incremental: an asymmetric repetition guard (Anthropic lacks what OpenAI has) and a missing dedup guard on mid-session consolidation that can double-spend LLM tokens in an overlap window. Neither is a stall, leak, or unbounded-growth defect.
