## ADDED Requirements

### Requirement: The lead can read the live tier setup
The lead tool team_tiers SHALL return every tier's models in order with their health, effort, run as and current resolution. Mnemo SHALL tell the external lead when the tiers changed since it last read them, and its tool descriptions SHALL refresh through a tools/list_changed notification when the tool list changes, including after Mnemo2 restarts.

#### Scenario: Tier change reaches the lead
- **WHEN** Claude Code leads
- **WHEN** the user sets High to run as a Claude Code agent and saves
- **THEN** the lead receives tools/list_changed
- **AND** the next dispatch result says the tiers changed

#### Scenario: Health is visible
- **WHEN** a model hit its usage limit
- **THEN** team_tiers shows it unavailable until a time

