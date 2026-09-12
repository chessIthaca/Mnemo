## Verdict: FINDINGS (0 high, 2 low)

Round-3 authoritative verification of plan 75deaec0's full change set (59fc685 + ca7c152, wt/agenticcoder, HEAD = ca7c152). **The High 1 fix is correct, complete, and properly regression-tested; the three LOW fixes from 59fc685 are all in place; hygiene is clean.** Two low findings: the HOW record's gate description is one fix behind (missing the `!is_subagent()` guard), and the deferred root-cause design option deserves a backlog note. One commit note: the BUG record's "three halves" update is UNCOMMITTED in the working tree (verified accurate — it must land in the final commit).

## Verification detail

### 1. High 1 fix (ca7c152) — VERIFIED correct and complete

The gate (src/runtime/agent.rs:244-249) now reads all four conditions:

    if self.auto_continue_streak < MAX_AUTO_CONTINUE
        && self.agent_loop.workflow_expects_progress().await
        && !self.agent_loop.has_running_descendants().await
        && !self.agent_loop.is_subagent()

- **The guard is state-agnostic and terminal.** `is_subagent` is `Mutex<bool>` (loop_impl.rs:158), default `false` (loop_impl.rs:646), stamped `true` for every parented loop by the spawner (src-tauri/src/ipc/spawn.rs:141-151; `set_is_subagent(true)` at :150 — exactly the call site the round-1 report cited). Whichever state `Workflow::load_latest` derives from the shared main plan, a subagent now fails the gate and parks on a normal turn end. The **Reviewing variant** (introduced by 59fc685) is pinned by the new test; the **Executing variant** (pre-existing, the child-side counterpart of bbd712c9) is closed by the same single condition — the guard does not consult state, so covering one variant covers both. All four predicates are pure reads (no side effects), so evaluation order is immaterial.
- **Mechanism chain re-verified link by link at HEAD:** parented subagents share the main plans dir (spawn.rs:126-131, `own_plans_dir=false` → `factory.build_with_id`); `build_inner` gives every spawned agent a fresh Workflow that calls `load_latest()` (factory.rs:603-612); `load_latest` derives state from the on-disk plan (mod.rs:985-995: no plan → Planning, complete + unreviewed → Reviewing, else Executing). Exactly as the amended round-1 report and the ca7c152 commit message describe.
- **Main agent unaffected.** `is_subagent()` defaults to false and is only set by the spawner when `parent_id.is_some()`; `auto_resume_fires_when_workflow_is_reviewing` (agent.rs:5582-5678) builds an unstamped loop, drives the workflow to Reviewing exactly as before (create_plan_with_kind(Implementation) + complete_step(0), state asserted in-test at agent.rs:5614-5618), and asserts the auto-continue fires (calls >= 2). Pre-existing bounds intact: streak increments only inside the arm, MAX_AUTO_CONTINUE still 12, descendant park unchanged, fresh Prompt still resets the streak.
- **Single producer confirmed.** The exact string "[harness note] continue from where you left off." has exactly one producer — the gated None-arm (agent.rs:262-264). The only near-match (agent.rs:332-335) is the compact-resume note ("Context was compacted mid-task — continue from where you left off."), a different mechanism (fires after Compact, not on a normal turn end) — unaffected by this fix.
- **Tests:** the ca7c152 commit message reports 1948 passed / 0 failed / exit=0 (59fc685 reported 1947/0; +1 = the new subagent test — consistent). I could not independently re-run the suite (read-only reviewer, no shell); verification is code-level by reading HEAD, the same posture as rounds 1-2. Zero warnings under `#![deny(warnings)]` is consistent with the reported green run; no `#[allow]` anywhere in src/ (the only textual match is a prose mention in a doc comment, loop_impl.rs:571).

### 2. Regression test — VERIFIED: exercises the changed path, fails pre-fix

