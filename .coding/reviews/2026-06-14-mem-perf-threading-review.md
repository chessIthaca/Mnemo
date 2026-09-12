# Full review: memory leaks, performance, threading — 2026-06-14

**Scope:** Rust backend (`src/`, `src-tauri/`) + TS frontend (`frontend/src/`), audited for memory leaks (unbounded collections, un-retained tasks, listener/interval leaks), performance (blocking I/O on async/main threads, unbounded re-renders, poll cadence, I/O amplification), and threading (lock-across-await, lock ordering, detached tasks, channel backpressure). Every claim cites code read in-session at HEAD.

**Method:** started from the spawn/channel/lock surface (all `tokio::spawn`, `std::thread`, `spawn_blocking`, `Mutex<HashMap>` sites), then deep-read the runtime manager, agent lifecycle, provider/trace/browser/memory subsystems, IPC layer + forwarder, and frontend store/reducer/effects. Cross-checked every 2026-04-19 freeze-diagnosis finding against HEAD so fixed items are not re-reported.

---

## Summary

| # | Severity | Area | Finding |
|---|----------|------|---------|
| N1 | **Medium** | perf | Trace tab re-fetches up to 2 MiB raw response every 1.5 s, forever, via a sync (main-thread) command |
| N2 | **Low-Med** | threading | Sync `std::fs` I/O inside async IPC commands parks tokio workers (read/save/load conversation) |
| N3 | **Low** | memory | `error_logged_ids` dedup set grows one u64 per failed request, never pruned |
| N4 | **Low** | perf/disk | `provider-errors.jsonl` is append-only with no cap/rotation (traces.jsonl got one; the error log didn't) |
| N5 | **Low** | threading | `corpus_digest()` does sync file reads on the async runtime at every agent exit |
| N6 | **Low** | threading | `set_trace_logging(false)` blocks the sync command until the writer drains (up to an 8 MiB write) |

No High findings. All five previously-reported leak/freeze vectors (F1, F1b, F2, F3, F6 of the 2026-04-19 diagnosis) are **verified fixed at HEAD** — see bottom section.

---

## N1 — MEDIUM (performance / UI jank) — Trace-tab detail poll never stops, and each fetch is heavy

**Where:** `frontend/src/components/views/LlmTraceView.tsx:609-635` (detail `useEffect`, `POLL_MS = 1500` at :554) + compare-mode twin at :640-658; backend `src-tauri/src/ipc/trace.rs:26-28` (`get_llm_request`).

**Mechanism:** when a trace row is selected, the view re-invokes `get_llm_request(selectedId)` every 1.5 s. The stated reason ("the raw response keeps growing while a request is streaming") only holds while the request is in flight — but the interval runs **indefinitely** for as long as the row stays selected and the Trace tab is mounted. Each call returns the full record including `response_raw` (capped at **2 MiB**, `trace.rs:57`), which means: clone of the record + serde serialization + IPC transfer + React state replace of up to ~2 MiB every 1.5 s (~1.3 MB/s sustained), doubled when compare mode is on. Additionally, `get_llm_request` is a **non-async** Tauri command, so the clone + serialize runs on the main thread — recurring main-thread work proportional to response size is a textbook jank source. (`list_llm_requests` is fine — 32 small summaries.)

**Fix (small):** stop the poll once the record is terminal — `finish_reason != null || error != null` and the last two polls returned identical content (or add an `is_complete` flag to `LlmRequestDetail` and clear the interval when set). Optionally make `get_llm_request` `async` so the clone lands on the runtime pool like the rest of the heavy commands.

---

## N2 — LOW-MEDIUM (threading) — sync `std::fs` I/O directly inside async commands

**Where:** `src-tauri/src/ipc/files.rs:30` (`read_file`), `:311` (`browse_markdown_file` post-dialog read), `:405-410` (`save_conversation` create_dir_all + write), `:428` (`load_conversation`).

**Mechanism:** these are `async fn` commands executing `std::fs::read_to_string` / `write` inline — the exact class F1 belonged to (blocking on a tokio worker), just without a subprocess. For small `.md` reads the park is negligible, but `save_conversation` / `load_conversation` serialize/parse the **entire transcript JSON** (up to 1000 entries with tool payloads) — a multi-MB synchronous write/read on the runtime. The codebase's own convention (`git_ops.rs`, all `tool/agent/*` file tools, `browse_markdown_file`'s dialog) is `spawn_blocking`; these four sites predate or bypass it.

