## ADDED Requirements

### Requirement: Stale-read redirects are not strikes
The system SHALL NOT count a failed tool result that is a stale-read gate redirect (EDIT INTERCEPTED) or an unknown read receipt error toward a worker's strike streak, and SHALL NOT let it reset the streak either. An exact repeat of the previous call SHALL still count as a strike.

#### Scenario: Intercepted edit followed by a re-read
- **WHEN** a worker's edit is intercepted by the stale-read gate
- **WHEN** the worker re-reads the file and edits again
- **THEN** no strike nudge is sent for those calls

#### Scenario: Repeated identical intercepted call
- **WHEN** a worker sends the same intercepted edit call three times in a row
- **THEN** the repeats count as strikes as before

