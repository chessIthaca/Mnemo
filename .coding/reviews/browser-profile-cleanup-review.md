# Review: browser profile cleanup + stale settings fixture

Branch `fix/browser-profile-cleanup-and-fixture-drift`, all uncommitted changes
(`git status`: `src/browser/mod.rs`, `frontend/src/lib/ipc-fixtures/dto-get-settings.json`,
`.coding/plans/stack.json`, new `.coding/plans/66a455d6-*.md`).

Scope verified against the full files (not just the diff): `BrowserManager`
state/lock structure, all `Arc<Browser>` holder sites, all `close()` callers
across the workspace, the fixture vs `src/config/general.rs` serde defaults,
and the changed tests.

**Verdict:** the core fix is sound and well-reasoned (explicit kill → wait →
retry-loop removal, OS thread so removal outlives the runtime; both changed
tests now kill + remove deterministically). The fixture change is correct
(`bundled_embedding_model: null`, `show_memory_activity: false` match the serde
defaults and their round-trip tests in `src/config/general.rs:405-501`; key
order stays alphabetical). Findings below are race-window correctness and a
locking-discipline inconsistency — not regressions to the fixed tests.

---

## Medium

### M1. `kill_browser`'s `Arc::try_unwrap` fallback silently leaks the Chromium child AND the profile dir — and its doc comment states a false invariant
`src/browser/mod.rs:558-563` (`kill_browser`), reachable from
`src/browser/mod.rs:532` (`close`) and `src/browser/mod.rs:198-206`
(`ensure_browser` respawn).

The only other holder of the launch-mode `Arc<Browser>` is the transient clone
in `navigate` (`src/browser/mod.rs:288-292`), taken under the lock and held
**across the `new_page` CDP await outside the lock**. If a `navigate` is in
flight when `close()` (or the `ensure_browser` respawn path) runs:

1. `Arc::try_unwrap` fails (strong count 2) → **no kill happens, ever** — there
   is no retry; the `Err` arm just drops the Arc.
2. Per this diff's own (verified) premise, chromiumoxide 0.7 never sets
   `kill_on_drop`, so when the in-flight `navigate` later drops its clone and
   the `Browser` destructs, the child **stays alive** holding locks on the
   profile dir.
3. `schedule_profile_removal` (`src/browser/mod.rs:573-583`) has already been
   called unconditionally, so its thread burns the full ~15s retry budget
   against a live process, then gives up. `sweep_stale_profiles` can't recover
   (the >1h sweep also fails while the process lives and does not retry over
   time).

Net: orphaned headless Chromium process + orphaned profile dir — exactly the
bug class this diff set out to eliminate, resurfacing in a race window. The
`ensure_browser` respawn path is **production code** (any operation after the
CDP connection drops while a prior `navigate`'s clone is still draining), so
this is not test-only.

The doc comment's justification is factually wrong on both counts:
- *"their drop order doesn't matter"* — false: drop does not kill the child, so
  leaving the last drop to another holder means the child is never killed.
- *"this path only runs on manager shutdown"* — false: the respawn path in
  `ensure_browser` (`src/browser/mod.rs:198-206`) calls `kill_browser` on every
  stale-browser respawn, mid-session.

Suggested fix (pick one, keep kill→removal ordering):
- Loop `Arc::try_unwrap` with a bounded deadline (e.g. re-check every 100ms up
  to ~5s — an in-flight `new_page` fails fast once the handler is aborted /
  connection drops) before falling back; or
- Store the child PID (or the `Child` handle) in `State` at launch and kill via
  the OS regardless of Arc holders; or
- At minimum, only schedule profile removal after a kill was actually
  performed, and correct the comment.

## Low

### L1. `close()` awaits `kill_browser` while holding the state mutex — contradicts the module's locking contract and the diff's own rationale
`src/browser/mod.rs:521-535`.

`close()` acquires `self.state.lock().await` (line 522) and then awaits
`Self::kill_browser(state.browser.take()).await` (line 532) — a kill + wait-for-exit
that can take hundreds of ms. This contradicts:
- the module doc's locking contract (`src/browser/mod.rs:23-26`: the state
  mutex is held "only for short state mutations"; I/O runs on cloned handles
  outside the lock), and
