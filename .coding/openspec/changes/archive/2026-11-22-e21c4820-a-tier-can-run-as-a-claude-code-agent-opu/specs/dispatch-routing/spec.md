## ADDED Requirements

### Requirement: Agent tiers are decided before fold-down
Whether a dispatch goes to a Claude Code agent SHALL be decided from the tier routing settled on (the lead's explicit tier or Laya's cleared tier) before an empty tier folds down.

#### Scenario: Empty agent tier
- **WHEN** the high tier has claude_agent opus and no models
- **WHEN** Claude Code leads and dispatches on high
- **THEN** the step is handed to the Claude Code agent

