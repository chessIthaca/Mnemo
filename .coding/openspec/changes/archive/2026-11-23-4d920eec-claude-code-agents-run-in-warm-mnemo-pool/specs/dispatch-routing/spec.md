## ADDED Requirements

### Requirement: Agent worker dispatches wait for the previous launch and are capped
A dispatch or hand-off that would leave a second agent worker waiting for its agent SHALL be refused, and so SHALL one that would make more than three agent workers run at once.

#### Scenario: Second waiting agent
- **WHEN** one agent worker waits for its agent
- **WHEN** the lead dispatches another agent-tier step
- **THEN** the dispatch is refused and names the waiting worker

#### Scenario: Cap of three
- **WHEN** three agent workers are Working or Fixing
- **WHEN** another agent-tier dispatch arrives
- **THEN** it is refused

