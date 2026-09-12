## Verdict: FINDINGS (1 high, 3 low)

The core fix is correct, minimal, and properly regression-tested for its stated goal — the main agent's closing sequence now self-recovers in Reviewing, with all bounds (MAX_AUTO_CONTINUE, descendant park, streak placement) preserved. **Amended post-verdict:** immediately after this review was delivered, the reviewer subagent itself received a live "[harness note] continue from where you left off." auto-continue turn. Investigation traced it to a subagent-side auto-continue path that this diff newly extends to Reviewing-state subagents — the textbook closing-sequence reviewer now self-continues after delivering its report. That is **High 1** below (observed live for the Executing variant; the Reviewing variant is verified by code chain). The other three findings are documentation/knowledge-hygiene items. No defect in the changed lines themselves.

**Scope reviewed** — full `git diff HEAD` (`src/agent/loop_impl.rs` +15/−8, `src/runtime/agent.rs` +99/−1) plus untracked `.coding/knowledge/bug/2026-12-31-closing-sequence-turn-parks-until-manual-c-garbl.md` and `.coding/plans/75deaec0.md`. `git status --short` confirms exactly these four entries — no other modified or untracked files.

## Findings

### High 1 — Gate change extends auto-continue to subagents that load the main plan's state: a closing-sequence reviewer now self-continues after delivering its report (observed live during this review)

**Symptom (live, this session):** this reviewer finished its task, wrote the report, and ended its turn with a final answer — then received "[harness note] continue from where you left off." as its next input. That exact string has exactly one producer in the codebase: the auto-continue None-arm (agent.rs:252). The note fired on the *subagent's own loop*.

**Mechanism (every link verified):**
1. Tool-spawned subagents share the **main** plans dir (spawn.rs:126-131: `own_plans_dir=false` → `factory.build_with_id`).
2. `build_inner` gives every spawned agent a fresh Workflow that calls `load_latest()` (factory.rs:603-612) — by design, "load the latest plan from disk so a newly-spawned agent resumes an in-flight plan."
3. `load_latest` derives state from the on-disk plan (mod.rs:985-995): unchecked steps → **Executing**; complete + unreviewed → **Reviewing** (asserted by `load_latest_resumes_executing`, mod.rs:1541, and `load_latest_detects_reviewing_when_unreviewed`, mod.rs:1558).
4. Subagents run the same `run_turn_with_retry` None-arm (agent.rs:233-257); the gate has **no `is_subagent` guard**, and a subagent has no descendants, so `!has_running_descendants()` is always true.
5. When the subagent's single task ends normally (Finish::Stop → StopReason::None): streak < 12 ✓, `workflow_expects_progress()` → true, no descendants → **auto-continue fires on the finished subagent**, up to MAX_AUTO_CONTINUE=12 synthetic turns.

**Impact split:**
- *Subagent spawned while the plan is in-flight* (this session: plan 3/4 on disk → Executing): self-continue fires under the **old gate too** — a pre-existing, undocumented child-side counterpart of the bbd712c9 parent-spam bug. Not caused by this diff, but deserving its own bug record.
- *Subagent spawned during Reviewing* (the textbook closing-sequence reviewer: plan complete, unreviewed): the **old** Executing-only gate returned false → the reviewer parked cleanly after its report. The **new** `workflow_expects_progress()` returns true → the reviewer self-continues. **This regression is introduced by this diff**, and it hits the most common subagent type in the exact window the plan was fixing.

**Consequences:** up to 12 synthetic LLM turns after the report is delivered (token burn); a nudged reviewer can mutate its own deliverable (`write_review_report` is in its toolset — an append/overwrite on a synthetic turn corrupts the report); and the parent's descendant-park plausibly holds until the chain exhausts, delaying the very closing sequence this plan protects.

**Recommended fix** (either, plus a regression test that spawns a subagent whose workflow loads a Reviewing-state plan and asserts it parks):
- Minimal: gate the auto-continue arm on `!is_subagent()` — the flag already exists (spawn.rs:150 `set_is_subagent(true)`); subagents are single-task, so a normal turn end should park them and let the child-completion Suggestion resume the parent.
- Root-cause: parented subagents shouldn't resume the main plan at all — skip `load_latest()` (start in Planning) when `parent_id.is_some()`, since they cannot mutate plans anyway (spawn.rs:141-146).

