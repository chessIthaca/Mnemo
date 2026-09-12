# Freeze / progressive-slowdown diagnosis — 2026-04-19

**Symptoms (user-reported, repeated):** the app freezes mid-session ("crashed again, or really the app freezes"); reasoning gets slower over time within a session. Trace file-logging was **OFF** during the freezes.

**Method:** static audit of unbounded in-memory collections, blocking calls on the async runtime, channel backpressure, frontend state growth, and process/disk evidence gathered live at HEAD. Every claim below cites the code read in-session.

---

## Live evidence (captured during diagnosis)

- `myharness-app` process: **898 MB working set / 1024 MB private** — the main Rust process itself is the largest consumer (each child WebView2/Chromium renderer is only 50–300 MB).
- `.coding/logs/traces.jsonl` = **84.5 MB**, `.coding/memory.db` = **70.8 MB**.
- `provider-errors.jsonl` tail: repeated `glm-5.3` `"stream aborted — consumer dropped"` — turns being aborted mid-stream (consistent with stall→interrupt or error-retry loops).

---

## Ranked findings

### F1 — HIGH — Blocking `git` subprocess on the async runtime every 5 s (freeze)

**Where:** `frontend/src/App.tsx:317` polls `getGitBranch()` on a 5000 ms `setInterval` **and** on every window-focus event. Handler: `src-tauri/src/ipc/files.rs:287-304`.

**Mechanism:** `get_git_branch` does
1. `state.project.root.lock().await` — takes the shared project lock, then
2. `std::process::Command::output()` — a **synchronous, blocking** subprocess call executed directly on the tokio runtime thread, **while still holding the project lock**.

If the spawned `git rev-parse` ever stalls (credential/GPG prompt, antivirus scan, slow/weak disk, locked `.git`), one tokio worker thread blocks. tokio's multi-thread scheduler does not preempt a blocked worker; if enough workers park on this, **all async work stops — including the event forwarder that drives the UI** → the app appears frozen. Even a *fast* git call holds the project lock for its duration, so concurrent IPC that needs `project.root` queues behind it every 5 s.

This is the same class of bug as the 2026-04-18 deep-review A1 forwarder stall (already fixed for run-all git via `spawn_blocking` + clone-root-first at `run_all.rs:213-216` / `git_ops.rs:55,86,110`). `get_git_branch` is the **one remaining** blocking-git call outside `spawn_blocking`.

**Fix (small, mirrors the A1/A4 pattern):**
1. Clone the root and drop the lock *before* spawning: `let root = state.project.root.lock().await.root.clone();`
2. Wrap `Command::output()` in `tokio::task::spawn_blocking`.
3. (Optional but recommended) replace the 5 s poll with an event-driven refresh (git watcher / refresh-on-turn-end) to stop paying the cost when nothing changed.

---

### F3 — HIGH — Unbounded frontend transcript + per-delta full spread (progressive slowdown)

**Where:** `frontend/src/hooks/agentState.ts:337-340` (`pushTranscriptEntry` spreads + appends, **no cap**); `frontend/src/hooks/agentEventReducer.ts:137-139` (`reduceTextDelta` does `{...agent, streamingText: agent.streamingText + text}` on every rAF flush). Only `toolOutputLog` is capped (`.slice(-50)`, `agentEventReducer.ts:856`); `transcript` and `activityLog` are **not**.

**Mechanism:** a long session accumulates one transcript entry per tool call / steer / error / skill / reasoning block. Every streaming delta then shallow-copies the agent object — and React re-renders the conversation — over a list that grows without bound. Cost per token rises linearly with session length ⇒ *"reasoning gets slower over time."* This is a UI-responsiveness slowdown, not an LLM prefill slowdown (the backend context IS managed — see F5 below).

**Fix:**
1. Cap `transcript` and `activityLog` (e.g. keep last N entries, same pattern as the existing `toolOutputLog` `.slice(-50)`), or virtualize the conversation list.
2. Memoize transcript-entry components so a delta doesn't re-render the whole history.

