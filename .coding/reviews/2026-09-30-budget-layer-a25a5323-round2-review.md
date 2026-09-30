## Verdict: FINDINGS (0 high, 1 low)

Round-2 delta review of backlog a25a5323 / plan a1047bff, scope strictly `git diff 6eff28a..HEAD` — the two fix commits `219ed81` (round-1 L1: clear stale spend stamp on planless turns) and `969a867` (round-1 L2: cost report counts model requests, not evidence rows). Both round-1 findings are fixed correctly; one new LOW cosmetic defect was introduced by the L2 fix. The uncommitted remainder is bookkeeping only (backlog status flip to `in_flight` + the two untracked `.coding/` records) — accurate as-is, no code carry-over.

## What was read

- `git log` (branch tip 969a867), `git show 219ed81`, `git show 969a867`, `git diff HEAD` + `git status` (only `.coding/` noise, one line).
- Context reads around the delta hunks: turn.rs stamp site + budget-gate ordering (:430-569), `read_plan_scope` (:1370-1414), the usage site `record_spend_row` (:3725-3779); loop_impl.rs slot helpers (:1020-1073); budget.rs full `cost_report` (:265-370) + the updated tests; plan.rs finish-append test assertions.
- Docs cross-check: README.md:43 (Optional Laya budget sentence), PLAN.md:1395-1415 (budget bullet), budget.rs module/render doc comments.

## Round-1 L1 fix (219ed81) — verified correct

1. **The clear runs exactly on the planless path.** The else branch fires only when `read_plan_scope()` returns `None`, which by definition (turn.rs:1372-1381) means no plan is active (chat turns / subagents run planless). The pause/pending-question flow cannot be disturbed: `budget_gate` runs BEFORE the stamp site and returns an early outcome only when a cap fires — which requires a live plan (gate returns immediately at :1413 with `plan_scope` None), so a pausing turn never reaches the clear. The disabled-budget path is symmetric and correct: stamping/clearing is independent of the budget flag (the ledger rows are written regardless; the flag gates only the caps), and the clear sits on the same flag-independent path.
2. **Slot semantics stay sound.** `clear_spend_attribution` (loop_impl.rs:1061) uses the identical `if let Ok(mut slot)` lock pattern as `set_spend_attribution`; `take_spend_attribution` remains consume-once via `slot.take()`. A stale stamp can now only survive while a plan is live, where the next iteration overwrites it and finish/abandon consume it — the round-1 report's noted dormant residual, not a new issue. No path leaves a stale stamp consumable by a planless turn.
3. **The regression test is non-vacuous.** It seeds the slot with a `dead-plan` stamp, runs a planless turn against a usage-reporting provider (700+30), and asserts the INLINE counters deterministically first (`total_tokens()==0`, `plan_id==None`) — these are the exact assertions behind the recorded red-before proof ("left: 730, right: 0" with the clear disabled), so the test is red without the fix without depending on timing. It then sleeps 100 ms and asserts `spend_rows_for_plan("dead-plan")` is empty; in the pass case no spawned write exists at all, so the sleep only guards the fail case. It also asserts the provider actually served the turn (`!outcome.text.is_empty()`), ruling out a vacuous early-exit pass.

## Round-1 L2 fix (969a867) — verified correct

4. **Totals and per-lane counts derive from model requests only** (`billed` = rows with `reason != "verify"`); token sums for non-verify rows are unchanged (verify rows carried no in/out tokens — the plan.rs cached-total change 400→300 comes from that fixture's verify row carrying `cached_tokens`, which the filter now correctly excludes). The Totals line uses `billed.len()`.
5. **Verify rows still listed under "Deterministic checks"** — that section (budget.rs:361) still filters `rows` on `reason == "verify"`, untouched.
6. **Unregressed sections confirmed by reading:** lane-per-step (route-only filter, :319-328), Retries (:338), Escalations (:347) all still iterate `rows`. The verify-only ledger case now renders "0 request(s)" and is asserted.
7. **Tests updated on both sides:** budget.rs fixture gained a non-verify `default` lane row so the new arithmetic is exercised (5 requests / 4700+750 / 350-token no-lane bucket, plus `!report.contains("6 request(s)")`), and the plan.rs finish-append test follows the same corrected numbers.

## Finding

**LOW 1 — dangling "Per lane:" header on a verify-only ledger (budget.rs:311, introduced by 969a867).** `out.push_str("\nPer lane:\n")` is emitted unconditionally. Before the delta, non-empty `rows` implied at least one lane entry, so the header always had content; with the `billed` filter, a verify-only ledger (all rows `reason == "verify"`) renders the section header with nothing under it — e.g. a plan whose only rows are harness-verify checks gets a bare "Per lane:" line between the "Totals: 0 request(s)" line and "Deterministic checks". Cosmetic, same class as round-1 L2; the verify-only test asserts "0 request(s)" but not the absence of the empty header. One-line fix: emit the header only when `!lane_rows.is_empty()` (and optionally extend the verify-only test to assert `!report.contains("Per lane")`).

## Constitution checks

- **Documentation sync** — budget.rs render doc comment updated in-delta to document the verify-row exclusion. README.md:43 ("tokens per lane, lane per step, retries, escalations, deterministic checks, cache hits; real numbers only") and PLAN.md:1406-1410 ("totals, tokens per lane, lane per step, …") make no request-count claim the semantic change invalidates — no stale wording found.
- **Multi-platform neutrality** — pure Rust, no platform APIs, paths, or shell syntax in either commit. Holds on macOS and Windows.
- **File-tools-first** — no shell-mutation artifacts in the delta; edits are code hunks only.
- **Warning-free build** — no `#[allow]` in the delta; `clear_spend_attribution` is called (turn.rs:537), `billed` fully used. Parent's post-fix runs: root `cargo test` 2969 passed / 0 failed, src-tauri `cargo test` 332 passed / 0 failed — green under `#![deny(warnings)]` proves zero warnings. No new dependencies (no Cargo.toml/Cargo.lock in the delta).
- **Bookkeeping accuracy** — `.coding/backlog.jsonl` uncommitted hunk flips item a25a5323 to `in_flight` with plan a1047bff (accurate — not finished); the untracked plan file and round-1 review record match what shipped.

Reviewed-state: 969a8676de71e99f262330a85c9d6f28c3806efb
