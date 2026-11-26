## ADDED Requirements

### Requirement: A remote Claude-agent worker's session is started by the lead
The dispatch answer for an agent-tier worker placed on a host SHALL carry a claude_session block naming the SSH connection, the node checkout and the prompt. Mnemo MUST NOT start the session or launch claude itself. Mnemo SHALL follow the session through the host node and finish the worker when the session ends its turn, and team_agent_done SHALL remain authoritative.

#### Scenario: Remote agent dispatch
- **WHEN** the lead dispatches an agent-tier worker that is placed on a linked host
- **THEN** the answer carries a claude_session block with target "ssh", the SSH connection, the node checkout path and the prompt
- **AND** the answer carries no claude_agent block and no launch token

#### Scenario: Session ends its turn
- **WHEN** the remote session's transcript on the host ends its turn
- **THEN** Mnemo finishes the worker with the session's last answer as the report

