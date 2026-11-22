## ADDED Requirements

### Requirement: Saving tiers updates what the lead sees
Saving a changed tier ladder in Settings SHALL bump the tier revision, so an attached Claude Code lead's tool descriptions refresh and its next dispatch says the tiers changed.

#### Scenario: Save tiers
- **WHEN** the user changes a tier and clicks Save tiers
- **WHEN** Claude Code leads
- **THEN** the lead's next dispatch says the tiers changed

