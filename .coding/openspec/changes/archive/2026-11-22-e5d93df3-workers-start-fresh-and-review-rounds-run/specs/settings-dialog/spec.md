## ADDED Requirements

### Requirement: The Tiers page offers an agent effort
When a tier runs as a Claude Code agent, its card SHALL offer an Agent effort choice of Default, low, medium, high, xhigh or max, saved with the ladder; choosing Mnemo models SHALL clear it.

#### Scenario: Choose xhigh
- **WHEN** the user picks Claude Code agent Opus and Agent effort xhigh on High and saves
- **THEN** endpoints.toml stores claude_agent_effort xhigh on the high tier
- **AND** the lead preview names the effort