**Fix (mechanical):** wrap each fs call in `tokio::task::spawn_blocking` (paths are already owned/validated before the call). Mirror the doc-comment style of `get_git_branch` (files.rs:354-359).

---

## N3 — LOW (memory) — `error_logged_ids` grows without bound

**Where:** `src/provider/trace.rs:199` (decl), `:452-461` (insert in `maybe_log_error`). No removal or clear site exists anywhere in the file.

**Mechanism:** every request that reaches a terminal error state inserts its id into the `error_logged_ids` HashSet (dedup so a status-failure followed by a stream-error logs once). Ids are monotonically increasing and never reused, so the set grows by one u64 (+ hash node overhead) per failed LLM request for the **lifetime of the process**. Tiny per entry — but it is a true unbounded collection, and the codebase otherwise prunes every dedup structure (`notified_children`, `prev_workflow_state` on `Exited`, events.rs:336-337). A pathological endpoint-retry loop (e.g. the historical "stream aborted — consumer dropped" spam) grows it fastest.

**Fix (tiny):** bound it — e.g. `if seen.len() > 10_000 { seen.clear(); }` before insert (correctness impact of a rare re-log is nil: the append is idempotent-ish diagnostics), or store the last N ids in a `VecDeque`.

---

## N4 — LOW (performance / disk) — `provider-errors.jsonl` has no rotation

**Where:** `src-tauri/src/.../trace.rs` `writer_main` append path (:651-676); `set_error_log_path` (:504-516) documents it as **always-on**.

**Mechanism:** the F2 fix capped `traces.jsonl` at 8 MiB (`MAX_TRACE_FILE_BYTES`, :52, enforced at :744), but the always-on provider-error log — one compact JSON line per failed request, no toggle — is opened `create + append` and never truncated or rotated. On a long-lived install with a flaky gateway this file grows indefinitely (disk, not RAM; the 84 MB `traces.jsonl` of the freeze diagnosis showed how fast log files grow here once something misbehaves).

**Fix (small):** same over-cap fresh-start rule as traces.jsonl — check `metadata().len()` before append; if over (say) 8 MiB, truncate once. One line summary per failure makes rotation cheap.

---

## N5 — LOW (threading) — `corpus_digest` sync reads on the async runtime at agent exit

**Where:** `src/runtime/agent.rs:444-470` (`spawn_consolidation`): `corpus_digest(...)` is called **before** `tokio::spawn`, i.e. synchronously on the agent task's async context. It reads up to 2×`CORPUS_MAX_FILES_PER_DIR` (20) files × 1500 chars + dir mtimes (`consolidation.rs:19-28`).

**Mechanism:** same class as N2 — sync fs on a tokio worker. Bounded (≤ ~30 files) and only at agent exit, so impact is small; flagged because `spawn_consolidation`'s own doc says "Never blocks the caller", and on a slow/locked disk (AV scan on `.coding/plans`) the exit path can park a worker. 

**Fix (one-liner):** move the `corpus_digest` call inside the spawned task.

---

## N6 — LOW (threading) — logging-toggle command can block on the writer thread

**Where:** `src-tauri/src/ipc/trace.rs:46-48` (`set_trace_logging`, sync command) → `trace.rs:522-527` (`set_logging_enabled(false)` calls `flush_file_writes`) → `:555-567` (blocking `ack_rx.recv()`).

**Mechanism:** unchecking "Log to file" blocks the command until the background writer drains its queue — which may be mid-rewrite of a file up to 8 MiB (`write_records_to_file` reads + rewrites the whole file below cap). As a sync command this runs on the main thread; a slow disk makes the checkbox visibly hitch. (The `RunEvent::Exit` call site, main.rs:539-547, is correct and must stay blocking.)

**Fix (small):** make `set_trace_logging` an async command and await the flush via a oneshot, or fire-and-forget the flush (the writer already checks `log_enabled` at drain time — the flush only orders the file-finality guarantee).

---

## Verified clean (ruled out — checked in-session, do not re-report)

