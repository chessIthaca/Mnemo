# Tier ladder

## Purpose
The five worker tiers in `endpoints.toml`, how an empty tier resolves, and how configuration writes are serialized.

## Requirements

### Requirement: Five fixed tiers
The system SHALL keep exactly five tiers in fixed order: trivial, routine, medium, high and escalate, each with an id, a name, a definition, an ordered model list and an optional reasoning effort. Escalate SHALL never be offered to Laya or the kNN overlay as a routing answer, and the lead's team_dispatch SHALL refuse it for fresh work; team_resume and team_route_fix MAY move an existing worker to it.

#### Scenario: Routing never picks escalate
- **WHEN** routing mode is auto and Laya classifies a dispatch
- **THEN** the chosen tier is one of trivial, routine, medium or high

#### Scenario: Lead picks escalate for fresh work
- **WHEN** the lead calls team_dispatch with tier escalate
- **THEN** the dispatch is refused as escalation only, never a first pick
- **AND** no worker is provisioned

#### Scenario: Escalating an existing worker
- **WHEN** the lead calls team_resume on an idle worker with tier escalate
- **THEN** the worker restarts on the escalate tier, folding down when it has no model

### Requirement: Empty tiers fold down
The system SHALL resolve a tier without models to the nearest lower tier that has one, and SHALL require the lowest tier (trivial) to have a model.

#### Scenario: Empty medium
- **WHEN** the medium tier has no model and the lead dispatches with tier `medium`
- **THEN** the worker runs on the routine tier's first model

#### Scenario: Emptying trivial
- **WHEN** an endpoint rename or removal would leave trivial without a model
- **THEN** the change is refused

### Requirement: Retired tier ids
The system SHALL refuse the retired ids simple, average and difficult in tool calls and SHALL convert them (to trivial, routine and high) only in stored data, writing a converted file back once.

#### Scenario: Old id in a dispatch
- **WHEN** the lead dispatches with tier `average`
- **THEN** the dispatch is refused as an unknown tier, listing the current tier ids

### Requirement: Serialized configuration writes
The system SHALL hold an exclusive OS file lock on `<config>/config.write.lock` for every write of config.toml, endpoints.toml, keys.toml and mcp.toml, for the legacy migration and for crash recovery, retrying every 50 ms for up to 5 seconds. A cached Config save SHALL never write the ladder.

#### Scenario: Two writers at once
- **WHEN** two processes save endpoints.toml at the same time
- **THEN** one waits for the other's lock and both edits are kept

### Requirement: One Tiers page
The system SHALL configure the ladder, the Laya routing choice (shown on top) and the Laya settings (at the bottom) on one Settings > Tiers page in Mnemo2.

#### Scenario: Laya not configured
- **WHEN** Laya is not running and the user picks a routing choice that needs Laya
- **THEN** the Tiers page says the choice needs Laya and the lead's tier stands until then
