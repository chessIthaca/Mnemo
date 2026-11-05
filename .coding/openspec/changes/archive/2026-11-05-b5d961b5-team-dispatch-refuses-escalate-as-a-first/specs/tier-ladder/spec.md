## MODIFIED Requirements

### Requirement: Five fixed tiers
The system SHALL keep exactly five tiers in fixed order: trivial, routine, medium, high and escalate, each with an id, a name, a definition, an ordered model list and an optional reasoning effort. Escalate SHALL never be offered to Laya or the kNN overlay as a routing answer, and the lead's team_dispatch SHALL refuse it for fresh work; team_resume and team_route_fix MAY move an existing worker to it.

#### Scenario: Routing never picks escalate
- **WHEN** routing mode is auto and Laya classifies a dispatch
- **THEN** the chosen tier is one of trivial, routine, medium or high

#### Scenario: Lead picks escalate for fresh work
- **WHEN** the lead calls team_dispatch with tier escalate
- **THEN** the dispatch is refused as escalation only, never a first pick
- **THEN** no worker is provisioned

#### Scenario: Escalating an existing worker
- **WHEN** the lead calls team_resume on an idle worker with tier escalate
- **THEN** the worker restarts on the escalate tier, folding down when it has no model

