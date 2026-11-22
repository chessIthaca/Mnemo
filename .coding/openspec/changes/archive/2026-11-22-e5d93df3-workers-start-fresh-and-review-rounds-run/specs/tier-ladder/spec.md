## ADDED Requirements

### Requirement: An agent tier can carry an effort
A tier with claude_agent MAY carry claude_agent_effort set to low, medium, high, xhigh or max. The dispatch claude_agent block SHALL carry that effort, and the tier guidance line SHALL name it. A claude_agent_effort without claude_agent MUST fail validation.

#### Scenario: Opus at xhigh
- **WHEN** the high tier runs as a Claude Code agent opus with effort xhigh
- **WHEN** Claude Code leads and dispatches on high
- **THEN** the claude_agent block carries effort xhigh

#### Scenario: Effort without an agent
- **WHEN** a ladder sets claude_agent_effort on a tier without claude_agent
- **THEN** saving the ladder fails with a validation error