---

### F2 — MEDIUM — `traces.jsonl` is unbounded and fully rewritten per batch (real bug, NOT the freeze cause)

**Where:** `src/provider/trace.rs:690` (documented *"The file is unbounded… no rotation"*); writer thread `writer_main` at `trace.rs:572-675`; `write_records_to_file` at `trace.rs:700-759`.

**Mechanism:** with "Log to file" ON, every streamed chunk enqueues a `Msg::Dirty` over an **unbounded** `std::sync::mpsc` channel (`trace.rs:258`), and each writer wakeup **reads + rewrites the entire file** to upsert the dirty ids. An 84 MB file rewritten per batch is quadratic disk I/O, and the unbounded channel backlog grows in RAM while the writer grinds. The in-memory ring itself is fine (capped at `MAX_RECORDS = 32`).

**Status:** user confirmed logging was OFF during the freezes — so this did **not** cause them, but it's worth fixing.

**Fix:** rotate/truncate `traces.jsonl` (cap at N MB or N lines) and/or make the mirror append-only for new ids instead of whole-file rewrite.

---

### F6 — LOW — Minor known leaks (not freeze-worthy)

- `prev_workflow_state` HashMap in the event forwarder (`src-tauri/src/ipc/events.rs:161`) keeps one entry per dead agent id — grows by one per subagent, ~1 byte + node overhead each (previously flagged 2026-04-04).
- Native child WebView2 is a **single** labeled instance (`CHILD_WEBVIEW_LABEL`, `browser_webview.rs:40`) reused across navigations and torn down on `RunEvent::Exit` — **not** a leak.

---

## Ruled out (verified bounded — not suspects)

- Browser console ring: `CONSOLE_CAP = 500`/page; pages/console/console_tasks cleaned on `close_page` and pruned in `list_pages` (`src/browser/mod.rs:85,393-435,1140`).
- Working-memory tool events capped at 500 chars (`src/memory/mod.rs:84,1122-1128`).
- All rusqlite access via `spawn_blocking` on separate read/write connections (`src/memory/mod.rs`).
- Bundled ONNX embedder inference on `spawn_blocking` (`src/memory/embedder.rs:206`).
- Memory recall is FTS-capped (50 candidates; 200-row fallback) — not a per-turn full-table scan.
- 2026-04-18 A1/A4 forwarder git stall: **fixed at HEAD** (clone-root-before-checkpoint + `spawn_blocking` for checkpoint/commit/rollback).

### Backend context IS managed (so slowdown isn't prompt growth)
`ContextManager` summarizes at `max_tokens × fill_rate` (`context.rs:61-67`), keeping `keep_recent = 6` (`turn.rs:197-206`). Prompt size is bounded, so the *backend* prefill doesn't grow without limit. The perceived reasoning slowdown is the **frontend** re-render cost (F3) and, between summarize trips, the ordinary within-turn prompt growth (token count is a chars/4 heuristic, `turn.rs:155-188`).

---

## Recommended fix plan (smallest-first, independently shippable)

| # | Finding | Fix | Size |
|---|---------|-----|------|
| 1 | **F1** | `get_git_branch`: clone root → drop lock → `spawn_blocking(Command::output)` | ~10 LOC |
| 2 | **F3** | Cap `transcript` + `activityLog` (keep-last-N, mirroring `toolOutputLog`); memoize entries | ~30 LOC |
| 3 | **F1b** | Replace 5 s git-branch poll with event-driven refresh | ~40 LOC |
| 4 | **F2** | Rotate/truncate `traces.jsonl`; append-only mirror for new ids | ~60 LOC |
| 5 | **F6** | `prev_workflow_state.remove(agent_id)` on `Exited` | ~3 LOC |

**Do #1 first** — it's the only remaining blocking-subprocess-on-runtime call and the most credible freeze vector.
