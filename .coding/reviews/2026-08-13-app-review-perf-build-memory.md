# App Review — Runtime Performance, Build-System Efficiency, Memory Efficiency

**Perspective:** Performance / Build / Memory (read-only)
**Reviewer:** read-only review subagent
**Scope:** whole codebase at HEAD (`src/`, `src-tauri/`, `frontend/`, Cargo workspace, vite config). No code executed — findings are reasoned from source.

## Verdict

The codebase is in **good shape overall** — the hot paths show clear evidence of prior optimization passes (rAF-batched streaming, FTS5 pre-filtered recall, incremental token counting, lock-free CDP round-trips, capped ring buffers). The most significant findings are **build-system**: two heavyweight crates (`syntect`, `markdown`) are declared as dependencies but **never used** anywhere in the Rust source, adding real compile time and binary bloat for zero benefit. Runtime-perf issues are mostly Medium/Low; the browser lock discipline and frontend streaming batching are correctly done. Memory is well-bounded everywhere I checked.

---

## CRITICAL

**No Critical findings.** Nothing blocks the runtime per turn, no unbounded growth, no deadlock, no correctness-adjacent perf bug.

---

## HIGH

### Build

- **B1 — `syntect` dependency is declared but never used (dead heavyweight dep).**
  `Cargo.toml:20` declares `syntect = "5"`. A repo-wide search for `syntect` / `syntect::` / `use syntect` / `SyntaxSet` / `HighlightLines` across all of `src/**/*.rs` returns **zero matches**. Syntax highlighting is done in the *frontend* via `rehype-highlight` (`frontend/src/components/chat/Message.tsx:110`). syntect pulls in a large `SyntaxSet` (embedded grammar definitions) and `onig`/regex machinery — one of the heavier pure-Rust deps in the tree. Impact: adds tens of seconds to a clean `cargo build`/`cargo test`, inflates incremental rebuild surface, and bloats the binary with unused grammar tables.
  **Fix:** delete `syntect = "5"` from the root `Cargo.toml` (and run `cargo update` to drop it from the lockfile).

- **B2 — `markdown` dependency is declared but never used (dead dep).**
  `Cargo.toml:48` declares `markdown = "1"` with the comment "used by the lib's markdown module" — but there is **no markdown module** (`pub mod markdown` / `mod markdown` / `markdown::to_html` all return zero matches). Markdown *parsing* for plans is hand-rolled in `src/workflow/plan_file.rs`; markdown *rendering* is in the frontend (`react-markdown`). The only `markdown` hits in Rust are the unrelated `[markdown]` config section (`MarkdownConfig`). Impact: smaller than syntect but still pure dead weight on every build.
  **Fix:** delete `markdown = "1"` from the root `Cargo.toml`.

---

## MEDIUM

### Runtime performance

