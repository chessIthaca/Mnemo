## Verdict: FINDINGS (3 high, 3 low)

Review of ALL uncommitted changes on wt/agenticcoder for plan eedeaeab ("Show the splash indexing-progress dialog when switching projects"). The switch-path happy case is correct (gate skip + immediate Started + snapshot catch-up + 800ms hold all verified), but the snapshot catch-up introduces race paths that can break the cold-start no-flash guarantee and — worse — wedge the app behind an undismissable modal. The in-code comment, plan, and SPEC all describe a "terminal Done is ALWAYS emitted" safeguard that the code does not actually implement.

Summary of findings:
- H1: Terminal Done is NOT always emitted — the emit is still inside `if forwarded` (main.rs:1547), contradicting the adjacent comment, plan step 1(f), and the new SPEC. This removes the documented safety net for late-seeded overlays.
- H2: The progress snapshot is published for EVERY pass, including 1s-gated sub-second cold passes — a fetch landing inside the pass window seeds and SHOWS the overlay on a cold start of an already-indexed project (no-flash regression), and with H1 it never receives a terminal event → permanent undismissable modal.
- H3: The frontend seed callback (`getIndexProgress().then`) is not invalidated by events that arrive while the fetch is in flight — a response resolving after the terminal `done` was handled seeds a frozen post-pass overlay with no future events → same wedge, reachable on the switch path too.
- L4: Torn snapshot reads (two separate Relaxed atomics). L5: unrelated untracked knowledge files in the tree. L6: misleading test title in indexingOverlay.test.ts.

Details follow below.

## Verified clean (no finding)

- **`switched_open` capture (main.rs:968–1000):** set `true` only on the valid pending-marker arm; the invalid-marker arm returns `NeedsProject` before any pass exists, `--project` and cwd-walk stay `false` (cold-start semantics — correct, they are not switches). Threaded correctly into the spawn task; the `move` progress closure captures the `Copy` bool.
- **Gate logic (codegraph_cmds.rs:317–344):** `(switched || elapsed ≥ 1s) && throttle` — switch skips only the time gate, throttle (first/every-50th/last) always applies. Gate table extended with `switched=true` rows including "throttle still drops non-boundary ticks". Correct.
- **Snapshot set/record/clear ordering:** `mark_startup_index_active()` at task start; `record_startup_index_progress` on every tick BEFORE the gate (snapshot complete even for dropped ticks); `clear_startup_index()` at main.rs:1538 BEFORE the terminal emit at :1570 — a post-pass fetch reports `None`, not stale final counts. Correct ordering.
- **`forwarded` initialized to `switched_open` (main.rs:1495–1504):** on switch, `Started` was actually emitted, so counting it as forwarded is accurate → Failed IS emitted on a failed switch pass (correct); on cold start `forwarded` starts false and Failed stays gated (never pop a failure card for a silent pass). Correct semantics — but see H1 for the Done side.
- **Console mode (no window):** `mark`/emit only in the `Some(handle)` arm; `g.index(None)` path never touches the snapshot. `get_index_progress` → `None`. Clean.
- **Wire shape:** `done`/`total` are single lowercase words, so snake_case == camelCase; `index_progress_snapshot_wire_shape` pins `{"done":3,"total":7}`; frontend `IndexProgressSnapshot` matches; command registered in `invoke_handler` (main.rs:705). Clean.
- **Timer/token mechanics in IndexingOverlay (the happy paths):** `handleEvent` bumps the token and cancels any pending hide timer on entry; the timer callback re-checks `token !== mine`; cleanup clears the timer and the listener; the subscribe `.then` drops late-resolving listeners under StrictMode double-mount (`disposed` guard); the fetch `.then` checks `disposed`. Rules of hooks OK — `useBrowserOverlay(state !== null)` is called unconditionally (IndexingOverlay.tsx:104). A `progress` event arriving mid-hold keeps the overlay up and re-arms the hold from the ORIGINAL `shownAt` (correct). `started` resetting the bar to 0/0 is safe because channel ordering guarantees Started precedes its ticks for any given listener, and the seed uses `prev ?? snap` so it never clobbers fresher event state.
- **Cold-start event path (absent seeding):** still silent — forwarded stays false on a sub-second pass, no events; the reducer test "done is idempotent from the hidden state" pins the invisible lone-done fold. (The regression is the NEW snapshot path — H2.)
- **Create-project seed pass (projects.rs, unchanged):** immediate `Started` (line 118), `seed_stores` progress cb (line 129), Done/Failed terminal — never touches the snapshot. Consistent: a create flow always has its overlay mounted and subscribed BEFORE the user triggers the pass, so catch-up is unnecessary there; `get_index_progress` returns `None` during a seed pass → the seed callback no-ops. No double-Started conflict (channel-ordered; a second Started resets the bar, which is correct for a new pass).
- **Security:** `get_index_progress` is a read-only counter getter — no approval surface, no paths, no fs, no state mutation. No bypass.
- **Multi-platform neutrality:** additions are `std::sync::atomic` / `std::time` (Rust) and `window.setTimeout/clearTimeout` (webview). No `cfg(windows)`, no Windows paths/shell syntax. Clean.
- **Docs:** README bullet updated for switch behavior; the -3 SPEC got `status = "superseded"`; the successor SPEC file exists with correct `supersedes` front matter and the memory record was superseded (the live record already describes switch coverage). PLAN.md needs no row — no contradicting entries (all "progress/switch" hits there are agent-loop/plan-UI concepts, unrelated to the overlay stream). `indexingOverlay.test.ts` is in the vitest include (vitest.config.ts:59) — extending it required no include change, as planned.

