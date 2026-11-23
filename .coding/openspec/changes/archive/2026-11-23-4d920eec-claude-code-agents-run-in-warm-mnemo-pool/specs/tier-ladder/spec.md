## ADDED Requirements

### Requirement: Pools return to the ladder's workers when no worker needs them
A pool checkout held by an audit that has reported, or by a stopped or failed worker whose plan step is done, SHALL be released with its build output kept, so a later dispatch on any tier can reuse it warm.

#### Scenario: Reported audit
- **WHEN** an audit worker delivers its report
- **THEN** its pool is released and its record keeps the report

