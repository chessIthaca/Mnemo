## ADDED Requirements

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

