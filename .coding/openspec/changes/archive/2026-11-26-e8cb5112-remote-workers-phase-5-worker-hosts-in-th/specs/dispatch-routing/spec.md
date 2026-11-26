## ADDED Requirements

### Requirement: Host state is reported from the live link and only for active workers
A continue answer (team_message or team_route_fix to a remote Claude Code agent worker) SHALL name the host's currently linked SSH connection, never a connection stored on the worker record that has since been unlinked or replaced. With no linked connection it SHALL name none and explain how to proceed. A worker's host-away time (host_offline_s) SHALL be reported only while the worker is active, and a merge SHALL clear the worker's host-away mark.

#### Scenario: Continue after a relink
- **WHEN** a remote Claude worker was placed with SSH connection A
- **WHEN** the host is unlinked and linked to connection B
- **WHEN** the lead sends team_message to the worker
- **THEN** continue_session.host is B
- **AND** the stored worker record still names A

#### Scenario: Finished worker on an away host
- **WHEN** a worker's host went away and the worker then merged or finished
- **THEN** team_status shows no host_offline_s for that worker

