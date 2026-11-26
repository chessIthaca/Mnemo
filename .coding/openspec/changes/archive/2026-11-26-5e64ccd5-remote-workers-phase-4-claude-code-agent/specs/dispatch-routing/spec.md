## MODIFIED Requirements

### Requirement: Workers are placed on a host after the tier is chosen
After the tier is chosen, dispatch SHALL place a Mnemo-model worker on a connected paired worker node that has a free slot and whose operating system can run the project's checks, and SHALL run it locally otherwise. A lead-supplied host MUST win over automatic placement. A Claude-agent-tier worker MAY be placed on such a node only when the node also has a linked SSH connection on which the lead can start a Claude Code session; otherwise it MUST stay local. Mnemo MUST NOT start the Claude Code session itself.

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

#### Scenario: Agent tier without an SSH link
- **WHEN** the lead dispatches an agent-tier worker
- **WHEN** no qualified node has a linked SSH connection
- **THEN** the worker stays local and the dispatch answer is the existing claude_agent block

#### Scenario: Agent tier on a linked host
- **WHEN** the lead dispatches an agent-tier worker without a host
- **WHEN** a qualified node with a free slot has a linked SSH connection
- **THEN** the worker is placed on that node
- **AND** the dispatch answer tells the lead to start a Claude Code session on that SSH connection in the worker's node checkout

