+++
title = "browser profile lifecycle — watchdog + marker invariants"
created = "2027-01-11"
+++

The browser profile lifecycle (src/browser/mod.rs, plan bcf507c7 / commit 341b25d) has invariants future changes must preserve:

1. **The sweep never touches a live profile.** In-process liveness = membership in the `LIVE_PROFILES` registry (registered after a successful launch, unregistered at every take-site + `Drop for State`). Cross-process liveness = the `.mnemo-live` marker's mtime being within `MARKER_GRACE` (15min vs the 30s watchdog tick). Any doubt (unreadable or future-dated mtime, clock skew) KEEPS the dir — a lingering temp dir is always preferable to deleting a live instance's profile (review round-1 LOW-1).
2. **Retiring a profile declares it dead**: `unregister_live_profile` backdates the marker past grace, so the sweep (which runs at launch, every watchdog tick, and app startup in main.rs) is the retry-forever net for dirs the ~15s removal retry loop misses.
3. **The watchdog spawns lazily on first browser launch** (`ensure_watchdog` from `navigate` — `BrowserManager::new` may run outside a tokio runtime; check-spawn-store under one state-lock hold prevents double-spawning), holds a `Weak` to the state Arc (exits when the manager drops), and is deliberately NOT aborted by `close()` — it keeps sweeping until the manager drops.
4. **The watchdog never awaits while holding the state lock** (reap spawns the kill task; the sweep runs via `spawn_blocking` outside the lock), and the sweep snapshots the registry instead of holding its lock across blocking FS work.

Related: BUG record "Offline browsers accumulate per session — lazy reap + launch-only sweep" (.coding/knowledge/bug/2027-01-11-offline-browsers-accumulate-per-session-lazy-rea.md); reviews .coding/reviews/2026-09-11-offline-browser-reap-watchdog-review.md + -round2-review.md.
