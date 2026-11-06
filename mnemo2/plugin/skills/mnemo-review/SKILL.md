---
name: mnemo-review
description: Review a finished Mnemo2 worker thoroughly and merge it. Use only in a Mnemo2 project, when a worker has finished its turn (a "finished its turn" notice) and the Mnemo tools (team_check, team_review, team_merge) are available.
when_to_use: A Mnemo notice says a worker finished its turn or its audit, the user asks to review or merge a worker, or findings need closing.
---

# Reviewing a Mnemo2 worker

The review is yours; the worker's summary is a claim, not evidence. The merge gate demands a review of the exact current tip with substantive evidence, so work in this order.

## 1. Orient

- `team_status` with the exact `worker_id` from the notice. Note `status`, `branch`, `worktree`, `plan_id` and step, `review_risk`, any open findings and recorded checks. A worker is reviewable when it is idle (completed or in review). If it is still working, wait for its notice.
- `plan_show` for the plan: the step's text, the decisions and the interfaces are the intent you review against.

## 2. Read the diff unanchored

Before reading the worker's own summary, read the change from the project root with read-only git:

- `git log --oneline <lead-branch>..<branch>` and `git diff <lead-branch>...<branch> --stat`, then the full diff.
- Open the changed files in the worker's `worktree`, not only the hunks, when a change depends on its surroundings.
- Do not edit anything in the worktree. It must stay clean, and any commit moves the tip and invalidates checks and review.

Look for:

- Intent: does the diff do what the step and the plan say, and nothing more? Flag changes outside the step's files.
- Execution path: trace the new code from its entry point to its effect. Look for unreachable code, wrong defaults, missing error handling, and edge cases the brief mentioned.
- Integration: callers, exports, signatures and data shapes against the plan's interfaces and the neighbouring code. Compare what other merged workers changed.
- Test quality: do the tests exercise the new behaviour, would they fail without the change, and do they cover the failure paths? A passing test count alone is not a review. For a `bug_fixing` plan, check the regression test recorded on the plan by name: it must reproduce the defect without the fix and pass with it.
- Hygiene: leftover debug code, stray files, secrets, formatting churn, commented-out code, dependency changes nobody asked for.

## 3. Run the checks

Run the verification commands from the plan and the brief inside the worker's `worktree`, so the exact tip is tested. Add the project's usual build, test and lint commands when the brief did not name them.

Record each command you really ran with `team_check`: a short `name` (for example "cargo test"), `status` passed or failed, and the relevant output in `detail`. Never record a command you did not run. A failed check goes in as failed, and its failure becomes a finding or goes back to the worker. Checks belong to the tip; after any new commit run them again.

If the change is user-visible, exercise it as well (run it, call it, open it) and say so in the evidence.

Afterwards run `git status` in the worktree. If the checks left tracked changes (formatters, generated files, lockfiles), the merge will refuse the dirty worktree: route a fix to the worker with `team_route_fix` or tell it with `team_message`. Do not commit or clean them up yourself.

## 4. Record the review

`team_review` with:

- `summary`: your verdict in a few sentences.
- `findings`: the complete list. Each has `title`, `detail` (why it matters), `file`, `line` and `severity` high or low. Open findings block the merge regardless of severity, so do not use findings for taste; use them for things that must change. Reuse the same ids when re-reviewing.
- `evidence`: `unanchored_pass` true, plus your own substantive `intent`, `execution`, `integration`, `test_quality` and `residual_risk`. These are what you checked and what you accept, not a paraphrase of the worker's summary.

## 5. Close findings

- Tiny and exact: `review_fix`. Then re-run the checks and re-review, since it moves the tip.
- Larger: `team_route_fix`. Wait for the worker's next "finished its turn" notice, review the delta, re-run the checks, and `team_review` again.
- Verified fixed without moving the tip, or deliberately not changed: `team_resolve_finding` with `fixed` plus evidence, or `wontfix` plus a written justification.

## 6. Independent audit when the risk is high

Do this when the step was dispatched with `review_risk` "high", or when the change touches shared contracts, persistence, concurrency, security, or is large and cross-cutting. Use `team_audit_dispatch` on the idle worker at its final tip. The `mnemo-lead` skill describes the audit and its adjudication. Wait for the audit's own notice, weigh each of its findings, and link it in the next `team_review` through `evidence.independent_reviewer_id` and a written `evidence.audit_adjudication`. A fix moves the tip and makes the audit stale.

## 7. Merge

When the review is current, no finding is open and all checks passed on the current tip, call `team_merge`. The lead checkout must be clean and on a non-main branch. Afterwards:

- Read the board notice about the step (done, plan closed, or cannot close yet) and `plan_show` if it needs attention. A "cannot close yet" can be the knowledge gate: record the durable learning (next section), then tick the last step again with `plan_step_done`.
- After a merge that other open work depends on, run the project's tests on the lead branch before dispatching dependents.
- After parallel steps, do the plan's "Integration check" step yourself (run the combined result), then `plan_step_done`.
- If the merge makes things worse and has not reached `main`, `team_revert_merge`.
- Leave the worker's branch and checkout alone: `team_merge` deletes the branch after a clean merge. If `team_status` shows an `error` starting with "merged, but", tell the user; Mnemo2 retries it at the next merge and at its next start.
- A conflict is not auto-resolved. See the Review and merge cycle in the `mnemo-lead` skill.

Finish with a short report to the user: what merged, what the checks showed, what you decided about findings, and what is still running. If more workers are active, make sure one background waiter is running.

## 8. Record what the work established

After each merge, write what the work taught the project to Mnemo's memory, not only to the user — the `team_merge` result ends with a reminder line:

- `memory_write` the durable facts with a typed title: `SPEC:` how the feature now works, `DECISION:` a choice and its rationale, `HOW:` a recurring workflow. Keep each record compact and pointer-first, and leave out plan IDs, branch names and commit hashes — a record carrying them is a plan-scoped note, retired at plan close, and does not satisfy the close gate.
- While you are the attached lead, an `implementation` or `research` plan cannot close until such a record was written, amended or superseded after the plan was created, so record with the merge instead of waiting for the "cannot close yet" notice. A `bug_fixing` plan captures its `BUG:` record automatically at close; check that the captured record tells the truth (symptom, root cause, fix, regression test) and `memory_update` it if it does not.
- A merge that makes a stored fact wrong calls for `memory_supersede`, never a second live record. `memory_update` refines a still-current record; `memory_amend` appends a dated amendment.