## Findings

### H1 — Terminal Done is NOT always emitted; code contradicts its own comment, the plan, and the SPEC

**Where:** src-tauri/src/main.rs:1539–1571.

The comment block (1539–1546) says: *"Terminal Done is ALWAYS emitted: a lone Done folds to null in the overlay's reducer (invisible when nothing is showing), so cold starts stay flash-free — but any overlay that mounted late (snapshot catch-up) still gets a definitive end instead of waiting on a pass that will never tick again."* Plan step 1(f) required exactly this, and the new SPEC file documents it ("terminal Done ALWAYS emitted … late overlays get a definitive end").

The code below it still gates BOTH terminal events on `forwarded`:

```rust
if forwarded.load(std::sync::atomic::Ordering::Relaxed) {
    let terminal = match &pass { /* Done | Failed | Failed */ };
    emit_index_progress(&handle, &terminal);
}
```

On a cold start with a sub-second pass, `forwarded` never becomes true → **no terminal event is emitted at all**. The documented wedge-prevention ("a definitive end") does not exist. Combined with H2 this strands a seeded overlay permanently; even with H2 fixed it leaves the SPEC/plan/comment describing behavior the code doesn't have.

**Fix:** emit `Done` unconditionally on pass end (it is visually free — a lone `done` folds to null in the reducer and `hideDelayMs(null, …)` returns 0); keep `Failed` gated on `forwarded`. E.g.:

```rust
match &pass {
    Ok(Ok(stats)) => emit(&handle, &Done { summary: … }),
    _ if forwarded.load(Relaxed) => emit(&handle, &Failed { … }),
    _ => {}
}
```

**Doc nuance after the fix:** README's "sub-second passes emit nothing" becomes wire-inaccurate (a lone invisible `done` IS emitted); either reword to "emit nothing visible" or leave as-is since the user-visible claim ("reopens silently") stays true — but the SPEC/comment/code must agree.

### H2 — Snapshot published for gated cold passes → cold-start no-flash guarantee broken (+ undismissable wedge with H1)

**Where:** src-tauri/src/main.rs:1491 (`mark_startup_index_active()` unconditional), src-tauri/src/ipc/codegraph_cmds.rs snapshot helpers + `get_index_progress`, frontend/src/components/projects/IndexingOverlay.tsx:158–165.

`mark_startup_index_active()` runs for EVERY startup pass — cold or switched — and `record_startup_index_progress` records every tick regardless of the 1s gate. `get_index_progress` returns `Some((done, total))` whenever ACTIVE, with no notion of "this pass will actually be shown".

**Trace (cold start, already-indexed project):**
1. Cold start, `switched_open = false`. Task marks the snapshot ACTIVE; sub-second pass ticks are recorded but forwarded by nothing (gate).
2. In parallel, the webview boots: React mounts App's `IndexingOverlay`, the effect subscribes, then fetches `get_index_progress`.
3. If the fetch **lands inside the pass window** (racy — depends on webview boot vs. pass duration; realistic on fast machines / release builds / larger already-indexed repos), it returns `Some((k, N))` → `setState(prev => prev ?? {progress})` → **the overlay SHOWS on a cold start of an already-indexed project** — exactly the regression the plan and README forbid ("still reopens silently on a cold start").
4. With H1 unfixed the pass then emits NO terminal event (nothing was forwarded) → the overlay never hides. The `progress` state has **no Dismiss button** (only `failed` does), the modal is `fixed inset-0 z-50` over everything, and `useBrowserOverlay(true)` keeps the native child webview hidden → **the app is unusable until killed**.

Even with H1 fixed, step 3 alone breaks the guarantee: the seed shows, then `done` arrives and holds it ≥800ms → an ~800ms splash on a cold reopen, whenever the race hits.

**Fix (backend, preserves all three behaviors):** publish the snapshot only when the pass is actually visible — e.g. `mark_startup_index_active(switched: bool)` sets ACTIVE only when `switched`, and the progress callback flips ACTIVE on (after recording) on the first **forwarded** tick. Result: cold sub-second pass → snapshot never fetchable → silent (no flash, no wedge); cold slow pass → gate opens at 1s → snapshot becomes fetchable → catch-up works; switch → fetchable from the start.

**Regression test to add:** pin that a cold pass which never opens the gate never exposes the snapshot (e.g. mark(false) → record many ticks → snapshot None; mark(true) / mark-then-forwarded-tick → Some). Today's `startup_snapshot_records_and_clears` documents the opposite (mark → Some immediately), so this test forces the design decision.

### H3 — Seed fetch not invalidated by events that arrive mid-flight → post-`done` reseed can strand a frozen overlay

