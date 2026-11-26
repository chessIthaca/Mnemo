## ADDED Requirements

### Requirement: Workers are placed on a host after the tier is chosen
After the tier is chosen, dispatch SHALL place a Mnemo-model worker on a connected paired worker node that has a free slot and whose operating system can run the project's checks, and SHALL run it locally otherwise. A lead-supplied host MUST win over automatic placement. Claude-agent-tier workers MUST stay local.

#### Scenario: No node connected
- **WHEN** the lead dispatches a Mnemo-model worker
- **WHEN** no worker node is connected
- **THEN** the worker runs locally exactly as before

#### Scenario: Idle suitable node
- **WHEN** the lead dispatches a Mnemo-model worker without a host
- **WHEN** a connected node has a free slot and passed the project's checks
- **THEN** the worker runs on that node
- **AND** team_status shows the node as the worker's host

#### Scenario: Lead pins local
- **WHEN** the lead dispatches with host "local"
- **THEN** the worker runs locally even when a node is idle

