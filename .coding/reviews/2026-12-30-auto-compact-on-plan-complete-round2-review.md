## Verdict: FINDINGS (1 high, 0 low)

Round-2 verification of the four round-1 fixes for plan 7369d7f3, committed as 06aca97 on `wt/agenticcoding` (working tree verified clean at HEAD).

**Summary:** All four round-1 fixes are correctly implemented in the production code — HIGH-1's `compacting` field additions are complete and exhaustive, HIGH-2's run-state re-check is correctly scoped with no deadlock, LOW-3's `current_item` clear + `item_resolved` gate are correctly ordered, LOW-4's README paragraph is in, and the round-1-verified core design is fully intact. **But the fix commit broke the test build:** the updated regression test `auto_compact_gate_is_spawned_and_run_all_only` uses `gate` before its declaration — a hard E0425 compile error in the `#[cfg(test)]` module. `cargo test` cannot pass on 06aca97; the parent's "cargo test workspace 2018+16 passed / 0 failed" claim is false for the committed state (the run must predate the final LOW-3 test edit). A two-line reorder fixes it.

---

### HIGH-1 (round 2): Regression test does not compile — `gate` used before its declaration

`src-tauri/src/ipc/run_all.rs`, test `auto_compact_gate_is_spawned_and_run_all_only` (lines 821–830, confirmed by three independent reads: `git show 06aca97`, direct file read, literal search):

```rust
821:        let clear = body
822:            .find("*r.current_item.lock().expect(\"current_item lock poisoned\") = None;")
823:            .expect("must clear current_item once the item resolves");
824:        assert!(
825:            clear < gate,            // ← `gate` is NOT yet in scope
826:            "the in-flight pointer must be cleared before the auto-compact window opens"
827:        );
828:        let gate = body             // ← first and ONLY binding of `gate` in the file
829:            .find("auto_compact_on_plan_complete")
830:            .expect("the setting read must exist");
```

Rust has no hoisting: the `assert!(clear < gate, …)` at :824–827 references `gate`, whose only binding anywhere in the file is the `let gate` at :828 — E0425 "cannot find value `gate` in this scope". Blast radius: `mod tests` is `#[cfg(test)]`-gated (run_all.rs:295), so `cargo build` still succeeds, but the src-tauri test target — and with it the whole workspace `cargo test` (root Cargo.toml:150 `members = ["src-tauri"]`) — fails to compile. Consequences:

- The claimed "cargo test workspace 2018+16 passed / 0 failed" cannot be true of 06aca97. Whatever was run, it was not this commit's test suite — the run evidently predates the edit that inserted the `clear`/`gate` assertions (the LOW-3 fix). This also violates the project constitution ("Run cargo test before marking a workflow step complete").
- The test meant to pin the LOW-3 contract (clear-before-gate + `item_resolved` gate + run-all-only placement) is currently dead code — none of its pins are enforced, so LOW-3 has no live regression guard until this is fixed.

**Fix (two-line reorder):** move the `let gate = body.find("auto_compact_on_plan_complete").expect("the setting read must exist");` binding above the `assert!(clear < gate, …)`. Then actually re-run `cargo test` from the repo root (unpiped, read `$LASTEXITCODE`), plus `npx tsc --noEmit` and `npx vitest run` in `frontend/` to re-confirm the other two claims.

---

## Round-1 fix verification (all four correct in the production code)

### HIGH-1: Frontend type-check — FIXED, verified

`RunAllProgress.compacting` is required (frontend/src/lib/types.ts:46–53). Exhaustive search for construction sites (`runAll: {`, `setRunAll(`, `RunAllProgress {` across frontend and src-tauri): exactly four frontend object literals — useAgentStore.ts:677 (store initializer), useAgentStore.test.ts:41, useAgentStore.preview.test.ts:161, ipc-contract.test.ts:110 — all now carry `compacting: false`. No other construction site exists (`setRunAll` is only declared at useAgentStore.ts:570 and passed through at :865; BacklogView only reads). The one remaining literal without the field — preview.test.ts:168's `run_all: { active: true, done: 2, total: 5 }` — is `as any`-cast into `applyBacklogChanged` (:170, no TS2741) and its `toEqual` at :175 passes because the stored object never gains a `compacting` key (absent key, not a defined one). Rust side: both `RunAllProgress` arms in `emit_backlog_changed` (backlog_cmds.rs:122/:128) and the contract fixture (contract_fixtures.rs:205) carry the field. `npx tsc --noEmit` clean is statically plausible — no construction site is left that could fail it (not re-runnable in this reviewer's toolset: no shell).

### HIGH-2: Stop/halt during the compaction window — FIXED, verified

