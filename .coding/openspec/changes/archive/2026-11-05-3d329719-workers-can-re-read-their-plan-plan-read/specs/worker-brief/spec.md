## ADDED Requirements

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