- the diff's own comment in `ensure_browser`
  (`src/browser/mod.rs:193-196`: *"awaiting the kill here would hold the state
  lock across it"* — the stated reason that path spawns instead).

Any concurrent `navigate`/`list_pages`/`webview_*` call stalls for the kill
duration. Not a deadlock (the kill never re-takes the state lock) and tests are
sequential, hence Low. Fix: mirror `ensure_browser` — release the lock, then
run `kill_browser` → `schedule_profile_removal` chained in one spawned task
(chaining preserves the kill-before-removal ordering `close()` currently gets
from awaiting inline; a bare spawn of only the removal would lose it). If
inline await is kept deliberately (e.g. so `profile_dir_is_removed_on_close`
starts its poll after a completed kill), document the exception in the module
locking note instead of leaving the contradiction.

### L2. No production shutdown path kills the browser — the fix only covers tests / explicit `close()`
`src-tauri/src/ipc/browser.rs`, `src-tauri/src/main.rs:746`, `src/browser/mod.rs:521`.

A workspace-wide search shows `BrowserManager::close()` has **zero production
callers** — only tests (`src/browser/mod.rs`, `src/tool/browser/mod.rs`). At app
exit the manager `Arc` (and its `Browser`) is simply dropped, and by this
diff's own verified analysis (no `kill_on_drop`), the headless Chromium child
survives the app on Windows, holding its temp profile dir (the >1h sweep then
can't remove it). This is pre-existing and outside the diff's diff-lines, but
it is squarely within the leak this diff set out to fix, so worth closing while
here: e.g. a `Drop` guard on `BrowserManager` (or a Tauri exit hook) that kills
the child via the same mechanism. If the team prefers to defer, record it
explicitly — noting it here so it is a decision, not an oversight.

## Nit (comment accuracy)

### N1. `kill_browser` doc describes an unreachable case
`src/browser/mod.rs:554-557`: *"A browser we merely connected to (the live
WebView2) has no child; `kill` is a no-op there"* — `kill_browser` is only ever
called with `state.browser` (launch-mode); `close()` detaches the webview via
plain `drop` (`src/browser/mod.rs:540-543`), never via `kill_browser`. Also
`Self::kill_browser(Some(browser)).await` at `src/browser/mod.rs:201` always
passes `Some` — the `Option` parameter only exists for the `close()` call site.
Trim the stale sentence (and consider taking `Arc<Browser>` directly) when
fixing M1, since that doc rewrite is needed anyway.

---

## Constitution compliance

- **Doc comments on public functions** (`agent.md`): the new helpers
  `kill_browser` / `schedule_profile_removal` are private and documented
  anyway; `close()`'s doc was updated correctly. ✔ (subject to M1/N1 comment
  corrections)
- **No `#[allow]`** introduced; build is warning-free (`deny(warnings)` at both
  crate roots, green `cargo test` proves it). ✔
- **Code style** matches the file's existing conventions. ✔
- **Fixture**: `dto-get-settings.json` matches serde output exactly
  (alphabetical key order preserved; both new fields verified against
  `src/config/general.rs:73,226` defaults `None`/`false` and their round-trip
  tests). ✔
- **Windows hazards**: retry loop correctly handles delayed lock release after
  `TerminateProcess`; `remove_dir_all` never follows symlinks (no
  path-substitution concern on the tempdir-owned path); detached removal
  threads are bounded (one per close/respawn). ✔
- **Plan bookkeeping** (`.coding/plans/stack.json`, new plan `.md`): expected,
  no findings. ✔

## Verification requested after fixes

`cargo test` (workspace) + `npm test` were green before this review; re-run
`cargo test --workspace` after addressing M1/L1 (Rust-only changes).