- **R1 — `cl100k_base()` BPE tokenizer is re-initialized on every `count_tokens` call.**
  `src/agent/context.rs:246` — `let bpe = cl100k_base().ok()?;` inside `try_tiktoken_count`, which is invoked from `ContextManager::count_tokens` → called from `src/agent/turn.rs:142,154,163,217`. `cl100k_base()` decodes/loads the embedded BPE rank table each time; it is not a cheap accessor. tiktoken-rs does not cache the result across calls at this call site. Although `turn.rs` already reduces *how often* a full exact count runs (incremental char-based reuse between tool iterations, Perf H2), the full-count path still fires on every summarize and every large (>8 KiB) tool-result append — and re-builds the tokenizer each time.
  Impact: adds a few ms of redundant CPU per exact count; on a long turn with many large tool outputs this repeats. Not runtime-blocking (it's CPU, off the I/O path) but pure waste.
  **Fix:** cache the `CoreBPE` in a `once_cell::sync::Lazy` / `std::sync::OnceLock` so the table is decoded once per process.

- **R2 — `MemoryStore` runs rusqlite calls on the async runtime without `spawn_blocking`.**
  `src/memory/mod.rs` — every `conn.lock().await` is followed by synchronous rusqlite `prepare`/`query`/`execute` on the tokio executor thread (e.g. `recall` at :522, `write` at :502, `batch_access` at :626, the stats aggregations at :743–861). These are blocking SQLite calls. For a small per-project DB (hundreds–low-thousands of rows, the stated design envelope) each is sub-millisecond, so this is **not** a practical stall today. But `recall` loads up to 50 rows × 768-dim embedding BLOBs and runs an O(K) cosine pass, and the `project_stats`/`session_list` aggregations scan `request_stats` — all while holding a tokio worker thread. Under a large store or many concurrent agents this could starve the single-threaded-per-core runtime.
  Impact: potential tail-latency on the async runtime as the store grows; low today.
  **Fix:** wrap the blocking `recall`/stats bodies in `tokio::task::spawn_blocking` (the `Connection` is `Send`), or document the small-DB assumption as a hard invariant with a size cap. (Contrast: `describe_image` already correctly uses `spawn_blocking`, `src/tool/agent/describe_image.rs:107`.)

### Memory

- **M1 — `sweep_stale_profiles` does a blocking `std::fs::read_dir` + `remove_dir_all` of the temp dir on every browser (re)spawn.**
  `src/browser/mod.rs:201-220`, called from `ensure_browser` (:168) which runs while the caller holds the state `Mutex` (`navigate` at :237). `read_dir(std::env::temp_dir())` scans the *entire* system temp directory synchronously, and `remove_dir_all` of stale profiles is recursive deletion — both on the async executor and both under the manager lock. This fires once per spawn/respawn (not per operation), so it's bounded, but a large temp dir makes a browser relaunch stall the lock.
  Impact: brief pause on browser respawn only; the temp-dir scan cost scales with unrelated temp files.
  **Fix:** move the sweep to `spawn_blocking` and/or run it outside the state lock (it touches no shared state).

---

## LOW

### Runtime performance

- **R3 — `navigate` holds the state lock across `ensure_browser`, which `.await`s `Browser::launch`.**
  `src/browser/mod.rs:235-239` — the lock is held across `Browser::launch(...).await` (a process spawn + CDP handshake, potentially hundreds of ms). The module doc (:23-26) and most ops correctly drop the lock before CDP round-trips, but the *launch itself* runs under the lock. Only first-use / respawn, and there's exactly one browser, so contention is theoretical (another op would just wait).
  **Fix:** acceptable as-is; if respawn latency ever matters, gate launch with a dedicated `OnceCell`/spawn mutex instead of the state lock. Low priority.

- **R4 — screenshot path base64-encodes a PNG inline on each Browser-tab refresh (manual pull).**
  `src-tauri/src/ipc/browser.rs:67-69` encodes the full PNG to base64 on the async thread per refresh. Refresh is user-initiated (button / page-select), **not** a polled interval (verified: `BrowserView.tsx` has no `setInterval` for screenshots — only manual refresh + live console push), so this is not a hot loop. base64 inflates payload ~33% but it's local IPC.
  **Fix:** none needed; only revisit if screenshots ever move to a polling timer.

### Build

- **B3 — `reqwest` enables `gzip`, `brotli`, `deflate` features for an LLM/embeddings client.**
  `Cargo.toml:51`. The lib's own comment says reqwest is "HTTP for embeddings fallback (Ollama /api/embed)"; the OpenAI client streams SSE (`stream` feature is genuinely needed). gzip/brotli/deflate pull in `flate2`, `brotli`, `miniz_oxide` etc. LLM providers content-encode responses rarely, and SSE streams are usually identity/plain. These decoders are dead weight for the actual traffic.
  Impact: moderate extra compile time + binary size; no runtime cost.
  **Fix:** audit whether any endpoint actually serves compressed bodies; if not, drop to `["json", "rustls-tls", "stream"]`.

- **B4 — Frontend is a single ~1 MB+ bundle; `chunkSizeWarningLimit: 1500` suppresses the warning instead of splitting.**
  `frontend/vite.config.ts:20-24`. The comment justifies it (Tauri loads locally, no network), which is reasonable for load time. But `rehype-highlight` + `react-markdown` + `remark-gfm` + `lucide-react` in the entry chunk means the whole thing parses/evaluates on startup. For a desktop app this is a one-time startup cost, not per-interaction.
  Impact: slower first paint only; acceptable for a Tauri shell.
  **Fix:** optional — lazy-load the markdown/highlight stack behind the first assistant message render to trim startup parse time. Low value.

### Memory

- **M2 — `streamingText` accumulation is a `String` re-allocated per rAF flush.**
  `frontend/src/hooks/agentEventReducer.ts:130` — `streamingText: agent.streamingText + event.text` on each flush, and `useAgentStore.ts:855` same pattern. This is O(n²) in total bytes copied for a long response (each concat copies the whole accumulated string). Mitigated by rAF batching (one concat per frame, ~60/s, not per token) and by JS engine string-rope optimizations, so in practice a multi-KB response stays cheap. Worth noting only because it's the single hottest allocation path in the frontend.
  Impact: negligible in practice due to batching + ropes.
  **Fix:** none required; if profiling ever shows it, accumulate into an array and `join` once per flush. Micro-opt.

---

## Areas explicitly checked and found CLEAN (no findings)

- **Browser state-lock hygiene (`src/browser/mod.rs`):** Correct. CDP round-trips (navigate/screenshot/eval/click/type/url/title) all run on cloned `Page`/`Arc<Browser>` handles *outside* the `Mutex` (see `lookup` :500, `list_pages` :294, `screenshot` :371, `eval` :418). The lock is held only for short map mutations. Doc at :23-26 accurately describes this. Only the spawn-under-lock (R3) and temp-sweep (M1) caveats.
- **Console event forwarding:** Console ring buffer is capped (`CONSOLE_CAP = 500`, :80); the broadcast channel is bounded (512) and lagging subscribers are dropped, not blocked (`record_console` :637-639, forwarder `Lagged(_) => continue` in `ipc/browser.rs:38`). Per-page forwarder tasks are tracked in `console_tasks` and aborted on close/respawn (:156, :349, :472). No leak.
- **Frontend streaming batching (`useAgentEvents.ts`):** text/reasoning/arg deltas are buffered and flushed once per animation frame (`flushBuffers` :136, `scheduleFlush` :178) — one store update per frame, not per token. Scroll is throttled to 100 ms (`Conversation.tsx:52-75`). Streaming renders plain text, markdown parsed once on finalize (`Message.tsx:91-104`). Correctly done.
- **Per-turn token counting (`turn.rs:129-167`):** uses incremental char-based reuse between tool iterations and only recounts exactly after a summarize or a >8 KiB append — a deliberate, sound optimization (Perf H2). The only inefficiency is the per-call tokenizer init (R1), not the call frequency.
- **Memory recall (`memory/mod.rs:522-610`):** FTS5 pre-filter caps the cosine pass to a 50-row candidate pool (down from O(N)); full-scan fallback capped at 200 rows; allocation-free ASCII-CI keyword boost (`contains_ascii_ci` :271); `batch_access` collapses K access-bump queries into one (:626). Read/write connections split under WAL so reads don't serialize behind writes (:103-116). Well optimized.
- **Unbounded growth / ring buffers:** trace log is a bounded ring (`MAX_RECORDS = 32`, `MAX_RAW_RESPONSE_BYTES = 2 MiB`, `provider/trace.rs:33,38`); console capped (above); context growth is managed by `summarize_at_fill_rate` summarization (`context.rs`); browser temp profiles are swept on respawn and removed on close with a retry loop (:483-493). Backlog store is a bounded user-managed list persisted atomically. No unbounded growth found.
- **Spawned-task leaks:** agent loops removed from `agent_loops` on `Exited` (forwarder, `ipc/events.rs`); console forwarders aborted on close; `cleanup_inactive_subagents` cancels stale subagents on a fresh plan (:194). Clean.
- **Process spawning:** `shell` and `git` tools correctly use `tokio::process::Command` (async, non-blocking) with `kill_on_drop` + timeout (`shell.rs:142-174`), not `std::process::Command` on the runtime. The `std::process::Command` hits in `git.rs:407-588` are all in `#[cfg(test)]` helpers. Clean.
- **Async Mutex / lock ordering (`ipc/state.rs`):** documented lock-ordering invariant (`manager` before `agent_loops`, :39-44); the event forwarder locks the manager only briefly and *never* while blocked on `recv()` (:228-257). Delta events take zero locks. No contention hazard found.

---

## Summary of recommended actions (impact-ordered)

1. **Remove `syntect` and `markdown` from `Cargo.toml`** (B1, B2) — dead heavyweight deps; biggest, cheapest build win.
2. **Audit `reqwest` compression features** (B3) — drop `gzip`/`brotli`/`deflate` if no endpoint uses them.
3. **Cache the tiktoken BPE tokenizer** in a `OnceLock`/`Lazy` (R1) — stop re-decoding the rank table per exact count.
4. **Move memory-store blocking calls to `spawn_blocking`** (R2) — future-proofs the async runtime as the store grows.
5. **Move the browser temp-profile sweep off the locked async path** (M1) — `spawn_blocking`, outside the state lock.

Everything else is Low or already well-handled.
