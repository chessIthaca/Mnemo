# UI Freeze Audit — Ranked Findings (2026-08-22)

Audit plan: 308b8490 — "Audit UI freeze candidates: locks, blocking I/O, wait conditions".
Scope: every code path reachable from the UI that can block the main thread or wait on a
lock/file/database/subprocess/channel; verify past freeze fixes are still in place.

## Executive summary

The freeze the user reported again today (14:31) is **the same recurring class as the
2026-08-22 morning hangs**: main thread blocked (0% CPU) for ~10 s during active agent
turns. Root cause is a **trace.rs lock-order inversion (AB-BA) plus sync trace IPC
commands blocking on those locks on the Tauri main thread**. The fix for both is **already
in the working tree** (`src/provider/trace.rs` + `src-tauri/src/ipc/trace.rs`, uncommitted)
and **fully verified** — but the running binary predates the fix, so the user still hits the
freeze. The immediate action is: rebuild + restart the app; commit the fix to a feature
branch (user has said "don't commit yet" — respect that).

All previously-fixed freeze items (F1/F2/F3/F6, R1/R2/R3, N1–N6) are **verified still in
place**; no regression found.

## Ranked findings

### HIGH-1 — trace.rs writer holds `log_path` across `records.lock()` + 8MiB file write (AB-BA inversion) — **fix in tree, verified, UNBUILT**
- **Where**: `src/provider/trace.rs` `writer_main` (~line 924). Pre-fix the edition-2021
  `if let Some(path) = shared.log_path.lock()...` scrutinee temporary kept the `log_path`
  `MutexGuard` alive across `records.lock()` and `write_records_to_file`.
- **Mechanism**: `with_record` (trace.rs:588) holds `records` while `maybe_log_error` takes
  `log_path` — the reverse order. Writer holds `log_path` + waits on `records`; with_record
  holds `records` + waits on `log_path` → deadlock. 12 watchdog reports in
  `.coding/logs/hang-*.txt` (8/21 14:06 → 8/22 14:31), every one: main thread 0.0% CPU
  (blocked, not busy), ~10s stall, during `agent1 Started` / `workflow → Executing`.
- **Fix (in tree)**: scoped path clones — the guard is dropped before `records.lock()` and
  before any I/O (both `log_path` and `error_log_path`).
- **Verification**: regression test `provider::trace::tests::writer_never_holds_log_path_while_waiting_for_records` — recorded failing pre-fix (plan e2ba07d8), **passes** on the fixed tree. Full suite: `cargo test` 1411 passed, `cargo test` (src-tauri) 144 passed, warning-free.
- **STATUS**: fix code-complete + verified but **uncommitted** (user chose "don't commit yet") and **the running `mnemo-app.exe` (built 13:36) predates the source fix (14:10/14:17)** — the 14:11/14:31 freezes came from the pre-fix binary. **Action: commit on a feature branch + rebuild + restart.**

### HIGH-2 — sync trace IPC commands run on the Tauri main thread and block on those locks
- **Where**: `src-tauri/src/ipc/trace.rs` — `list_llm_requests`, `clear_llm_requests`,
  `get_trace_logging` were `pub fn` (sync). Tauri v2 runs sync commands on the **main
  thread**, so any wait on the `records`/`log_path` mutexes (e.g. the writer's 8MiB
  rewrite, or the AB-BA deadlock above) freezes the UI.
- **Fix (in tree)**: all three converted to `async fn ... -> Result<_, IpcError>` with
  `spawn_blocking`. `get_llm_request` / `set_trace_logging` were already async (N1/N6).
- **Verification**: src-tauri crate compiles + 144 tests pass. Frontend call sites need no
  change (Tauri unwraps `Ok`).
- **STATUS**: same as HIGH-1 — fixed in tree, unbuilt, uncommitted.

### MEDIUM-3 — `std::sync::Mutex` on `agent_chat` webview handle is `lock()`ed on the main thread
- **Where**: `src-tauri/src/main.rs:200-202` — `*agent_chat_handle.lock()...` in `setup`.
  Setup is startup-only; the exit teardown already uses `try_lock` (main.rs:796). No
  mid-session lock on this mutex found. Low risk — retained as MEDIUM only because a
  future `lock()` on the main thread (e.g. a command) would reintroduce the class.

### LOW-1 — `deferred_rect` `std::sync::Mutex` in `browser_webview.rs`
- `try_apply_rect` uses `try_lock` (browser_webview.rs:328) — verified still in place (R3).
  Guard is held only for push/pop, never across I/O. Low.

### LOW-2 — `ActivityRing` mutex push on the event-forwarder hot path
- `events.rs:254` — one mutex push per state-changing event. 64-entry cap, never held
  across awaits. Low.

## Verified-still-fixed (no regression)

| Fix | Evidence |
|---|---|
| F1 git-branch poll off main thread | `ipc/files.rs:168, 863, 1143` — `get_git_branch`/git history/status all `spawn_blocking`/worker |
| F1b slower branch poll | 5s interval (frontend) |
| F2 trace-file cap | `trace.rs:54` (8MiB cap) |
| F3 unbounded transcript caps | `ipc/files.rs:531` (diff over-budget) |
| F6 event-forwarder deltas lock-free | `events.rs:311-321` — zero locks for delta events |
| R1 agent-chat `set_size` off the main thread | `main.rs:216-228` — latest-wins worker thread |
| R2 rAF event buffering + 40ms fallback | `frontend/src/hooks/deltaFlush.ts` + `useAgentEvents.ts` (hybrid flush) |
| R3 webview rect drop-not-queue | `browser_webview.rs:327-346` (`try_lock` + deferred rect) |
| N1 get_llm_request async | `ipc/trace.rs:49` |
| N2-N6 (fs in async commands, error-log rot, corpus_digest, set_trace_logging) | verified async + `spawn_blocking` |
| Memory/codegraph progress throttling | `memory_maintenance.rs:161-169` (1-in-50) |

## Watchdog evidence

All 12 hang-*.txt reports: `stall at fire: ~10006-12271 ms`, `main thread cpu: 0.0% busy`.
10 of 12 are exactly ~10s (the stall threshold) — i.e. the main thread was **blocked waiting
on something that would have resolved, then recovered**. Consistent with the AB-BA deadlock
window (the other side eventually released) rather than a hard spin.

## Recommended actions

1. **Rebuild + restart the app** with the current tree — the freeze fix is in source but
   not in the running binary. (Blocked on user's "don't commit yet" — the build needs the
   app closed because the exe is locked.)
2. When the user allows: create `fix/trace-freeze`, commit `src/provider/trace.rs` +
   `src-tauri/src/ipc/trace.rs` (regression test included), merge to main, and supersede
   the BUG: memory with the commit pointer.
3. Optional: switch `agent_chat_handle` to `try_lock` in setup (currently only exit path
   uses it).
4. Optional: watchdog threshold is 10s — the ~10s episodes are the deadlock self-recovering;
   consider lowering to 5s for earlier capture, or keep 10s (above legitimate jank).

## Files changed (uncommitted)
- `src/provider/trace.rs` (+139: scoped path clones + regression test + invariant docs)
- `src-tauri/src/ipc/trace.rs` (+41: 3 commands async + spawn_blocking)
