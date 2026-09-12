## Verdict: FINDINGS (0 high, 6 low)

**One MEDIUM + five LOW.** All seven prior findings verified: five RESOLVED, two STILL-OPEN (both documented/accepted trade-offs). The codebase is performance-solid on the critical hot paths — the agent turn loop, SSE streaming, memory recall, and IPC batching are all well-engineered with the right caching, `spawn_blocking` discipline, and bounded buffers. The fresh findings are incremental optimizations, not stalls or leaks.

---

# Performance & Resource-Efficiency Review — Mnemo at HEAD

**Perspective:** Performance / resource efficiency (read-only)
**Reviewer:** read-only review subagent
**Scope:** whole codebase at HEAD — `src/` (mnemo crate), `src-tauri/` (app shell + IPC), `frontend/` (React/TS). No code executed; findings reasoned from source with file:line evidence.

## Prior findings — verification

### 2026-08-13 review

| # | Finding | Status | Evidence |
|---|---------|--------|----------|
| B1 | `syntect` dead dependency | **RESOLVED** | Not present in `Cargo.toml` (lines 12-86). No `syntect` reference anywhere in the tree. |
| B2 | `markdown` dead dependency | **RESOLVED** | Not present in `Cargo.toml`. No `markdown` reference. |
| R1 | tiktoken BPE re-initialized per `count_tokens` | **RESOLVED** | `try_tiktoken_bpe()` (`src/agent/context.rs:696-700`) caches the `CoreBPE` in a `OnceLock<Option<CoreBPE>>` — initialized once, reused forever. Token counting is now incremental: `TokenAccounting::update()` (`src/agent/turn.rs:380`) only BPE-encodes messages appended since the previous count + a head swap when the system head changed, replacing the old per-iteration full-history pass. |
| R2 | memory rusqlite calls on the async runtime | **RESOLVED** | `recall_peek` (`src/memory/mod.rs:1311`) wraps the FTS5 candidate fetch + load in `spawn_blocking`. `write` (`:1254`), `access` (`:1449`), `batch_access` (`:1472`) all use `spawn_blocking`. The embedding query (`embedder.embed(query).await`, `:1286`) runs ONNX inference in `spawn_blocking` (`src/memory/embedder.rs:128-129`). No synchronous SQLite on a tokio worker. |
| M1 | browser temp-sweep under the state lock | **RESOLVED** | `sweep_stale_profiles` (`src/browser/mod.rs:319`) now spawns the kill + profile-dir removal on a fire-and-forget `spawn_blocking` task (`:285`), outside the `BrowserManager` state lock. `schedule_profile_removal` (`:671`) uses `std::thread::spawn` (`:673`) for the kill+cleanup. No lock held during I/O. |

### 2026-09-15 review

| # | Finding | Status | Evidence |
|---|---------|--------|----------|
| F1 | `is_project_scoped` blocking `canonicalize` | **STILL-OPEN (accepted trade-off)** | `src/agent/approval.rs:114-123` still calls `std::fs::canonicalize` synchronously. The inline comment documents the trade-off: canonicalize is needed to resolve symlinks/relative paths for the safety-boundary check, and it fires only on the approval path (not per-turn). Accepted. |
| F8 | `complete_with_retry` fixed backoff, no jitter | **STILL-OPEN** | `src/agent/dispatch.rs:643-680`: `let mut delay_ms = 1000u64;` → fixed 1s→2s backoff, no jitter. The turn-level retry (`run_turn_attempt`, `src/runtime/agent.rs:362`) uses `1000 * 2^(attempt-1)` exponential, also no jitter. LOW — under synchronized load, parallel retries could thunder. Mitigated: 429s now short-circuit via the cross-provider fallback (no backoff sleep), so the fixed backoff only applies to transient connection failures. |


## Fresh findings (code shipped since 2026-09-15)

### M1 — MEDIUM: Codegraph `index()` re-reads every file every pass (no mtime pre-filter)

**Evidence:** `src/codegraph/mod.rs:272` — `let Ok(bytes) = std::fs::read(path) else { ... };` reads every file to hash it (`:276-278`), then compares the hash to the stored value (`:289`). The mtime IS recorded (`mtime_of` at `:306`, `:393`) but is NOT consulted as a fast-path before the read. The watcher's `debounce_loop` (`src/codegraph/watcher.rs:163`) coalesces a save burst into one `graph.index(None)` call, but that call still walks and reads every file.