- **F1 FIXED:** `get_git_branch` clones root → drops lock → `spawn_blocking` (`files.rs:361-367`); env-hardened `read_git_branch` (:329-352).
- **F1b FIXED:** git poll is 60 s + window-focus + turn-end edge (`App.tsx:304-341`, `didMainTurnEnd` in agentState.ts:329-338) — no more 5 s subprocess churn.
- **F2 FIXED:** `traces.jsonl` capped at 8 MiB with over-cap fresh-start (`trace.rs:52, :744-748`); ring `MAX_RECORDS=32`; raw response capped 2 MiB with truncation flag (:57, :340-362); writer coalesces + dedupes per batch (:580-610). Residual (documented in-code): below-cap wakeups still rewrite the whole file (≤ ~8 MiB I/O per batch) — acceptable, bounded.
- **F3 FIXED:** transcript capped 1000, activityLog 300 (`agentState.ts:349-364`, applied at all 7 append sites in the reducer/store), planDiffs 100, toolOutputLog 50; `Message` rows memoized with custom equality (`Message.tsx:227`); deltas rAF-batched (`useAgentEvents.ts:1-26`); tradeoff (exports lose pre-cap history) documented in-code.
- **F6 FIXED:** `notified_children` + `prev_workflow_state` removed on `Exited` (`events.rs:336-337`); agent also removed from manager + `agent_loops` map (:329-331).
- **Runtime lifecycle:** every agent task emits `Exited` on all termination paths; fire-and-forget spawns (turn.rs:697 stats, :1283 plan-memory; runtime/agent.rs:460 consolidation) hold only `Arc` clones and terminate. `AgentHandle` is lock-light (atomic flag); manager map is `remove()`d on `Exited`.
- **Channels:** all hot channels bounded (`mpsc::channel(8/64/128)`); manager `send` uses `try_send` so a full inbox surfaces an IPC error instead of blocking; fan-in receiver taken by the forwarder so it never holds the manager lock while waiting.
- **Forwarder locking:** lock taken at most once per structural event, zero times for deltas; `notify_parent_on_completion` clones under the lock and sends after drop (:424-462); `cleanup_inactive_subagents` sends `try_send` only while holding the lock (:543-555). Lock-ordering invariant (manager → agent_loops) documented and held (`state.rs:38-43`).
- **Browser manager:** pages/console/console_tasks pruned on close + zombie-prune in `list_pages` (`browser/mod.rs:393-403, 413-435`); console ring capped 500; CDP connect retries outside the lock; profile dirs removed on a retry thread (bounded 15 s) because tokio tasks die at runtime shutdown; headless Chromium + both child WebView2s torn down on `RunEvent::Exit` (main.rs:539-579) — no orphan HWNDs/processes.
- **SSE stream task:** detached but bounded — `tx.send` failure (consumer dropped) returns; stalled reads bounded by `READ_TIMEOUT` 90 s (openai.rs:79, 585-614); SSE buffer drained in place (no O(n²) re-copy); trace mirroring failure-tolerant.
- **Memory subsystem:** all rusqlite access on `spawn_blocking`; ONNX inference on `spawn_blocking` (embedder.rs:206); tool-event capture capped 500 chars; recall FTS-capped.
- **Frontend teardown:** every `setInterval`/`addEventListener` reviewed has a matching cleanup (App.tsx theme/mq:126/275, focus:326/330, geometry:472/479, drag:646-656; GraphView polls only while indexing; LlmTraceView/MemoryDebugView/PlanProgress clear on unmount; MdViewer, StatusBar, EndpointCard, InflightBar, ApprovalPrompt all remove). Right panel mounts **only** the active tab (RightPanel.tsx:114-120), so inactive views don't poll. `nameResolutionInFlight` Set is deleted in `.finally()` (useAgentEvents.ts:198). Store persistence writes only on commit (no per-pointermove localStorage). Agent maps cleaned on `exited` (agentEventReducer.ts:863-881).
- **Model-resolver provider cache:** bounded by (endpoint, model) pairs, invalidated wholesale on `set_config` (model_resolver.rs:141-171).

---

## Recommended fix order (smallest-first)

1. **N3** bound `error_logged_ids` (~3 LOC) and **N4** truncate `provider-errors.jsonl` over cap (~5 LOC) — one PR, closes the two remaining unbounded-growth items.
2. **N1** stop the trace-detail poll on terminal records (+ `async` command) — the only user-perceivable one.
3. **N2** `spawn_blocking` for the four fs-IPC sites �� mechanical, matches existing convention.
4. **N5 + N6** move `corpus_digest` into the spawn; async-ify `set_trace_logging` — polish.
