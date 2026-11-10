## ADDED Requirements

### Requirement: Idle compaction has an on/off switch
The Savings page SHALL show an "Idle compaction" switch that is on by default, and saving it SHALL turn idle compaction on or off without a restart.

#### Scenario: Default is on
- **WHEN** the user opens Settings → Savings on a config that never set idle_compaction
- **THEN** the Idle compaction switch is on

#### Scenario: Switching it off is saved
- **WHEN** the user switches Idle compaction off and saves
- **THEN** config.toml holds idle_compaction = false under [general.optimizer]
- **AND** agents stop idle compaction without a restart

