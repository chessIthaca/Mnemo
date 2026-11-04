---
name: mnemo-plan
description: Plan a feature or bug fix on a Mnemo2 project's plan board before dispatching workers. Use only in a Mnemo2 project, when the Mnemo tools (plan_create, team_dispatch) are available, and the user asks for new work.
when_to_use: The user asks for a new feature, change or bug fix in a Mnemo2 project and no open board plan covers it yet.
---

# Plan first, then dispatch

In a Mnemo2 project all worker tasks belong to a step of a plan on the board. `team_dispatch` refuses a task without a `plan_id` and a 1-based `step`, and the plan is the record of intent every worker reads. Plan files live in `.coding/plans/<plan_id>.md`, with `board.json` beside them. They are written by the plan tools, so use the tools instead of editing the files by hand.

## 1. Scope

- Call `memory_search` with a query about the change and no `record_type` — mandatory before every `plan_create`, and enforced while you are the attached lead: `plan_create` is refused until a `memory_search` ran since your previous `plan_create` (or since you attached). Earlier decisions, known bugs and how-tos decide the design. For a defect, also `memory_search` with `record_type` "bug": a recorded root cause and regression test may already exist, and a `bug_fixing` `plan_create` is refused until one ran.
- Call `plan_list` and `team_status`. If an open plan already covers the request, continue it with `plan_show` and `plan_update` instead of creating a second one.
- Read the code the change touches. Find the exact files, symbols, existing tests and the commands that verify them.
- Ask the user only for what changes the plan: unclear scope, a choice between designs, or something expensive or risky. Otherwise state your assumptions in the plan.

## 2. Write the plan

Call `plan_create` with:

- `title` and `goal`: the outcome in one or two sentences.
- `kind`: `implementation` (the default), `research`, or `bug_fixing`. A defect is ALWAYS a `bug_fixing` plan, never `implementation`. It needs `bug` (the symptom), its context must name the regression test — which test, in which file, and how it fails without the fix — and the bare test function name is recorded with `plan_update` `regression_test` once it exists. Exception: a bug-triggered change that adds capabilities, new dependencies or spans multiple modules is an `implementation` plan, with the bug documented as motivation in `goal` and `context`. If you file a defect fix as `implementation` or `research`, the `plan_create` result appends a reminder line that bugs use `bug_fixing`: re-plan with the right kind unless the exception applies.
- `context`: at least 40 characters, in practice a page. Why the work exists, constraints, decisions already taken, exact file paths and symbols, and the verification commands. Workers read it, so write stable intent only. Never put status in it (commits, review state, what is next); that goes into `plan_update` `progress`.
- `steps`: ordered, each starting with a bold **header** and written as an implementation spec (see "Write each step as a spec" below), not a one-line task. The board refuses a step that names no file path (a research-only step may carry an explicit "no-code research" marker instead), so name the files each step touches.
- `interfaces`: contracts between steps (exports, signatures, data shapes, file ownership).
- `acceptance_scenarios`: observable outcomes, one per entry. Workers receive them, and high-risk dispatch requires at least one.

The board also refuses a plan that is too thin to resume from.

### Sizing steps

- One step is one independently deliverable unit that one worker can finish, test and commit: a small module, one endpoint with its tests, one UI component.
- Give steps disjoint files. A file owned by two steps means either one step or a recorded interface.
- Order by dependency. A step that needs another's output is dispatched after that step merges. Only independent steps run in parallel.
- Add a final verification step for work only you can do, such as an end-to-end run. You tick it yourself with `plan_step_done`.

### Write each step as a spec

Mnemo exists so that cheap models do the implementing. A cheap model follows instructions well but cannot fill design gaps, so the design has to be in the plan. A step that names only the outcome ("add a loading state", "fix the cite bug") leaves the worker to design it, and the result comes back for several review rounds. Write every step so a worker can implement it without making a design decision:

- **Files and symbols.** Every file the step changes, and in each the functions, types or components to add or change, with their signatures (parameters, return type, error type). Mark new code as new, for example "new: `fn conversation_needs_compaction(&self, cm: &ContextManager, messages: &[Message]) -> bool` in `src/agent/loop_impl.rs`". Name things exactly even before they exist; a precise design is the point.
- **Data shapes.** New fields, structs, events or wire types with their exact names and types, and who reads and writes them.
- **Behaviour rules.** The rules the code must follow, as a list, including the edge cases: empty input, zero, a resumed session, concurrency, a failure halfway, what must stay unchanged.
- **Order of edits.** When the order matters (a test first, a type before its users, a migration before code), say it.
- **Tests.** Each test by name, with its setup, the action and the exact assertions. For a bug, the regression test comes first and must fail on today's code.
- **Done checklist.** The commands to run and the observable result that means the step is finished.
- **Don't touch.** Files that other steps or parallel plans own.

You, the lead, write these specs yourself. Read the code they touch, so the names, signatures and line anchors are real. Prefer long specs with every detail needed to implement faithfully over short ones. There is no length to stay under. Put everything specific to the step in the step text, not only in the dispatch `brief`: the brief is not saved in the plan, so a worker that is resumed, compacted or switched to another model loses it.

First plan, then implement: write every step's spec before the first dispatch, not step by step as workers finish.

Only for really complicated work, where you cannot write the spec without substantial research (an unfamiliar subsystem, a performance question, several viable designs), may the first step be a spec-writing step on the `high` tier. It returns the specs, which you review and put into the plan, before any implementation step is dispatched.

### Acceptance and verification

- Put the verification commands (build, tests, lint) in the context and repeat the relevant ones in each brief.
- For each step, say what an observer can check afterwards: a test that passes, a command output, a screen state.
- For shared contracts, persistence, concurrency or security work you will dispatch with `review_risk` "high". That needs observable acceptance scenarios on the plan: `acceptance_scenarios` in `plan_create`, or `plan_update` `add_acceptance_scenarios` later. See the High-risk work section of the `mnemo-lead` skill.

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
- Record durable knowledge with `memory_write` as it settles during the work — a `SPEC:`/`DECISION:`/`HOW:` title, no plan IDs, branch names or commit hashes. This is a habit, not a gate: writing as you go just means the close gate below is already satisfied when you reach it.
- The plan closes after its last step when the team review passes. While you are the attached lead, an `implementation` or `research` plan additionally needs at least one durable knowledge record to close: a live `memory_write` with a `SPEC:`, `DECISION:` or `HOW:` (or `BUG:`) title, written, amended or superseded since the plan was created — plan-scoped notes do not count. A "cannot close yet" notice names `memory_write`: record what the plan established, then tick the last step again with `plan_step_done` to close. A `bug_fixing` plan is exempt — its `BUG:` record is captured automatically at close.
- `plan_abandon` with a reason is the last resort.
