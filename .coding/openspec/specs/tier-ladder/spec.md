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

### Requirement: A tier can run as a Claude Code agent
Every tier except escalate MAY carry `claude_agent` set to opus, sonnet, haiku or fable. When an external Claude Code lead is attached and a dispatch settles on such a tier, the step SHALL be handed to the lead's own Claude Code agent with that model instead of a Mnemo worker session; without an external lead the tier SHALL resolve to its models exactly as before.

#### Scenario: Agent tier with Claude Code leading
- **WHEN** Claude Code is attached as lead
- **WHEN** the high tier has claude_agent opus
- **WHEN** the lead dispatches a step on high
- **THEN** the result carries a claude_agent block with the worktree, branch and prompt
- **AND** no Mnemo worker session is started

#### Scenario: Agent tier with the in-app lead
- **WHEN** no external lead is attached
- **WHEN** the high tier has claude_agent opus and a model
- **THEN** the dispatch starts a Mnemo worker on that model

#### Scenario: Escalate cannot be an agent tier
- **WHEN** a ladder sets claude_agent on escalate
- **THEN** saving the ladder fails with a validation error

### Requirement: An agent tier's worker is tracked
A worker that runs on an agent tier SHALL be tracked through its agent's transcript once registered, so its activity counts like a Mnemo worker's.

#### Scenario: Registered agent
- **WHEN** the lead registers the agent of an agent-tier worker
- **THEN** the worker's activity comes from the agent's transcript

### Requirement: An agent tier can carry an effort
A tier with claude_agent MAY carry claude_agent_effort set to low, medium, high, xhigh or max. The dispatch claude_agent block SHALL carry that effort, and the tier guidance line SHALL name it. A claude_agent_effort without claude_agent MUST fail validation.

#### Scenario: Opus at xhigh
- **WHEN** the high tier runs as a Claude Code agent opus with effort xhigh
- **WHEN** Claude Code leads and dispatches on high
- **THEN** the claude_agent block carries effort xhigh

#### Scenario: Effort without an agent
- **WHEN** a ladder sets claude_agent_effort on a tier without claude_agent
- **THEN** saving the ladder fails with a validation error

### Requirement: Tier model health is reported
For each tier, Mnemo SHALL report each configured model as available, unavailable until a time after a usage limit, or not configured, and the tier and model a dispatch would resolve to now.

#### Scenario: Unavailable first model
- **WHEN** a tier's first model is unavailable after a usage limit
- **THEN** the tier resolves to its next available model, or folds down
- **AND** team_tiers shows the first model unavailable until its time
