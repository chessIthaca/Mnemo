## ADDED Requirements

### Requirement: Usage-limited models are skipped
When a worker fails because its model hit a provider usage limit, Mnemo SHALL treat that model as unavailable for six hours, so tier resolution and fold-down skip it. In routing mode auto Mnemo SHALL resume the worker on its re-resolved tier; otherwise the failure notice SHALL name the resume call. Fix-round prompts SHALL ask for the narrowest covering tests instead of the full checks.

#### Scenario: Weekly limit
- **WHEN** a worker fails with a weekly usage limit error
- **THEN** its model is unavailable
- **AND** the next dispatch on that tier folds to the next model

#### Scenario: Auto mode continues
- **WHEN** routing mode is auto
- **WHEN** a worker fails with a usage limit
- **WHEN** its tier folds to another model
- **THEN** the worker is resumed on that model with its transcript

#### Scenario: Narrow fix rounds
- **WHEN** a finding is routed to its worker
- **THEN** the fix prompt asks for the narrowest covering tests, not run_checks