**Note:** this does not contradict the verification below — those checks examined the main-agent path, which is correct. The gap is that the gate is shared by *all* agents' loops, while subagent workflows load the main plan's state from the shared plans dir.

### Low 1 — Stale call-site comment above the changed gate (doc-sync)
`src/runtime/agent.rs:220-226` — the None-arm comment still reads "If the workflow is still in **Executing** state (an active plan with steps to complete), push a synthetic 'continue'…". The gate one line below now covers Executing **or Reviewing** (`workflow_expects_progress`), so the comment directly above the changed line is now inaccurate — the same doc-sync class prior reviews flagged. Fix: reword to "in Executing or Reviewing state (an in-progress plan or the closing sequence)".

### Low 2 — New doc comment enumerates the parking states incompletely (doc-sync)
`src/agent/loop_impl.rs:1566-1567` — "Planning and Complete are waiting/terminal states — no synthetic turns there." `WorkflowState` has a fifth variant, `Skill` (mod.rs:43-48), which also parks; the sentence reads as if Planning/Complete were the only other states. Fix: e.g. "Any other state (Planning, Complete, Skill) is waiting/terminal/overlay — no synthetic turns there." (Skill parking is pre-existing behavior, unchanged by this diff — a doc-completeness nit, not a behavior-change request.)

### Low 3 — Stale HOW knowledge record now contradicts the shipped gate (knowledge hygiene; pre-existing staleness extended)
`.coding/knowledge/how/2026-08-31-re-add-dropped-auto-continue-budget-test-via-sma.md:14` — "KEY CORRECTNESS FACTS" still describes the loop as `if streak < MAX && is_workflow_executing() { … }` and "Workflow must be in Executing … else is_workflow_executing()=false and auto-continue never drives." That is now wrong twice over (bbd712c9 added the descendant park; this plan renamed the method and extended it to Reviewing). Per the project's memory-hygiene rule (supersede/update when contradicted — never leave both live), update or supersede that record's gate description. Its other facts (MAX=12, Prompt resets the streak, plateau = 13 calls) remain true.

## Verification detail

### 1. Gate correctness (focus 1) — VERIFIED
- `workflow_expects_progress` (loop_impl.rs:1568-1574): `matches!(state, Executing | Reviewing)`. The workflow mutex is acquired and dropped inside the method — no `await` while held, called once per turn end (the same pattern the 2026-12-17 performance review cleared for the old method).
- `WorkflowState` (mod.rs:26-49) has five variants: Planning, Executing, Reviewing, Complete, **Skill**. Planning/Complete/Skill → `false` → park. Planning and Complete are the intended waiting/terminal parks (unchanged); Skill parking is pre-existing behavior, unchanged by this diff (see Observations).
- The None-arm (agent.rs:233-257) keeps every bound: `streak < MAX_AUTO_CONTINUE` (12, agent.rs:62) && progress-state && `!has_running_descendants()`; the streak increment sits inside the gate (a park consumes no budget); the consolidation checkpoint (streak % 8) and the harness note are untouched; `Interrupt` still always parks (agent.rs:219).
- Descendant-park semantics preserved in Reviewing: the `&&` chain is state-agnostic, so a Reviewing parent with a running reviewer still parks (no bbd712c9 regression), and the child-completion Suggestion arm is untouched (still resumes regardless of streak).
- **ask_user safety (new-behavior check):** extending auto-continue to Reviewing cannot bypass a pending user question — `ask_user` blocks the turn (dispatch emits `UserQuestion` carrying a oneshot and awaits the answer; ask_user.rs:14-23), so the turn has not ended and the None-arm is never reached while waiting. The reviewer-failure protocol (ask_user retry/abandon) is safe.
- `finish` → Complete and `abandon_plan` → Planning both park correctly after the change.
- These checks examined the **main-agent** path; the subagent-side interaction they did not cover is High 1 above.

### 2. No dangling `is_workflow_executing` references (focus 2) — VERIFIED
Tree-walk across 1988 files: **zero code references remain**. All 22 hits are historical text — `.coding/knowledge|plans|reviews` records describing the old gate (accurate as history) plus the new test's comment narrating the pre-fix bug (agent.rs:5574). The green, warning-free build under `#![deny(warnings)]` corroborates (a dangling call would not compile). Note: the code-graph index still resolves the old symbol — stale gitignored cache, converges on content-hash drift; not a finding.

