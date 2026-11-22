## ADDED Requirements

### Requirement: Agent workers are observable
The lead SHALL be able to register a running Claude Code agent for a worker with team_agent_started. Mnemo SHALL then report the agent's transcript, last activity, tool calls, tokens and finish state in team_status, flag it stalled after ten silent minutes, and notify the lead once per stall and once when the agent finishes without team_agent_done. The agent MAY post progress notes with team_progress, which land in the worker's log and as notices.

#### Scenario: Stalled agent
- **WHEN** an agent worker is registered
- **WHEN** its transcript has not changed for ten minutes
- **THEN** team_status shows it stalled
- **AND** the lead gets one notice

#### Scenario: Progress note
- **WHEN** the agent calls team_progress with a note
- **THEN** the note is in the worker's log and reaches the lead

### Requirement: TeamManager integrates branches into workers
The lead tool team_integrate SHALL merge an existing local branch into a worker's branch in its checkout. A conflict SHALL abort the merge, leave the checkout unchanged and list the conflicting files.

#### Scenario: Conflict
- **WHEN** the named branch conflicts with the worker's branch
- **THEN** the merge is aborted and the conflicting files are listed
- **AND** the worker's tip is unchanged