**Impact:** O(total project size) of disk reads per index pass — at startup and on every save burst. For a large project (thousands of files) this is seconds of background I/O on every edit storm. It runs in `spawn_blocking` (no async-runtime stall, no UI freeze), which is why this is MEDIUM not HIGH.

**Fix:** Before `std::fs::read`, compare the stored mtime to `mtime_of(path)`; skip the read+hash when unchanged. The mtime is already stored in `cg_content_meta`; the content hash remains the authoritative change signal when mtime did advance. This is the standard mtime-fast-path + content-hash-fallback pattern.

### L1 — LOW: Constitution reload does a blocking `stat` on the async runtime every loop iteration

**Evidence:** `src/agent/turn.rs:663` `self.constitution.constitution()` → `src/agent/loop_impl.rs:266-270` `ConstitutionHolder::constitution()` calls `src.reload_if_changed()` → `src/project/agent_md.rs:74` `reload_if_changed()` issues `std::fs::metadata` (stat) on both the global and project `agent.md`. This runs every loop iteration (every turn, plus every tool-call iteration within a turn).

**Impact:** A `stat` is microseconds and OS-cached, but it is technically blocking I/O on a tokio worker thread (two stats per iteration). Negligible in practice; noted for completeness.

### L2 — LOW: Safety-rules reload does a blocking `stat` on the async runtime per tool call

**Evidence:** `src/safety_rules.rs:253-254` `is_safe()` calls `reload_if_changed()`, which stats the rules file. Called on every tool-call approval check.

**Impact:** Same as L1 — a cheap stat, but on the async runtime. Negligible.

### L3 — LOW: `build_stable_head` rebuilds the system-prompt head String every turn

**Evidence:** `src/agent/turn.rs:666` `build_stable_head(&constitution)` (`src/agent/prompt.rs:392`) rebuilds the head String from the constitution + five `const` sections every loop iteration. The head is documented as "byte-stable for the whole session" (`turn.rs:698`), so it only changes when the constitution file changes (already detected by the mtime check in L1).

**Impact:** A few-KB allocation per turn. The volatile tail (with recalled memories) must be rebuilt, but the stable head could be cached and invalidated only on a constitution reload.

### L4 — LOW: Heavy native deps (fastembed/ONNX, chromiumoxide, tree-sitter) are not feature-gated

**Evidence:** `Cargo.toml:51` `fastembed = "4"` (ONNX Runtime), `:70` `chromiumoxide = "0.7"` (Chromium CDP), `:74-76` `tree-sitter` + grammars — all unconditional; there is no `[features]` section to gate them. Contrast: `reqwest` (`:54`, `default-features = false`) and `image` (`:67`, `default-features = false`) ARE feature-minimized, so the team knows the pattern. `tokio = { features = ["full"] }` (`:14`) similarly pulls every tokio feature.

**Impact:** A dev iterating on the agent loop (no embeddings, no debug browser, no codegraph) still compiles ONNX Runtime, Chromium automation, and tree-sitter grammars on every build — the heaviest native deps in the tree. The `[profile.dev] debug = "line-tables-only"` (`:100-101`) already mitigates link time. Accepted trade-off (all are core production features), but optional feature flags would speed the dev loop.

### L5 — LOW: R10 repetition-guard `response_text` grows unbounded within a turn

**Evidence:** `src/provider/openai.rs:878` `let mut response_text = String::new();` accumulates every `TextDelta` (`:1065` `response_text.push_str(text)`). `detect_repetition` (`:1543`) only inspects the tail (`text.len() - needed`, where `needed = REPETITION_WINDOW × threshold = 200 × 3 = 600` bytes), so it is O(window × threshold) per call — but the buffer itself is never truncated.

**Impact:** For a legitimate long generation (e.g. 100K tokens ≈ 400 KB), the full response is held in memory for the whole stream. Bounded by one turn (dropped when the stream task ends), so not a leak — but since only the suffix is ever inspected, the buffer could be truncated to the last ~1.2 KB after each check.


## Strengths (verified)

