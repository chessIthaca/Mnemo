## ADDED Requirements

### Requirement: The Tiers page offers Run as a Claude Code agent
Each tier card except escalate SHALL offer a Run as choice between Mnemo models and a Claude Code agent with Opus, Sonnet, Haiku or Fable, saved with the ladder.

#### Scenario: Choose Opus for high
- **WHEN** the user picks Claude Code agent — Opus on the High card and saves
- **THEN** endpoints.toml stores claude_agent opus on the high tier
- **AND** the lead preview shows the agent suffix on the High line

