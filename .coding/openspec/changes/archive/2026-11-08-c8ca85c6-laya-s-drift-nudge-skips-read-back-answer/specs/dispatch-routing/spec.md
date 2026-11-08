## ADDED Requirements

### Requirement: Drift nudges read only new guidance
The system SHALL run the drift check only on a lead or user message that can carry new work. It SHALL NOT classify text Mnemo2 generated itself (a route-fix brief or a plan-changed notice) or the first lead or user message a read-back worker receives, so those messages never produce a drift nudge and never spend the worker's one nudge. Any other lead or user message of at least `min_words` words SHALL still be classified in the background, and SHALL nudge the lead when the answer clears on a tier above the worker's.

#### Scenario: Read-back answer
- **WHEN** a worker dispatched with read_back receives its first lead message of at least min_words words
- **THEN** Laya is not asked about that message
- **AND** no drift nudge is sent
- **AND** the worker's one nudge stays available

#### Scenario: Plan notices before the read-back answer
- **WHEN** a read-back worker has received only Mnemo2 plan-changed notices
- **WHEN** the lead then sends its answer
- **THEN** Laya is not asked about the answer

#### Scenario: Route-fix prompt
- **WHEN** team_route_fix delivers a fix brief to a worker
- **THEN** Laya is not asked about the brief as drift
- **AND** no drift nudge is sent

#### Scenario: A later message adds scope
- **WHEN** a worker receives a lead message of at least min_words words that is not its read-back answer
- **WHEN** Laya clears the message on a tier above the worker's
- **THEN** the lead gets a "this got harder" notice
- **AND** a nudge row with reason drift is logged