**Where:** frontend/src/components/projects/IndexingOverlay.tsx:158–168.

The seed callback checks only `disposed` and `snap === null`. It does not capture/compare the event token and does not observe that a terminal event may already have been processed:

1. Subscribe resolves; `getIndexProgress()` dispatched. The command executes on the backend **before** `clear_startup_index()` (pass still running) → returns `Some((k, N))`.
2. The pass ends; the terminal `done` event is delivered and handled first (state null → folds invisibly; `shownAt` null → immediate no-op hide). Invoke responses and events have no ordering guarantee, and during heavy mount the promise resolution can trail the event by a long margin.
3. The fetch response then resolves → `prev ?? snap` seeds `{progress}` with the pass already over → no future events will ever arrive → **frozen overlay with no Dismiss, on the switch path too** (switch passes always emit Done, but the seed lands after it was consumed).

The pathological variant (response delayed past the full 800ms hold → reseed after hide) is the same defect with worse latency.

**Fix (one line-ish):** capture the token at dispatch and bail if any event arrived during the fetch — events are always fresher than the snapshot:

```ts
const myToken = token;
void getIndexProgress().then((snap) => {
  if (disposed || snap === null || token !== myToken) return;
  …
});
```

This also makes the reducer's `prev ?? snap` guard redundant-but-harmless and closes both the reorder and the slow-response variants.

### L4 — Torn snapshot reads (two separate Relaxed atomics)

**Where:** src-tauri/src/ipc/codegraph_cmds.rs — `STARTUP_INDEX_DONE` / `STARTUP_INDEX_TOTAL` + `record_startup_index_progress` / `startup_index_snapshot`.

The two counters are stored/loaded as independent Relaxed atomics, so a concurrent `get_index_progress` can read a tick's new `done` paired with the previous tick's `total`: transiently `(done > total)` → frontend `pct = Math.round(done/total*100) > 100` (bar clipped by `overflow-hidden`, cosmetic) or `(k, 0)` (already guarded — renders the indeterminate stub + "…"). Nanosecond-scale window, cosmetic worst case. Suggest clamping in `startup_index_snapshot` (`done.min(total)`) or documenting the tearing as accepted.

### L5 — Unrelated untracked knowledge files in the tree

`git status` shows two untracked files that are NOT part of this plan: `.coding/knowledge/bug/bb8a0ca9.md` and `.coding/knowledge/decision/2026-08-27-browser-tools-visible-in-every-workflow-state-ap.md` (they reference plan bb8a0ca9 / commits a3f8c31, c7f2074). They are mergeable knowledge files and harmless, but the closing commit should not silently mix them into the switch-overlay change — either commit them deliberately (separate commit or explicitly noted) or leave them out.

### L6 — Misleading test title in indexingOverlay.test.ts

`it("a zero-window call degenerates to immediate hide", …)` asserts `hideDelayMs(1000, 1000) === MIN` — i.e. the FULL hold for a bar shown exactly now, the opposite of "immediate hide" (its inline comment even describes the pre-hold behavior). Rename, e.g. "shown exactly now → hold the full window". Assertion itself is correct.

## Constitution / test-matrix checklist

- **Doc comments on all new public items:** present (Rust: `IndexProgressSnapshot` struct + fields, `get_index_progress`, and the pub(crate) helpers; TS: `IndexProgressSnapshot`, `getIndexProgress`, `MIN_VISIBLE_MS`, `hideDelayMs`). ✓
- **No `#[allow]`:** none added. Statics/helpers all used (no dead code). ✓
- **Warning-free:** claimed green (root `cargo test` 1564 passed, src-tauri build clean, frontend 643 passed + `tsc --noEmit` clean); nothing in the diff suggests a warning; not re-run (read-only) — re-run required anyway after fixing H1–H3.
- **Regression tests:** gate table + snapshot lifecycle + wire shape + hideDelayMs table present. **Gap:** no test pins the cold-start invisibility contract (see H2's suggested test) — that test must fail before the H2 fix and pass after. After H3's fix, a reducer/effect-level test isn't feasible in the node environment (no effect runner), so the token-guard fix is covered by H2's backend test + code review.
- **README/SPEC sync:** README updated; SPEC successor written and -3 marked superseded; memory record superseded. ⚠️ The SPEC text documents "terminal Done ALWAYS emitted" — true only after the H1 fix; keep SPEC as-is once H1 lands (code must match docs, not the other way).
- **PLAN.md:** no row needed, nothing contradicts. ✓
- **Multi-platform neutrality:** clean (see verified list). ✓
- **Security:** read-only counters, no approval bypass. ✓

## Suggested fix order

1. H1 (always-Done, Failed stays gated) — small, self-contained, makes code match comment/plan/SPEC.
2. H2 (snapshot published only for visible passes: switched-at-mark OR first forwarded tick) + its regression test.
3. H3 (token guard on the seed fetch).
4. L4/L6 while touching the same files; decide L5 before committing.
Then re-run the full matrix (root `cargo test` unpiped + `$LASTEXITCODE`, src-tauri `cargo build`, frontend `npm test` + `tsc --noEmit`) before the closing commit.
