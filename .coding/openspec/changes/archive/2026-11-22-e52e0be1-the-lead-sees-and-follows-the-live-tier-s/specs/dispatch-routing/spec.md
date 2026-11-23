## ADDED Requirements

### Requirement: Pinned models that bypass an agent tier are flagged
A dispatch from the external lead that pins a model on a tier which runs as a Claude Code agent SHALL warn that the model bypasses the agent, and SHALL still start the Mnemo worker.

#### Scenario: Pinned model on an agent tier
- **WHEN** high runs as a Claude Code agent opus
- **WHEN** the lead dispatches with tier high and an explicit model
- **THEN** the result warns that the model bypasses the agent
- **AND** a Mnemo worker starts on the pinned model

### Requirement: Escalation honours run as
When Claude Code leads, moving an implementation worker onto a tier that runs as a Claude Code agent, by team_resume, auto escalation or usage-limit auto-resume, SHALL stop its Mnemo session and hand the step to the lead's agent with a claude_agent block. Without an external lead the move SHALL switch models as before.

#### Scenario: Resume onto an agent tier
- **WHEN** Claude Code leads
- **WHEN** the lead resumes a worker on a tier that runs as a Claude Code agent
- **THEN** the result carries a claude_agent block
- **AND** the worker has no Mnemo session

#### Scenario: In-app lead
- **WHEN** no external lead is attached
- **WHEN** a worker is resumed on that tier
- **THEN** it switches to the tier's model