`auto_resume_does_not_fire_for_subagents` (agent.rs:5680-5787):
- Setup reproduces the live scenario end to end: a workflow driven to the state a closing-sequence reviewer's `load_latest` derives (create_plan_with_kind(Implementation) + complete_step(0) → Reviewing; the sibling test asserts the transition explicitly), a loop stamped `set_is_subagent(true)` (agent.rs:5739, mirroring spawn.rs:150), one task prompt.
- It waits for the first provider call, holds a 400 ms settle window, then asserts `calls == 1` — the same idiom as the established `auto_resume_parks_while_descendants_running` test (agent.rs:5542-5547).
- **Pre-fix failure mode (by code reading):** the old three-condition gate evaluates `streak(0) < 12` ✓, `workflow_expects_progress()` → true (Reviewing) ✓, `!has_running_descendants()` → true (no tracker wired) ✓ — the arm fires, the second provider call lands inside the settle window (auto-continue is immediate, same loop iteration), and `assert_eq!(calls, 1)` fails. The task record confirms it failed empirically pre-fix with the full 13-call plateau (12 synthetic + 1 task turn). The assertion sits on the exact line ca7c152 changed — a direct pin of the changed path, not adjacent behavior.
- The test builds the loop directly (AgentLoop::new + set_is_subagent) rather than via `spawn_agent_shared`. That is consistent with the module's test style (the bbd712c9 tests mock the descendant tracker the same way) and correctly isolates the gate semantics; the spawner stamping it relies on is verified by reading spawn.rs:141-151. Robustness note: even if the setup state drifted to Executing, the pre-fix gate would still fire (workflow_expects_progress covers both) — the test pins the guard, not the state.

### 3. The three LOW fixes from 59fc685 — all still in place

- **Low 1 (call-site comment):** agent.rs:220-223 — "If the workflow is in Executing or Reviewing state (an in-progress plan or the closing sequence), push a synthetic 'continue'…" ✓
- **Low 2 (doc comment):** loop_impl.rs:1566-1567 — "Any other state (Planning, Complete, Skill) is waiting/terminal/overlay — no synthetic turns there." ✓
- **Low 3 (HOW record):** the record's KEY CORRECTNESS FACTS (line 14) now describes `workflow_expects_progress()`, the Executing-AND-Reviewing coverage, the rename from is_workflow_executing, and the bbd712c9 descendant park ✓ — but it is now one fix behind again: see Finding Low 1 below (missing `!is_subagent()`).

### 4. Hygiene — CLEAN

- **No dangling `is_workflow_executing` references in source.** The only src/ textual match is agent.rs:5586 — a comment inside the regression test narrating the OLD gate ("the auto-continue None-arm gated only on is_workflow_executing()") — correct historical narration, not a live reference. All other matches are `.coding/` historical records (bug/plans/reviews) that legitimately describe the old gate as history. (The code-graph index still resolves the removed symbol — stale gitignored codegraph.db, same note as rounds 1-2; it rebuilds on next project open.)
- **No `#[allow]`** in src/ — the single textual match is a prose mention in a doc comment (loop_impl.rs:571).
- **Multi-platform neutral:** pure Rust logic — a `Mutex<bool>` flag, a `matches!` on an enum, tokio tests with tempdir. No paths, no OS APIs, no shell syntax.
- **No unrelated changes:** 59fc685 = loop_impl.rs + agent.rs + BUG record + HOW record + plan file + round-1 report; ca7c152 = agent.rs (gate + EXCEPTION 2 comment + test) + round-1 amendment + round-2 addendum. Every hunk is on-topic; the review-report amendments are the reviewers' own post-delivery observations landing with the fix, as both commit messages document.

### 5. Amended round-1 report vs shipped fix; BUG record — VERIFIED