- **Incremental token counting.** `TokenAccounting::update()` (`turn.rs:380`) BPE-encodes only messages appended since the previous count + a head swap when the system head changed. The old per-iteration full-history pass is gone. tiktoken BPE is `OnceLock`-cached (`context.rs:696`).
- **Memory recall off the async runtime.** FTS5 fetch + load in `spawn_blocking` (`memory/mod.rs:1311`); ONNX embedding inference in `spawn_blocking` (`embedder.rs:128`); SQLite writes in `spawn_blocking` (`:1254`). The scoring loop (`:1364`) is allocation-free per memory: `cosine` (`:870`) and `contains_ascii_ci` (`:916`) use iterators/byte compares; the only per-recall allocations are the one `to_ascii_lowercase()` (`:1340`) and the result `Vec`/`HashMap`. `FTS_CANDIDATE_POOL=50` (`:1293`) and `FULL_SCAN_FALLBACK_CAP=200` (`:1299`) bound the candidate set.
- **Per-turn auto-recall cache.** `turn.rs:300-301` reuses scored results when the latest user-message text is unchanged within a turn — no re-embedding or rescan on tool-result appends.
- **SSE streaming is allocation-light.** Both OpenAI (`openai.rs:851` `let mut buffer = String::new()` + `:938` `parse_sse_buffer(&mut buffer)` using `String::drain()`) and Anthropic (same pattern) reuse a single buffer with in-place drain — no O(n²) re-copy, no per-delta allocation. The stream→consumer bridge is a bounded `mpsc::channel(128)` (`openai.rs:822`) — real backpressure.
- **IPC delta batching is bounded.** `DeltaBatcher` (`src-tauri/src/ipc/events.rs:304`) caps each bucket at `DELTA_BUCKET_CAP` (64 KiB, `:271`) and flushes on a fixed `DELTA_FLUSH_INTERVAL` (16 ms, `:266`). The deadline is armed once per batch and never reset (`:439`), avoiding starvation. The forwarder locks the manager only briefly, never while waiting on `recv()`.
- **Trace log is bounded.** `LlmRequestLog` is a ring buffer (`MAX_RECORDS=32`) with a dedicated `std::thread` writer (`trace.rs:2260`); `append_response` (`:538`) caps the mirrored response and sets `response_truncated` (`:558`). No unbounded growth.
- **Context window is managed.** `compact_old_tool_results` (`context.rs:372`) truncates old tool results to a summary (idempotent); `summarize_with_interrupt` (`context.rs:216`) summarizes the oldest turns at the fill threshold. Context does not grow forever.
- **429 cross-provider fallback is sound.** `run_turn_attempt` (`runtime/agent.rs:282`) does at most one fallback per turn (`tried_fallback` latch, `:288`), no cascading, no backoff sleep on 429 — the 429 returns immediately and the fallback issues a fresh request to the alternate provider.
- **No lock held across `.await`.** Provider/context-manager `RwLock` reads are cloned and dropped before any await (`turn.rs:69-76`); the workflow `Mutex` is dropped in a scoped block before the fan-in send (`turn.rs:339-352`); the SQLite `Mutex` is held only inside `spawn_blocking`.
- **Frontend hot path is memoized.** `Message` is `React.memo`-wrapped with a custom `arePropsEqual` (`frontend/src/components/chat/Message.tsx:435`); the agent-event listener is a module-scope singleton (`useAgentEvents.ts`) with rAF-batched flushing (16 ms deadline, 64 KiB cap, hybrid flush). The reducer's per-flush string concat is an accepted O(n²)-amortized trade-off, documented in `agentEventReducer.ts:257`.

## Build times & dependency weight

- `Cargo.toml` has no `[features]` section — every dependency is always compiled (see L4). The heaviest: `fastembed` (ONNX Runtime + model download on first use), `chromiumoxide` (Chromium CDP), `tree-sitter` + two grammars.
- Good mitigations already in place: `[profile.dev]`/`[profile.test]` `debug = "line-tables-only"` (`Cargo.toml:100-104`) cuts link time; `reqwest` and `image` are `default-features = false` with targeted features; `vendor/tao` is excluded from the workspace (`:112`).
- `src-tauri/Cargo.toml` is lean (Tauri + plugins + `mnemo` path dep); the only heavy transitive weight comes through `mnemo`. The Windows-only `windows-sys` features are precisely scoped (`:43-50`).

## Summary

The performance posture is strong: the four hot paths the mandate called out (turn loop, SSE streaming, memory recall, IPC batching) are all correctly offloaded, cached, and bounded. The single MEDIUM (codegraph full-file re-read per pass) is the only finding with a non-trivial payoff and a clean fix (mtime pre-filter). The five LOWs are micro-optimizations and accepted trade-offs — none indicate a stall, leak, or unbounded-growth defect. Prior findings B1/B2/R1/R2/M1 are fully resolved; F1/F8 remain as documented/accepted.
