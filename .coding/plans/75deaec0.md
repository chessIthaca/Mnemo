# Plan: Steer mid-flight never orphans tool calls + auto-continue covers Reviewing

## Goal
A mid-turn steer must never orphan an emitted tool call (every call executes or is explicitly recorded not-run; the result slot never renders foreign text), and a stalled/ended turn in the closing sequence must self-recover: auto-continue extends from Executing to Reviewing (bounded, no running descendants), so the agent resumes the closing sequence instead of parking until the user types "c".

## Kind
bug_fixing

## Context
Live-observed twice on 2026-12-31 (this session), both in turns resumed by the child-completion Suggestion after a reviewer finished: (1) read_files result rendered as "c"; (2) finish result rendered as "c" with the state provably unchanged (re-issued finish succeeded). The "c" is the user's own nudge text — a mid-turn user message is a steer (StopReason::fold → Steer, loop_impl.rs:423-447) that soft-stops the turn and drives the follow-up turn. The steer-drain machinery is one day old (plan edfff8d9, 2026-12-30): it fixed the after-stream-end window (emitted calls never silently cancelled) but both incidents show a call skipped anyway — a residual mid-stream/pre-dispatch seam. Auto-continue (agent.rs:233-257, MAX_AUTO_CONTINUE=12) fires only on is_workflow_executing() and is a turn-end hook; it is parked while descendants run (bbd712c9) and does not cover Reviewing, so a parked closing sequence waits for the user forever. Key code: turn.rs stream-select cmd arm ~1255-1320, hard-stop gate + not-run synthesis 1747-1773, between-call safe point 1728-1746, post-batch result push ~1899-2010; loop_impl.rs fold 423-500; agent.rs None-arm 233-257 + Suggestion arm; frontend agentEventReducer call↔result pairing.

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
In reviewer-finish-resumed turns, a user "c" nudge arriving while a tool call is in flight soft-stops the turn and silently skips the emitted call (finish never executes — workflow state unchanged), the orphaned call's result slot renders the steer text ("c"), and the agent parks until the user nudges again (auto-continue covers only Executing, not Reviewing).

## Regression test
auto_resume_fires_when_workflow_is_reviewing
