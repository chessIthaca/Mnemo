# Plan: Mnemo2 logs every popup and frontend error to a file

## Goal
Every message Mnemo2 pops up (toasts), and every uncaught frontend error, is appended to a log file with time, level and context, so a popup that vanished before the user could read it can be looked up afterwards by the user or the lead.

## Kind
implementation

## Context
Why: the user saw an error popup in Mnemo2, could not read it before it disappeared, and the lead could not find it anywhere: Mnemo2 keeps no record of popups. "lets add a task that error popups are logged to a log file. For now this error is lost."

Verified facts:
- verified: every popup goes through one helper, `withToast(state, message)` in `mnemo2/src/lib/store.ts` (line ~355), called from 44 places; it stores `{ id, message }` in `AppState.toast` (`mnemo2/src/lib/types.ts` ~line 638), and `mnemo2/src/components/Toast.svelte` auto-dismisses after 4200 ms (line ~11) with a dismiss button.
- verified: error paths such as `noteTeamError` feed `withToast`; there is no level field, so errors and confirmations look the same to the log.
- verified: `<project>/.coding/logs/` is the existing log folder (provider-errors.jsonl, traces.jsonl, emit-diag.log) and is gitignored by Mnemo2's scaffold (`src-tauri/src/mnemo2/runtime.rs` ~line 3404 writes `.coding/logs/` into .gitignore).

Decisions:
- `withToast(state, message, level = "info")` gains a level (`"info" | "error"`); every error call site (catch blocks, `noteTeamError`, failed saves) passes "error". Audit all 44 call sites and say in the report how many are errors.
- New Tauri command `ui_log(entries)` in a NEW file `src-tauri/src/mnemo2/ui_log.rs`, registered in `src-tauri/src/mnemo2/mod.rs` at the END of the invoke_handler list: appends JSONL `{ts, level, source, message, route, worker_id?, detail?}` to `<project>/.coding/logs/mnemo2-ui.jsonl` when a project is open, else `<config_dir>/logs/mnemo2-ui.jsonl`; size-capped with one rotation (e.g. 2 MB → `.1`). Never throws back into the UI: a failed write is swallowed.
- Frontend `lib/uiLog.ts`: `logUi(level, message, extra?)` batches and sends asynchronously (never blocks the UI); `withToast` calls it for every toast. Also install `window.onerror` and `unhandledrejection` handlers (in `main.ts`) that log as level "error" with the stack, without showing a popup.
- Error toasts stay until dismissed or for at least 12 s and pause on hover; info toasts keep 4.2 s. Add a small "Copy" button on error toasts.
- The lead (Claude Code) reads the file directly when the user mentions a vanished popup; no MCP tool needed in this plan.
- Use theme tokens only for any UI change (the app has a dark theme).

Verification: `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::ui_log` (append, rotation, no-project fallback), `cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests --bins -- -D warnings`; in `mnemo2/`: `npx svelte-check --tsconfig ./tsconfig.json`, `npm run check:no-mock`, `npx vitest run` (withToast logs with the right level; error toasts persist and pause on hover; window error handlers log). Never start a dev server or any command that does not exit on its own.

## Acceptance scenarios
- After any popup, a line with its time, level and text is in .coding/logs/mnemo2-ui.jsonl of the open project.
- An error popup stays visible until dismissed (or at least 12 s), pauses on hover, and has a Copy button; info popups still fade after about 4 s.
- An uncaught frontend exception is logged as an error with its stack, without a popup.

## Steps
- [x] 1. **Log every popup and uncaught frontend error** — new `src-tauri/src/mnemo2/ui_log.rs` (+ tests) registered in `src-tauri/src/mnemo2/mod.rs`, new `mnemo2/src/lib/uiLog.ts` (+ tests), `mnemo2/src/lib/store.ts` (withToast level + call sites), `mnemo2/src/lib/types.ts` (ToastMessage level), `mnemo2/src/components/Toast.svelte` (+ test), `mnemo2/src/main.ts` (window error handlers).

## Progress
- 2026-11-02 step 1 done: step 1 merged: uiErrorLog99 (uiErrorLog99) on deepseek-v4.1-flash:1017, merge commit 1484e0fe; review: Every toast and every uncaught frontend error is appended to .coding/logs/mnemo2-ui.jsonl (config-dir fallback, 2 MB rotation); toasts carry a level, error toasts stay at least 12 s or until dismissed, pause on hover and have Copy.
