# Review — Window title shows product name + loaded project folder

**Date:** 2026-10-26
**Plan:** Window title shows product name + loaded project folder
**Scope:** All uncommitted changes (`git diff HEAD` + untracked files).

## Files changed

- `src-tauri/src/main.rs` — the substantive change (new block inside `.setup(|app| …)`, lines 62-75).
- `.coding/backlog.json` — bookkeeping (backlog item 31 marked `done`).
- `.coding/plans/stack.json` — bookkeeping (active plan id updated).
- `.coding/plans/772bb040-a402-4dea-9678-ff2afc9e7da0.md` — untracked plan file (bookkeeping).

## Summary of the code change

A block was added inside the `.setup` closure, immediately after `backlog_root` is
computed (lines 54-60) and before it is consumed by `backlog_root.join(...)` (line 79).
The new block fetches the `"main"` webview window and, if present, sets its title to
`"myharness — {folder}"` where `folder` is the last path component of `backlog_root`,
falling back to `"."`.

## Findings

### Correctness — no findings

- **`backlog_root` is the right source in both branches.** The Ok branch resolves to
  `brain.project.lock().await.root.clone()` (the loaded project root); the Err branch
  falls back to `std::env::current_dir()` (with a `PathBuf::from(".")` ultimate
  fallback). Both represent the resolved project root, so deriving the folder name from
  `backlog_root` is correct in either case.
- **No borrow conflict.** `backlog_root.file_name().and_then(|n| n.to_str())` yields a
  `&str` borrowing `backlog_root`, but it is consumed immediately by
  `format!("myharness — {folder}")`, which produces an owned `String`. Non-lexical
  lifetimes end the borrow at that point, so the later `backlog_root.join(...)` at line
  79 is unaffected. `app.get_webview_window("main")` returns a `WebviewWindow` handle
  that does not borrow `app`, so subsequent `app.handle().clone()` usage is also fine.
- **Window label `"main"` is correct for Tauri 2.** `tauri.conf.json` defines a window
  with no explicit `"label"` field; Tauri 2 defaults the label to `"main"`. The
  `Manager` trait (providing `get_webview_window`) is imported at line 26
  (`use tauri::Manager;`).
- **`set_title` result correctly discarded.** `WebviewWindow::set_title` returns
  `Result<(), tauri::Error>`; it is discarded with `let _ =`, which is idiomatic and
  does not trip `#![deny(warnings)]`.

### Bugs — no findings

- **No panic on `None`.** The `get_webview_window("main")` result is guarded by
  `if let Some(window)`, so a `None` return simply skips the title update and leaves the
  static `"myharness"` title from `tauri.conf.json` in place. No unwrap/expect on the
  window.
- **Window exists at setup time.** In Tauri 2 the setup hook runs after the configured
  windows are created, so the `"main"` window is available. Even if it were not, the
  guard degrades gracefully.
- **`file_name()` edge cases handled.** A root path (`/` or `C:\`) yields `None` →
  fallback `"."`; a `.` path yields `Some(".")` → `"."`; trailing separators are
  stripped by `file_name()`. All produce a sensible title.

### Security — no findings

- The title is derived solely from a local filesystem path component (the last segment
  of the resolved project root). It is not user input and introduces no new mutation
  surface or IPC surface. No injection risk.

### Constitution compliance — no findings

- **Warning-free build.** `#![deny(warnings)]` is active at the crate root (line 7). No
  `#[allow(...)]` suppressions were added; the only `Result` discard uses `let _ =`,
  which emits no warning.
- **Doc comments.** No new public functions were introduced; the change is entirely
  inside `fn main()`. Existing public-function doc comments are untouched.
- **Code style.** The new block matches the surrounding style (why-comments, `let _ =`
  for discarded results, consistent formatting).
- **Windows 11 / paths.** Uses `std::path::PathBuf` and `file_name()`, which are
  cross-platform; no Linux paths or bash syntax. The em-dash `—` is valid UTF-8 in a
  Rust string literal and is handled correctly by Tauri's title setter on Windows.

## Conclusion

No findings. The diff is clean. The window-title change is correct, safe, and
constitution-compliant. The only other changes are `.coding/` bookkeeping files
(backlog status, plan stack, plan file), which are expected and not subject to code
review.