- The amended round-1 High 1 recommended: "Minimal: gate the auto-continue arm on `!is_subagent()` — the flag already exists (spawn.rs:150 `set_is_subagent(true)`); subagents are single-task, so a normal turn end should park them and let the child-completion Suggestion resume the parent", plus "a regression test that spawns a subagent whose workflow loads a Reviewing-state plan and asserts it parks". The shipped fix is exactly that, verbatim in mechanism: the guard, the EXCEPTION 2 comment documenting rationale and consequences (agent.rs:234-243), and the test. The report's impact split (Executing variant pre-existing / Reviewing variant introduced by 59fc685) matches the ca7c152 commit message's own attribution.
- **BUG record** (.coding/knowledge/bug/2026-12-31-closing-sequence-turn-parks-until-manual-c-garbl.md): the working-tree version accurately reflects all three halves — (1) the park fix (59fc685), (2) the orphan/garble audit (not reproducible on current code; stale-binary explanation), (3) the subagent self-continue (ca7c152) with mechanism, fix, regression test, the deferred design option, and the round-2 supersession note. Accurate and complete — but the update is uncommitted; see the Commit note.

### 6. Deferred root-cause fix — deferring is reasonable; backlog note recommended (Finding Low 2)

The alternative (skip `load_latest()` for parented subagents so they start in Planning) was deliberately not taken. Assessment: **the minimal gate fix was the right call, and the root-cause fix as stated is riskier than it looks**:
- The guard closes both variants with one state-agnostic condition and changes nothing else.
- Skipping load_latest would have three observable side effects beyond the stated get_workflow_state concern:
  1. `get_workflow_state(agent_id)` (src-tauri/src/ipc/agent.rs:667-693) feeds the UI per-agent state + plan + the ancestor staircase — subagent tabs would show Planning with no plan.
  2. The per-context model resolver resolves by workflow state + is_subagent (loop_impl.rs:1009/1076, model_resolver.rs:247) — state-based model selection for subagents would silently change.
  3. **`current_plan` returns "no active plan" in Planning** (pinned by `current_plan_returns_no_active_plan_in_planning`, src/tool/workflow/plan.rs:3007) — and the reviewer toolset explicitly includes current_plan (spawn.rs:167-172: "only the listed tools + current_plan are visible"). A reviewer that skipped load_latest could no longer read the plan under review via its sanctioned tool — a functional regression for the most common subagent type, not a cosmetic trade-off.
- Conclusion: deferring is not merely reasonable — the root-cause fix as stated should be rejected or redesigned (e.g. load the plan read-only without deriving a progress-expecting state) rather than simply deferred. The underlying design question ("should parented subagents share the main plan's state at all, now that auto-continue no longer consumes it?") is real and currently lives only inside a closed bug record; a one-line backlog note keeps it discoverable. See Finding Low 2.

## Findings

### Low 1 — HOW record's gate description is one fix behind: missing the `!is_subagent()` guard (knowledge hygiene)

`.coding/knowledge/how/2026-08-31-re-add-dropped-auto-continue-budget-test-via-sma.md:14` — "KEY CORRECTNESS FACTS (updated 2026-12-31, plan 75deaec0)" describes the gate as `if streak < MAX && workflow_expects_progress() && !has_running_descendants() { streak+=1; continue }`. ca7c152 added a fourth condition — `&& !is_subagent()` — which the record does not mention; a future agent reconstructing the gate from this record would omit the subagent guard and reintroduce High 1. Same staleness class as round-1 Low 3 (which this very plan fixed for the previous gate change), and this record is the living reference for the auto-continue budget test. Fix: extend the gate description to `... && !has_running_descendants() && !is_subagent()` and add one sentence — subagents ALWAYS park on a normal turn end (single-task; the child-completion Suggestion resumes the parent) — ca7c152, High 1 of the 2026-12-31 auto-continue-reviewing review.

### Low 2 — Deferred design option (subagent plan-state sharing) has no backlog entry

