## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes for plan bcf507c7 (backlog 2ccf92b0): `git diff HEAD` (`.coding/backlog.jsonl` bookkeeping, `src-tauri/src/main.rs` startup sweep, `src/browser/mod.rs` watchdog + live-profile registry + marker-aware sweep + `ensure_webview` leak fix) plus the untracked plan file `.coding/plans/bcf507c7.md`. The design is sound and the concurrency handling is correct (walkthrough below); two low findings should be fixed before commit.

### Findings

**LOW-1 — `sweep_orphan_profiles` reaps on "doubt" (future-dated or unreadable marker mtime), contradicting its own comment and the cross-process safety requirement.**
`src/browser/mod.rs:500-508`:

```rust
// Any doubt (missing mtime, clock skew) keeps the dir.
let fresh = meta
    .modified()
    .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age < MARKER_GRACE));
if fresh { continue; }
let _ = std::fs::remove_dir_all(&path);
```

- A marker mtime in the **future** (clock stepped *backward* between the owner's `touch_live_markers`/`register_live_profile` and this sweep) makes `now.duration_since(t)` return `Err` → `is_ok_and` yields `false` → `fresh = false` → the dir is **reaped**, not kept. Same when `meta.modified()` itself returns `Err` (filesystems without a reliable mtime): `is_ok_and` → `false` → reaped. Both cases contradict the comment on line 501 ("Any doubt (missing mtime, clock skew) keeps the dir") and the stated requirement that future mtimes be treated as fresh.
- The pre-marker code kept future-dated dirs (`t < cutoff` is `false` for a future `t`), so this edge regressed.
- The failure is correlated, not one-off: a single backward clock step makes *every* marker written before the step look future-dated, so the next sweep in any instance reaps them all — including other live instances' profiles.
- Rated LOW because the trigger is rare and the impact bounded (ephemeral temp profile; the victim's live Chromium usually holds locks so `remove_dir_all` fails best-effort; its own watchdog reaps + respawns) — but it violates the design's core "any doubt keeps the dir" invariant and must be fixed.
- Fix (one line, keep-on-doubt):
  ```rust
  let fresh = meta
      .modified()
      .map_or(true, |t| now.duration_since(t).map_or(true, |age| age < MARKER_GRACE));
  ```
  (If reap-on-doubt were genuinely intended, the comment must change instead — but keep-on-doubt is the direction the design's own philosophy and the review requirement demand.)

**LOW-2 — the plan's step-2 `BUG:` memory record is not findable in the memory store.**
`.coding/plans/bcf507c7.md:18` is checked "[x] … memory_write a BUG: record (symptom → root cause → fix + regression test name)". Two targeted semantic searches ("BUG offline browsers accumulate watchdog reap profile sweep", "2ccf92b0 offline browsers accumulating session reap watchdog") and a browse of the bug-record store surface no record for this defect — top hits are unrelated bugs (slid resolution, finish gate, reasoning_effort). The app auto-captures a `BUG:` record at `finish`, so it may land then, but as it stands the checked step is not reflected in the store. Write the record before finish (symptom: offline browsers accumulate per session; root cause: lazy `ensure_browser` detection + launch-time-only >1h sweep; fix: watchdog + marker-aware sweep; tests: `sweep_reaps_orphans_but_never_live_profiles`, `watchdog_reaps_dead_browser_without_subsequent_operation`), or verify it exists under an unexpected title.

### Verified correct (review-focus walkthrough)

**Concurrency — no deadlock, no double-reap.**
- `navigate` (`src/browser/mod.rs:573-586`) scopes the state lock around `ensure_browser`, releases it, then calls `ensure_watchdog` — which takes the lock itself. No self-deadlock; `ensure_watchdog`'s check-spawn-store runs under one lock hold (533-542), so concurrent navigates cannot double-spawn the watchdog.
- `watchdog_loop` (547-558): sleeps, upgrades the `Weak` (exits when the manager's state drops), reaps under a *scoped* state lock with no await inside, then `touch_live_markers` (std registry mutex, no await), then awaits the `spawn_blocking` sweep *outside* the state lock. No lock is held across an await anywhere in the loop.
- Watchdog vs `ensure_browser` vs `close`: every reap path takes the state lock first; whoever takes `browser`/`handler`/`profile` wins, the other sees `browser: None` and no-ops. No double-kill or double-remove path exists.
- The sweep snapshots the registry (`LIVE_PROFILES.lock().map(|l| l.clone())`, line 486) so it never holds the registry mutex across blocking FS work; `touch_live_markers` holds it only across bounded file opens. Registry poisoning degrades safely (`unwrap_or_default` → empty snapshot → live profiles still protected by their fresh markers).

**Registry/marker invariants — complete.**
- All `state.profile` sites enumerated: set at launch (405, registered at 397 *after* the successful `Browser::launch` — a failed launch leaves no entry and the `TempDir` drop removes the dir); taken in `reap_dead_browser` (434, unregistered at 439) and `close` (842, unregistered at 847-849); `Drop for State` (215-221) unregisters the still-owned case. No path leaves a live dir sweepable or a dead dir protected.
- `kill_browser` failure → the dir stays but its marker is already backdated past grace → the sweep reaps it on a later tick. Coherent: the sweep is the retry-forever safety net behind the ~15s `schedule_profile_removal` budget.
- The launch window (tempdir created, marker not yet written) is protected by the marker-less 1h fallback; the fire-and-forget launch-time sweep can never race the dir it is about to create into deletion.

**Cross-process safety.**
- MARKER_GRACE 15min vs the 30s tick is a 30× margin — sane for loaded/suspended machines (the doc comment reasons about exactly this). Marker-less dirs keep the conservative 1h fallback. See LOW-1 for the one invariant violation (future mtimes).
- The `.mnemo-live` dotfile inside the Chromium user-data-dir is inert — Chromium ignores unknown dotfiles in the profile root and locks only its own files; it is removed with the dir.
- The sweep only touches `mnemo-browser-*` / `myharness-browser-*` entries directly in `std::env::temp_dir()` — no recursion, no traversal; registry paths and `entry.path()` derive from the same base, so the `HashSet` hit is exact.

**`ensure_webview` concurrent-winner leak fix — correct.**
`src/browser/mod.rs:978-986`: the loser aborts its just-spawned handler task before returning the winner's handle; abort drops the handler future (closing the loser's CDP WebSocket) and the loser's connected `Browser` drop only closes the WebSocket (connect-mode owns no process). The winner's state is untouched.

**Bug-plan checks.**
- Regression tests exercise the changed paths: `sweep_reaps_orphans_but_never_live_profiles` (non-ignored; covers registry-protected, foreign-fresh-marker, stale-marker-reaped, legacy-fresh-kept, and unregister-then-reaped) and `watchdog_reaps_dead_browser_without_subsequent_operation` (#[ignore] integration; real Chromium, handler aborted, watchdog reaps with no subsequent operation, profile dir removed). The first fails against the old `sweep_stale_profiles` (the fresh-aged stale-marker dir would survive its 1h cutoff); the second fails against the old lazy-only detection.
- Root cause documented in the plan (Context + Bug sections, `.coding/plans/bcf507c7.md`). BUG memory: see LOW-2.

**Project constitution.**
- Doc comments present on every new fn/const/field, including the now-public `sweep_orphan_profiles`. No `#[allow]` suppressions. Std-only FS APIs (`File::set_modified`, `LazyLock`, `SystemTime`, `Weak`) — no Windows-only APIs in lib code; the main.rs addition is platform-neutral Tauri setup.
- Documentation sync: the module doc (lines 13-21) describes the watchdog + marker lifecycle; no stale `sweep_stale_profiles` references remain in README/docs — the only hits are historical plans/reviews under `.coding/`, which correctly document past states.
- File-tools-first: no shell-based mutation anywhere in the change.
- Test evidence: `cargo test` (2272 + 16 passed, exit=0, warning-free under `#![deny(warnings)]`) is the parent's recorded evidence; this reviewer has no shell and verified by code reading.

### Notes (no action required)
- The unit test implements four dirs, not the plan's five: the dropped case (no marker + dir mtime backdated >1h) can't be built portably from std (`File::set_modified` needs a writable `File` handle; opening a directory with write access fails on Windows and Unix). The dropped branch is carried over verbatim from the old sweep, and the fresh-legacy keep case still pins the fallback. Justified and documented in-test.
- A system-wide suspension longer than MARKER_GRACE (15min) ages every marker past grace while frozen; on wake a sibling instance's sweep could theoretically beat the owner's first touch. Inherent to any timeout-based liveness scheme, bounded impact (ephemeral profile, auto-respawn), and both instances wake together — acceptable.
- The watchdog deliberately keeps sweeping after `close()` until the manager drops (documented at `State.watchdog` + `ensure_watchdog`); the cost is one temp-dir walk per tick. Fine.
- `reap_interval_ms: AtomicU64` (vs the plan's plain `Duration` field) is a justified deviation — the test setter needs `&self`.
