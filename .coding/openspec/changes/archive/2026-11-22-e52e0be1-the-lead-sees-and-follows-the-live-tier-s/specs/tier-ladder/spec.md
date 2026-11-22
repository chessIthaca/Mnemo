## ADDED Requirements

### Requirement: Tier model health is reported
For each tier, Mnemo SHALL report each configured model as available, unavailable until a time after a usage limit, or not configured, and the tier and model a dispatch would resolve to now.

#### Scenario: Unavailable first model
- **WHEN** a tier's first model is unavailable after a usage limit
- **THEN** the tier resolves to its next available model, or folds down
- **AND** team_tiers shows the first model unavailable until its time