Deferring the root-cause fix is sound (see §6), and the BUG record documents the deferred option — but a closed bug record is history, not a queue. The design question (should parented subagents load and derive the main plan's state at all, given it now feeds only get_workflow_state, the model resolver, and current_plan?) deserves a one-line backlog note so a future session can pick it up deliberately — likely outcome: keep the sharing but make it explicit/documented rather than accidental. Fix: one backlog_add note referencing the BUG record and §6 of this report.

## Commit note (not a finding — action required in the closing commit)

The working tree is NOT clean (contrary to the task brief): `.coding/knowledge/bug/2026-12-31-closing-sequence-turn-parks-until-manual-c-garbl.md` carries the uncommitted "Two halves → Three halves" update (verified accurate in §5). That is the normal closing-sequence flow (record + review land together), but it MUST be included in the final commit alongside this report and the Low 1 fix — do not let it drop.

## Posture notes

- Read-only reviewer: no shell — `cargo test` could not be re-run; 1948 passed / 0 failed / exit=0 is per the ca7c152 commit message (consistent with 59fc685's 1947/0 + the one new test). All code-level verification above is from reading HEAD directly.
- The round-2 PASS (.coding/reviews/2026-12-31-auto-continue-reviewing-review-round2.md) verified only the pre-amendment finding set; its own addendum records the live subagent self-continue the round-2 reviewer experienced. It is superseded by this round-3 verification of the full amended finding set — no reliance placed on it here.

## Post-delivery addendum — live event, DOES NOT change the verdict (stale binary, not a code gap)

**Event:** Immediately after this review was delivered (report written, final answer sent), the round-3 reviewer's own loop received a synthetic "[harness note] continue from where you left off." self-continue turn — the exact High 1 phenomenon, in a session whose source tree is at ca7c152 (the fix).

**Diagnosis: the running app binary predates ca7c152.** The fix is present and correct in the source under review; the serving process was built before it. Evidence chain, verified this turn:

1. The exact string has exactly one producer — the auto-continue None-arm (agent.rs:262-264); the only near-match is the compact-resume note (agent.rs:332-335), a different string with a different trigger.
2. On current code that arm cannot fire for this reviewer: the stamping is unconditional for parented loops (spawn.rs:141-151 — `if parent_id.is_some() { … agent_loop.set_is_subagent(true); }`, no role/model/other conditions), and every spawn path funnels through `spawn_agent_shared` (callers: the `spawn_agent` IPC command, both `spawn_with_parent` variants, console `start`, `main`) — there is no bypass path that builds a parented loop unstamped.
3. This reviewer runs with the subagent restrictions (plan tools unavailable; reviewer toolset) that the parented spawn path applies — the same `if parent_id.is_some()` block that stamps `set_is_subagent(true)` on current code — so the block demonstrably executed for this loop.
4. Gate evaluation for this loop at the delivered turn's end: streak 0 < 12 ✓, `workflow_expects_progress()` → true (main plan 3/4 steps, "Verify" unchecked → Executing) ✓, no descendants ✓, `is_subagent()` → true → `!is_subagent()` fails → park. On current code this reviewer parks; the observed note therefore originated from code without the guard.
5. Corroboration: the round-1 reviewer's live hit (pre-fix source), the round-2 reviewer's live hit (its addendum), and the BUG record's half (2) "stale-binary explanation" — the running app has lagged the source under review throughout this plan's review rounds.

**Remedy (operational, not a code change):** rebuild and restart the app to pick up ca7c152 before drawing live-behavior conclusions. The semantics are already pinned by `auto_resume_does_not_fire_for_subagents` (fails pre-fix with the 13-call plateau, passes post-fix), so no new test is needed; a live confirmation after restart (spawn a reviewer, observe it park) is cheap reassurance. Expect further synthetic turns from THIS reviewer's tab until the restart — they are the stale binary's known symptom, not a regression.

**Verdict unchanged: FINDINGS (0 high, 2 low).** This event is an environment condition (process older than the fix), not a defect in the change set; it reinforces rather than undermines the round-3 verification.
