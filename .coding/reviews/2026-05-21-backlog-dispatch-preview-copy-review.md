# Review: Backlog dispatch bug fix + hover preview & copy button

**Date:** 2026-05-21
**Scope:** All uncommitted changes (`git diff HEAD`).
**Feature files:** `src-tauri/src/ipc/backlog.rs`, `src-tauri/src/ipc/backlog_cmds.rs`, `src-tauri/src/main.rs`, `frontend/src/lib/tauri.ts`, `frontend/src/components/views/BacklogView.tsx`.
**Out of scope (pre-existing/unrelated):** `package.json`/`package-lock.json` (vite 5→8, vitest 2→4 bumps), `.coding/backlog.json`, `.coding/plans/*` bookkeeping.

## Summary

The `dispatch_item` refactor is a **faithful, behavior-preserving extraction**. The new private `dispatch_item(app, state, &item)` helper contains the exact body of the old `dispatch_next_impl` (busy check → `manager.send` → `drop(manager)` → `emit_prompt_dispatched` → `set_status(InFlight)` → `single_in_flight.store` → `emit_backlog_changed` → `Ok(true)`), in the same order, with the same lock acquire/drop discipline. `dispatch_next_impl` now resolves the top-pending item and delegates; `dispatch_item_impl` resolves a specific id via `pending_item` and delegates. Auto-feed (`run_all.rs:652` → `dispatch_next_impl`) and Run-All (`run_all_dispatch_next`, separate path) are unaffected. The `single_in_flight` tracking is set identically on the per-item path, so `on_main_turn_resolved` (run_all.rs:638) will mark the correct item Done/Failed.

`pending_item` correctly refuses non-pending items (`i.id == id && i.status == Pending`); the unit test covers pending/in-flight/unknown-id.

## Findings

### Bugs (minor / non-blocking)

1. **Copy button: unhandled clipboard promise** — `frontend/src/components/views/BacklogView.tsx:244`
   `navigator.clipboard.writeText(item.text)` is fire-and-forget (not awaited, no `.catch`). If the write rejects (clipboard permission denied, or a non-secure-context webview), `setCopied(true)` still runs, showing a false "copied" checkmark. In a Tauri desktop webview this almost always succeeds, so impact is low, but the success state can lie. Consider `await`ing and gating `setCopied(true)` on success, or `.catch`ing to suppress the unhandled rejection.

2. **Copy button: reset timer not cleared on unmount** — `BacklogView.tsx:246`
   `setTimeout(() => setCopied(false), 2000)` is not tracked/cleared. If the card unmounts within 2s of a copy, `setCopied(false)` fires on an unmounted component. React 18 no longer warns about this and it's harmless, but it's inconsistent with the preview timer's cleanup discipline.

3. **Hover preview: no vertical clamping** — `BacklogView.tsx:264`
   `top = rect.bottom + 8` is not clamped to the viewport height. For a card near the bottom of the window, the 320px-wide / `max-h-72` panel can extend below the viewport (it does scroll internally, but its top can be off-screen). Horizontal clamping is handled (`Math.max(8, Math.min(rect.left, window.innerWidth - 336))`); vertical is not. Minor UX, not a correctness bug.

### Constitution compliance

- **Doc comments on public Rust functions:** ✅ `pending_item`, `backlog_dispatch_item`, `dispatch_item_impl`, and the private `dispatch_item` all have doc comments.
- **Line-ending style:** ✅ The five feature files show no LF/CRLF warnings in `git diff --numstat`. The CRLF warnings are confined to `.coding/backlog.json` and the plan `.md` (bookkeeping files, pre-existing LF content).
- **Windows/PowerShell:** N/A — no shell commands in the diff.
- **Never commit to main:** N/A — review only.

### Non-issues (verified, no action needed)

- **Hover preview timer on unmount-during-delay:** The `setTimeout` callback guards with `const rect = cardRef.current?.getBoundingClientRect(); if (!rect) return;` before any `setState`, so if the card unmounts during the 400ms delay, the callback self-disarms (no state update on an unmounted component, no React warning). The `previewTimer` ref is also cleared in `hidePreview` (mouse leave) and in the `useEffect` cleanup when `previewOpen` is true. No leak.
- **`dispatch_item` borrow:** `item: &BacklogItem` is borrowed; `text`/`images` are `.clone()`d for `send` (same as the original owned-item path), `item.id` is `Copy`. No borrow issues.
- **`id: u64` ↔ JS number:** Tauri deserializes the JS number into `u64`; backlog ids are monotonic from 1, well within `Number.MAX_SAFE_INTEGER`. Fine.
- **`hidePreview` stale closure in scroll effect:** The effect deps are `[previewOpen]`; `onScroll` captures a `hidePreview` that only touches refs + a stable setter, so the stale closure is correct.
- **`useCallback` / `Check` imports:** still used (`addImageFiles` line 89; `Check` in copy button + editor save). Not unused.

### Cleanup (optional, non-blocking)

- **Dead frontend export:** `backlogDispatchNext` in `frontend/src/lib/tauri.ts:611` is no longer referenced by any `.tsx` (the ▶ button now calls `backlogDispatchItem`). The backend `backlog_dispatch_next` command must stay (auto-feed uses `dispatch_next_impl`), but the frontend binding is now orphaned. Keeping it as public API is defensible; removing it would be tidier.

## Verdict

The core change (dispatch refactor + `pending_item`) is **correct and safe to commit**. All findings are minor/non-blocking UX and cleanup nits in the new frontend affordances; none are correctness or security blockers.
