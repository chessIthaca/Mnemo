# Worker brief

## Purpose
What a Mnemo2 worker is told about its plan at dispatch, how it re-reads the plan while it works, and how it learns that the lead changed the plan.

## Requirements

### Requirement: Workers get the whole plan at dispatch
The system SHALL give every implementation worker, at dispatch, its plan's title, goal, kind, context, decisions, interfaces, acceptance scenarios, spec changes, schema sections, the step list and the full text of its own step, followed by the lead's brief, and SHALL NOT include the plan's progress notes.

#### Scenario: Dispatch header
- **WHEN** the lead dispatches a worker on step 2 of a plan with a decision and a spec change
- **THEN** the worker's instructions contain the decision, the spec change and step 2's full text before the brief
- **AND** they contain no progress note

### Requirement: Workers can re-read their plan
The system SHALL offer workers a read-only plan_read tool that returns their own plan as it stands now, or with a capability name that capability's current OpenSpec spec, and every dispatch header SHALL say so. plan_read SHALL never return another plan.

#### Scenario: Decision after dispatch
- **WHEN** the lead records a decision after a worker started
- **WHEN** the worker calls plan_read
- **THEN** the result contains the new decision

#### Scenario: Current spec
- **WHEN** a worker calls plan_read with capability tier-ladder
- **THEN** the result is the current tier-ladder spec

### Requirement: Working workers hear about plan changes
The system SHALL send each worker that is working on a plan a short notice naming what changed when the plan's context, decisions, interfaces, steps, acceptance scenarios, spec changes or sections change, and SHALL NOT notify idle, completed, in-review or merged workers or notify on progress notes.

#### Scenario: Decision recorded
- **WHEN** a worker is working on a plan
- **WHEN** the lead records a decision on it
- **THEN** the worker receives 'Plan <id> changed: new decision. Call plan_read to re-read it before you continue.'

#### Scenario: Worker awaiting review
- **WHEN** a worker finished its turn and awaits review
- **WHEN** the lead records a decision
- **THEN** the worker receives nothing

### Requirement: The agent prompt carries the worker brief
The prompt handed to a Claude Code agent SHALL contain the same plan, step and brief a Mnemo worker receives, plus the worktree it must work in, the branch to commit on, and the rules never push, never git stash, never merge, and run the project checks before the last commit.

#### Scenario: Prompt contents
- **WHEN** a step is handed to a Claude Code agent
- **THEN** the prompt names the worktree path and branch
- **AND** the prompt contains the step text and the brief

### Requirement: The agent prompt sets the working rules
The prompt handed to a Claude Code agent SHALL tell it to start every shell command with a cd into its worktree, to keep shell commands in the foreground unless they run long, and to post a one-line team_progress note after each milestone.

#### Scenario: Prompt rules
- **WHEN** a step is handed to a Claude Code agent
- **THEN** the prompt carries the cd, foreground and progress rules

### Requirement: Workers know the commit they start from
Every worker SHALL record the lead commit its checkout was forked from, and its opening prompt SHALL name that commit. A worker with no commits of its own and a clean checkout SHALL be fast-forwarded to the lead branch tip before its next prompt when the lead branch has moved, and that prompt SHALL say so. A worker with commits MUST NOT be rebased or reset.

#### Scenario: Dispatch after a lead commit
- **WHEN** the lead commits
- **WHEN** the lead dispatches a worker
- **THEN** the worker's base commit is the new lead tip
- **AND** its opening prompt names that commit

#### Scenario: Waiting worker follows the lead
- **WHEN** a worker has no commits and a clean checkout
- **WHEN** the lead branch moves
- **WHEN** the worker receives a prompt
- **THEN** its checkout is fast-forwarded to the lead tip
- **AND** the prompt says it was fast-forwarded

#### Scenario: Worker with commits is left alone
- **WHEN** a worker has commits
- **WHEN** the lead branch moves
- **WHEN** the worker receives a prompt
- **THEN** the worker's tip is unchanged

### Requirement: A worker handed to an agent mid-step continues from its checkout
When a worker moves to a Claude Code agent tier mid-step, the agent prompt SHALL carry the worker brief plus where the previous worker left off: its previous model, and the instruction to read git status, the log since the base commit and the diff before changing anything.

#### Scenario: Continuation prompt
- **WHEN** a worker on a Mnemo model is resumed onto an agent tier
- **THEN** the agent prompt names the previous model and the base commit
