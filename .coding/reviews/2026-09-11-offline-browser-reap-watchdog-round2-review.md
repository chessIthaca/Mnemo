## Verdict: PASS

Round-2 verification of commit 341b25d ("Reap offline browsers: watchdog + marker-aware profile sweep", backlog 2ccf92b0, plan bcf507c7) on wt/agenticcoding. Both round-1 findings are correctly fixed, no regressions were introduced, the commit contains exactly the expected six files, and the working tree is clean.

### LOW-1 — keep-on-doubt marker semantics: FIXED

`src/browser/mod.rs:500-508` (`sweep_orphan_profiles`):

```rust
// Any doubt (missing mtime, clock skew) keeps the dir.
let fresh = meta.modified().map_or(true, |t| {
    now.duration_since(t).map_or(true, |age| age < MARKER_GRACE)
});
```

All paths traced:
- `modified()` → `Err` (unreadable mtime) → `map_or(true, …)` → `fresh = true` → dir **kept**.
- Future-dated mtime (clock stepped back) → `duration_since` → `Err` → `map_or(true, …)` → dir **kept**.
- Past mtime within `MARKER_GRACE` → kept; past mtime ≥ grace → the **only** reap path.

This is verbatim the fix round 1 prescribed; the "Any doubt … keeps the dir" comment now matches the behavior. The declare-dead mechanism is unaffected: `unregister_live_profile` backdates to `now − MARKER_GRACE − 60s` (a genuinely past mtime), so retired dirs are still reaped.

The regression test gained case E (`src/browser/mod.rs:2082-2104`): a dir whose marker is future-dated +1h, asserted to survive with the message "future-dated marker (clock skew) must keep the dir". Against the old `is_ok_and` chain this case fails (future mtime → `fresh = false` → reaped), so the test genuinely pins the fix. The sweep's `now` is captured after the test's future-stamp, so the marker is still future-dated at sweep time — robust.

### LOW-2 — BUG memory record: FIXED

- `memory_search` (record_type bug, query "offline browsers accumulate watchdog reap lazy sweep") returns the record as the top hit (score 0.85): semantic tier, id 7420ed8e-9418-574b-9d66-f7a1ee283671, title "BUG: Offline browsers accumulate per session — lazy reap + launch-only sweep" — findable, which is exactly what round 1 could not do.
- The knowledge file `.coding/knowledge/bug/2027-01-11-offline-browsers-accumulate-per-session-lazy-rea.md` (committed in 341b25d) is well-formed and pointer-first: symptom (user report 2027-01-12, backlog 2ccf92b0) → root cause (L1 lazy detection, L2 launch-only >1h sweep, L3 ~15s removal retry) → fix (watchdog + LIVE_PROFILES registry + `.mnemo-live` marker + launch/tick/startup sweep + `ensure_webview` leak fix) → **both** regression test names (`sweep_reaps_orphans_but_never_live_profiles`, `watchdog_reaps_dead_browser_without_subsequent_operation`) → pointers to the plan and round-1 review.

### No regressions — the LOW-1 edit touched nothing else

Every line anchor round 1 cited matches the current file exactly: register at 397 / profile set at 405 (launch), reap take/unregister at 434/439, registry snapshot at 486, fresh computation at 500-508 (net-zero line change), `ensure_watchdog` 533-542, `watchdog_loop` 547-558, navigate lock scope 573-586, close 842/847-849, `ensure_webview` winner path 978-986, `Drop for State` 215-221. The full 341b25d diff of `src/browser/mod.rs` contains exactly the feature round 1 verified (module-doc update, consts/registry/helpers, `State.watchdog` + `Drop`, `reap_dead_browser` extraction, marker-aware sweep, watchdog, navigate/close/`ensure_webview` edits, both regression tests) plus the two fixes — nothing else. The `src-tauri/src/main.rs` hunk is the 9-line platform-neutral startup sweep round 1 described (placed before the fallback/normal branch split, doc-commented); `.coding/backlog.jsonl` is the pending → in_flight bookkeeping update with plan metadata.

### Commit hygiene

- `git show --stat 341b25d`: exactly the six expected files — `.coding/backlog.jsonl`, `.coding/knowledge/bug/2027-01-11-offline-browsers-accumulate-per-session-lazy-rea.md`, `.coding/plans/bcf507c7.md`, `.coding/reviews/2026-09-11-offline-browser-reap-watchdog-review.md`, `src-tauri/src/main.rs`, `src/browser/mod.rs` — no extras.
- `git diff HEAD` and `git status --short` are empty — working tree clean; 341b25d is the branch tip (parent d88f656, the pre-item checkpoint).

### Test evidence

The parent's recorded evidence (this reviewer has no shell and verified by code reading, as in round 1): `cargo test --lib --features browser sweep_reaps_orphans` → 1 passed; full workspace `cargo test --quiet` → 2272 + 16 passed, 0 failed, exit=0, warning-free under `#![deny(warnings)]`. The `#[ignore]` integration test passed against real headless Chromium (1.32s) before the LOW-1 fix; the LOW-1 change only alters the keep/reap decision for error/future-mtime cases, which that test's normal path does not exercise — the reasoning is sound.

### Notes (no action required)

- Keep-on-doubt can in principle leave a dead dir unswept under a persistently pathological mtime (unreadable or pinned in the future); that is the safe direction (a lingering ephemeral temp dir vs. deleting a live instance's profile) and is precisely the semantics round 1 prescribed.
- The test's five dirs live in the real temp dir; each is registry-protected (A), marker-protected (B fresh, E future-dated), or age-protected (D fresh legacy), so a concurrent sweep from another test or instance cannot break the assertions, and every dir is cleaned up (reaped or dropped) before the test ends.