### 3. Regression test soundness (focus 3) — VERIFIED
- Setup is correct: `create_plan_with_kind(..., Implementation, None)` then workflow-level `complete_step(0)` — 0-indexed (plan_file.rs:318-319), and completing the root Implementation plan's only step transitions to Reviewing (mod.rs:682-700: root + Implementation|BugFixing → Reviewing). The test asserts `state() == Reviewing` with a clear setup message before the agent runs.
- `CountingMockProvider` (agent.rs:5242-5275) streams TextDelta + `Finish{Stop}` → the turn ends `StopReason::None` → exactly the changed arm. No descendant tracker in the config → `has_running_descendants()` = false.
- Pre-fix, the gate returned false in Reviewing → park → calls stay at 1 → the 10s deadline panic fires ("agent parked; the closing sequence stalls until the user types \"c\"") — the test genuinely fails on the old gate (and was observed red during the plan's reproduce step). Post-fix the second provider call arrives and the loop breaks at ≥ 2.
- Teardown terminates: `drop(cmd_tx)` → the chain exhausts the 12-turn budget → park → `cmd_rx.recv()` → None → task exits; the select! loop drains the bounded fan-in channel every 5ms so it never fills. No hang, no leak.
- The bbd712c9 tests (`auto_resume_parks_while_descendants_running` :5389, `auto_resume_resumes_after_descendants_finish` :5472) and the budget test (`prompt_resets_auto_continue_budget_after_exhaustion` :5278) lie outside both diff hunks — untouched; the budget test uses `create_plan` (Executing), so its semantics are unaffected by the Reviewing extension.

### 4. Constitution checks (focus 4) — VERIFIED
- The new pub method has a thorough doc comment (loop_impl.rs:1557-1567) including the incident reference. No `#[allow(...)]` anywhere in the diff. Pure Rust/tokio logic — multi-platform neutral: no Windows-only APIs, no paths, no shell syntax.
- The regression-test requirement is satisfied (fails without the fix, passes with it).

### 5. BUG record accuracy (focus 5) — VERIFIED
Every code reference in the record checks out: turn.rs:1273-1281 (mid-stream steer folds without breaking the stream), turn.rs:1747 (safe point breaks only on hard stops), turn.rs:1756-1771 (not-run synthesis), agentEventReducer.ts:1191 (`sweepRunningCards`), tests.rs:4628/4801 (both mid_stream_steer tests present), useAgentStore.test.ts:~1467-1486 (the sweep test block). The fix description and regression-test name match the shipped code; the stale-build hypothesis is framed as "leading explanation" with an explicitly accepted residual — no overclaim, no contradiction with the code.

### 6. Scope (focus 6) — VERIFIED
`git status --short`: exactly `M src/agent/loop_impl.rs`, `M src/runtime/agent.rs`, `?? .coding/knowledge/bug/2026-12-31-…md`, `?? .coding/plans/75deaec0.md`. Both diff hunks are on-topic (method replacement + the one gate line + the new test). Nothing unrelated.

## Observations (non-blocking; no action required for this plan)

- **This reviewer's own session will keep emitting bounded synthetic turns** (up to the 12-turn budget) after each of its turn ends — that is High 1 live, not new review activity. Expect idle/no-op reviewer turns; they park when the chain exhausts. Do not treat them as the reviewer asking for more work.
- **plan_file.rs:322-325 stale error message** (focus 5's flagged item): the 0-indexed workflow-level `complete_step` errors with "step numbers are 1-indexed, valid 1..={len}" — misleading for direct callers, though the IPC tool layer converts 1→0 and carries its own 1-indexed-facing tests. Pre-existing, untouched by this diff, out of scope per the plan — **not blocking**; worth a one-line fix in a future pass.
- **Skill-state parking**: a turn ending mid-skill parks (the old gate also excluded Skill). Unchanged pre-existing behavior; if mid-skill self-resume is ever wanted, that is a separate decision.
- **Test run**: 1947 passed / 0 failed, exit=0, warning-free — as reported by the main agent; not independently re-runnable by this read-only reviewer. The logic-level verification above is independent of that run.
- **Code-graph index** still resolves `is_workflow_executing` (stale, gitignored cache) — converges at the next project open; no action.
