## ADDED Requirements

### Requirement: Fix rounds on agent workers are recorded
team_route_fix on a Claude Code agent worker SHALL record the fix round as for a Mnemo worker and return the fix text with the SendMessage instruction for the registered agent; team_agent_done SHALL close the round.

#### Scenario: Routed fix
- **WHEN** the lead routes a finding to an agent worker
- **THEN** the worker is Fixing with the round recorded
- **AND** the answer carries the SendMessage instruction and the fix text

