## ADDED Requirements

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

