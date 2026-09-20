## Verdict: PASS

Round-2 verification of commit 880736c (HEAD of wt/mnemo; `git diff HEAD` and `git status` both empty — no uncommitted residue) against round 1 (.coding/reviews/2026-09-20-ec425270-wedged-offscreen-browser-review.md, FINDINGS 0 high / 3 low). All three findings are fixed correctly; the factory.rs comment precision item is fixed; the fixes introduce no new defects.

### L1 (docs sync) — FIXED ✓

`.coding/browser-debugging.md` "Notes & limits" gained the bullet at lines 121-126: "**Operations abort after ~30s (~60s for navigate)** — an unresponsive or wedged browser is killed and transparently restarted on the next operation; re-navigate to reopen your pages. Keep eval scripts short and split long waits: an infinite loop wedges the page until the timeout reaps it (a background watchdog also probes liveness and force-reaps an idle-wedged browser on its own)." Every required element is present (abort times, kill + transparent restart, re-navigate guidance, keep-scripts-short, watchdog probe mention) and the figures match the constants exactly: `CDP_OP_TIMEOUT = 30s` (src/browser/mod.rs:131), `CDP_NAVIGATE_TIMEOUT = 60s` (:136). Placed with the sibling behavioral-limit bullets; README/PLAN need nothing (round 1 already verified).

### L2 (late-timeout race) — FIXED ✓

The launch-generation guard is implemented exactly as specified and is sound:

- `State.generation: u64` (:209-214, doc comment explains the L2 rationale); initialized to 0 in **all three** constructors — `new` (:312), `new_with_webview_url` (:364), `Default` (:1864). A `generation` search over the file confirms exactly these three initializations and a single bump site.
- Bumped only in `ensure_browser` (:459, `state.generation += 1`) — the only place a fresh browser is stored, so the generation uniquely identifies the current launch.
- Captured by the `cdp` wrapper at op start (:599, `let generation = self.state.lock().await.generation;`) and passed to `force_reap_wedged(&self.state, generation)` (:603), which checks `state.generation != generation` **under the lock** (:551) and returns `false` without reaping on mismatch (:557).
- The watchdog captures `(state.browser.clone(), state.generation)` under its reap lock (:704-708) and passes the generation through to `force_reap_wedged` (:725).

**No deadlock from the wrapper's lock read** — verified by construction: the `:599` MutexGuard is a statement temporary, dropped at the end of the `let` *before* `tokio::time::timeout(timeout, fut)` runs the future, so no lock is held while the wrapped future executes. No `cdp` call site holds the state lock: `navigate` clones the browser handle under a short lock and drops it (:752-760); `list_pages` snapshots under a short lock and drops it (:860-868) — its pruning lock lives *inside* the wrapped future, sequenced strictly after the generation read's guard is dropped; the remaining ops use `lookup` (short lock, dropped). `force_reap_wedged` takes the lock itself (:550) and is only reached from the timeout arm (lock long released) and the watchdog (reap-lock block closed at :708 before the probe at :719). Review C1 (CDP outside the state lock) is preserved.

**The guard preserves the reap path.** When the timed-out op's browser is still current, no respawn occurred, generation matches, and `detach_browser` proceeds — the real reap. Edge cases check out: browser died-but-not-respawned → generation unchanged, `browser.take()` is `None` → clean no-op `false`; browser died-and-respawned → mismatch → the healthy respawn survives (exactly the L2 interleaving from round 1); watchdog's strike-2 firing after a mid-probe respawn → mismatch → no-op.

**The wedge regression test still exercises the real reap** (`wedge_renderer_times_out_and_self_heals`, :2471-2515): navigate spawns the browser (generation 1), the wedged eval captures generation 1 at op start, times out at 3s, and `force_reap_wedged(state, 1)` finds generation still 1 (nothing respawned in between) → real detach + kill; the asserted "unresponsive"/"restart" error, the self-heal navigate (generation 2), and the exactly-one-fresh-page `list_pages` assert all ride the genuine reap path. The browser-less unit test (`cdp_wrapper_times_out_pending_future`) also passes through the matching-generation arm (detach returns `None` → `false`), so the guard doesn't neuter it either.

### L3 (unbounded round-trip) — FIXED ✓

The navigate subscribe-failure cleanup arm (:801-819) now wraps `page.close()` in the same `cdp` wrapper (`"navigate"` + `cdp_timeout(CDP_NAVIGATE_TIMEOUT)`) — bounded, and a hanging close force-reaps via the same generation-guarded path; the result is discarded (`let _ =`), and the arm still returns the original `"failed to subscribe console events for the new page"` error. Using the navigate budget (60s) rather than the op budget for this navigate-path cleanup is coherent; both are bounded. If the cleanup close times out, the force-reap is safe: the half-opened page is not yet in state (insertion at :824-846 comes later), and a 60s-hanging close means the browser is wedged anyway — consistent with the "re-navigate to reopen your pages" message. With this, every launch-mode CDP round-trip in the file is bounded (launch handshake in `ensure_browser` intentionally excepted, as accepted in round 1).

### factory.rs ExecutingResearch ceiling comment — FIXED ✓

The comment (:1922-1928) now reads: "26_800 → 27_300 (2027-02-05): same cause as the Executing raise above (the offscreen browser timeout descriptions, plan ec425270, ~+484 — the browser family rides the research filters — plus ~+194 of update_plan append-window wording that landed after this filter's 2027-01-24 baseline); measures ExecutingResearch at 26_921 chars." The +678 delta (26_243 → 26_921) is now attributed precisely: +484 + ~194 = 678. ✓ The other three raises (Executing 33_200, PlanFrozen 34_500, Reviewing 28_400) carry matching dated comments with measured figures and headroom.

### Constitution checks

- **Multi-platform neutrality ✓** — the fixes are a `u64` field, a lock-read, an integer compare, and one more `cdp` wrap; no platform-specific APIs.
- **File-tools-first ✓** — no shell-based mutation; clean targeted edits.
- **Docs sync ✓** — L1 closed; module doc, tool descriptions, and human doc all consistent with the constants.
- **Warning-free ✓** — `force_reap_wedged`'s `bool` is discarded at both call sites (not `#[must_use]`, no warning); `generation` is read in production code; no new dead code; no `#[allow]`.
- **Doc comments ✓** — new private fns documented; `set_cdp_timeout` (pub(crate), cfg(test)) documented; public op signatures unchanged with their docs intact.
- **Regression test ✓** — still fails pre-fix via the 10s outer guard (the defect) and passes post-fix through the real reap path (see L2 above).
- **Commit hygiene ✓** — the commit carries the round-1 report, plan file, BUG + HOW knowledge records, and a `.coding/backlog.jsonl` addition (pending item 77efc1b7, load_tools gating — bookkeeping swept in per the `.coding/` side-car pattern; harmless).

### Verification note

Read-only review — tests not re-run by the reviewer. The implementer's stated matrix is consistent with the change (4 new tests: 2 unit tests run under plain `cargo test` only when the browser feature is on per the HOW memory — both feature runs cover them; the 2 `#[ignore]` integration tests run in the serial `--ignored` pass; the one parallel-run flake cited for `webview_frame_and_input_cdp_surface` is a pre-existing test outside this change's scope and passes serially). The parent's closing sequence re-runs the suite.
