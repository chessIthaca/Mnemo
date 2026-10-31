---
name: mnemo-plan
description: Plan a feature or bug fix on a Mnemo2 project's plan board before dispatching workers. Use only in a Mnemo2 project, when the Mnemo tools (plan_create, team_dispatch) are available, and the user asks for new work.
when_to_use: The user asks for a new feature, change or bug fix in a Mnemo2 project and no open board plan covers it yet.
---

# Plan first, then dispatch

In a Mnemo2 project all worker tasks belong to a step of a plan on the board. `team_dispatch` refuses a task without a `plan_id` and a 1-based `step`, and the plan is the record of intent every worker reads. Plan files live in `.coding/plans/<plan_id>.md`, with `board.json` beside them. They are written by the plan tools, so use the tools instead of editing the files by hand.

## 1. Scope

- Call `plan_list` and `team_status`. If an open plan already covers the request, continue it with `plan_show` and `plan_update` instead of creating a second one.
- Read the code the change touches. Find the exact files, symbols, existing tests and the commands that verify them.
- Ask the user only for what changes the plan: unclear scope, a choice between designs, or something expensive or risky. Otherwise state your assumptions in the plan.

## 2. Write the plan

Call `plan_create` with:

- `title` and `goal`: the outcome in one or two sentences.
- `kind`: `implementation` (the default), `research`, or `bug_fixing`. A bug_fixing plan needs `bug` (the symptom), and its context must name the regression test: which test, in which file, and how it fails without the fix. Record it later with `plan_update` `regression_test`.
- `context`: at least 40 characters, in practice a page. Why the work exists, constraints, decisions already taken, exact file paths and symbols, and the verification commands. Workers read it, so write stable intent only. Never put status in it (commits, review state, what is next); that goes into `plan_update` `progress`.
- `steps`: ordered, each starting with a bold **header**. The board refuses a step that names no file path (a research-only step may carry an explicit "no-code research" marker instead), so name the files each step touches.
- `interfaces`: contracts between steps (exports, signatures, data shapes, file ownership).

The board also refuses a plan that is too thin to resume from.

### Sizing steps

- One step is one independently deliverable unit that one worker can finish, test and commit: a small module, one endpoint with its tests, one UI component.
- Give steps disjoint files. A file owned by two steps means either one step or a recorded interface.
- Order by dependency. A step that needs another's output is dispatched after that step merges. Only independent steps run in parallel.
- Add a final verification step for work only you can do, such as an end-to-end run. You tick it yourself with `plan_step_done`.

### Acceptance and verification

- Put the verification commands (build, tests, lint) in the context and repeat the relevant ones in each brief.
- For each step, say what an observer can check afterwards: a test that passes, a command output, a screen state.
- For shared contracts, persistence, concurrency or security work you will dispatch with `review_risk` "high". That also needs a `## Acceptance scenarios` section with observable bullets in the plan file. See the High-risk work section of the `mnemo-lead` skill for how this works today.

## 3. Confirm

Summarize the plan to the user in a few lines: the steps, the tier each will use, and which run in parallel. For small, clear work, go straight on. For large, ambiguous or costly work, wait for a go-ahead.

## 4. Dispatch

- For each ready step call `team_dispatch` with `plan_id`, `step`, a stable `worker_id`, a `tier` and a task-specific `brief`. The `mnemo-lead` skill covers tiers and briefs.
- If the board refuses a parallel dispatch because two steps share a file, record the contract with `plan_update` `interface` and dispatch again.
- Then start the background waiter as the `mnemo-lead` skill describes, tell the user what is running (worker, step, tier), and end your turn. Do not wait in a loop.

## 5. Keep the plan current

- `plan_update` `decision`: a settled question that workers or a later session must know.
- `plan_update` `interface`: a contract between steps. It is required before two steps touching the same file are in flight.
- `plan_update` `progress`: execution state. Workers do not see it.
- `plan_update` `add_steps`: new work discovered along the way.
- A step ticks itself when a task on it merges and none is still open. Tick only your own steps with `plan_step_done`.
- The plan closes after its last step when the team review passes. `plan_abandon` with a reason is the last resort.