`compact_then_dispatch_next` (run_all.rs:1127–1188) now re-checks the run state between the wait and the dispatch (:1166–1169): the `run_all` guard is taken and **dropped inside a block expression**, then `None` ⇒ log + no dispatch (:1170–1175), `Some(true)` (stop requested) ⇒ `end_run` (:1176–1181), `Some(false)` ⇒ dispatch (:1182–1186). **No deadlock:** `end_run` (run_all.rs:129–132) and `run_all_dispatch_next` (:1272) each lock `run_all` themselves — the re-check's guard is already dropped (block scope) before either is awaited. The steer case lands in the `None` arm (`halt_run_all` → `end_run` → `run_all = None`) — the round-1 orphan-dispatch scenario is gone. `Some(true)` → `end_run` matches the stopped path in `on_main_turn_resolved` (:1612–1615). The residual race (a stop landing after the re-check but before the dispatch) is the pre-change microsecond window — acceptable.

### LOW-3: Stale `current_item` across the window — FIXED, verified

The done-counter bump block (:1596–1608) now clears `current_item` (`*r.current_item.lock()… = None`, :1607) as soon as the item terminally resolves — **before** the auto-compact window opens. The gate is `let auto_compact = item_resolved && setting` (:1629–1637), with `item_resolved` set (:1609) only after a terminal disposition (Done via the plan-loop gate, or Failed via abandonment); the gate-fail (:1560–1569) and terminal-error (:1580–1594) arms return early and leave it false. Verified consequences: (a) an interactive chat turn during the window finds `current_item == None`, skips the item block (:1520), and dispatches directly with `item_resolved == false` — no annotate-and-halt on the Done item, no double Done transition, no done-counter overshoot, and **no second compact task** (the round-1 residual is resolved); (b) a normal Done resolution still compacts — the gate does not skip it; (c) the deferral path's `None` handling is untouched and consistent — `run_all_dispatch_next` itself sets `current_item = None` when deferring (:1325–1330), and the resolution path's `None` arm (:1506–1517) is exactly the path an in-window interactive turn now takes. Lock ordering is uniform at all five `current_item` sites (run_all tokio Mutex → current_item std Mutex, never the reverse; no await under the std Mutex) — no deadlock.

### LOW-4: README — FIXED, verified

README.md:123's config paragraph now documents `[general].auto_compact_on_plan_complete` — default off, Settings → Advanced, compacts the main agent between Run-All items after each completed plan, failed/timed-out compaction logs and proceeds. Matches the implemented behavior.

---

## Test-contract assessment

- `compact_then_dispatch_next_orders_compact_wait_dispatch` (:847–893) — correctly updated: pins subscribe < send < wait < dispatch, plus the HIGH-2 contract (`r.stop.load(Ordering::Relaxed)` found after the wait and before the dispatch; `end_run(&app, &state)` present). Removing the re-check or the end-run arm fails the test. Good pin.
- `auto_compact_gate_is_spawned_and_run_all_only` (:798–845) — the pins are the right ones (setting gate, spawn-not-inline, `let auto_compact = item_resolved`, clear-before-gate, gate < dispatch, gate < auto_feed) and would catch regressions of LOW-3 and the run-all-only placement — **but it does not compile** (HIGH-1 above), so none of its pins are currently enforced.
- `forwarder_signals_compact_completion_for_main_agent` (:895–933) and the three behavioral `wait_for_compact_signal` tests (:935–974) — unchanged from round 1 (verified sound there), still coherent against the current source (events.rs:346/:475/:478/:493/:660).
- Config tests (general.rs defaults/round-trip, settings_dto.rs patch applies/absent-keeps) — present and correct.

## Round-1 core design intact — nothing reverted

Spawn-not-inline (run_all.rs:1639); subscribe-before-send (:1129–1131); the shared watch counter with pairing-guarded, main-agent-only forwarder increment (events.rs `compact_in_flight`); every-failure-path-proceeds (no main agent :1145, refused send :1141, timeout :1151, channel close via `changed()` → `Err`); run-all-only gating (branch placement before the single-dispatch/auto-feed path :1649+); the `compacting` progress line (flag + `emit_backlog_changed` on both flips :1132–1133/:1158–1159; BacklogView " · compacting…" rendered only inside the `runAll.active` span). All three `BacklogContext` construction sites (main.rs:450/:575/:652) carry the new fields.

## Verification-claims assessment

- "cargo test workspace 2018+16 passed / 0 failed" — **falsified**: the committed test module cannot compile (HIGH-1). Not re-runnable here (no shell in the reviewer toolset), but the E0425 is provable from source alone.
- "npx tsc --noEmit clean" — statically plausible; every `RunAllProgress` construction site carries the field and no other site exists (exhaustive search).
- "vitest 971 passed" — statically plausible; the only subtle case (preview.test.ts:175 `toEqual` without `compacting`) passes because the `as any` payload never sets the key.

**Required before finish:** fix HIGH-1 (two-line reorder in the test), then re-run the full suite (`cargo test` workspace, `npx tsc --noEmit`, `npx vitest run`) and commit the fix on `wt/agenticcoding`.
