## ADDED Requirements

### Requirement: Agent workers are handed back when their lead leaves
When the external lead detaches, or Mnemo2 starts without one, every agent worker whose agent was never launched or has stalled SHALL be handed back: it stops being an agent worker, becomes interrupted with the reason, keeps its record, branch, checkout and commits, and can be resumed on a Mnemo model. An agent that is still running SHALL be left alone.

#### Scenario: Lead leaves before launching
- **WHEN** an agent worker waits for its agent
- **WHEN** the external lead detaches
- **THEN** the worker is interrupted with the reason and is no longer an agent worker
- **AND** team_resume restarts it on its tier's Mnemo model with its branch and commits

#### Scenario: Running agent survives a detach
- **WHEN** an agent worker's agent is running and not stalled
- **WHEN** the external lead detaches
- **THEN** the worker stays an agent worker and keeps working

## MODIFIED Requirements

### Requirement: The lead finishes a Claude Code agent worker
A worker handed to a Claude Code agent SHALL be finished by Mnemo when its registered agent's transcript says it finished, with the agent's last message as its report, marking the worker completed and delivering the finished notice to the external lead. The lead MAY still call team_agent_done to report a failed run or to replace the report; after Mnemo finished the same run it SHALL NOT deliver a second notice. While such a worker is working, the agent gate SHALL allow the lead's Agent calls without asking.

#### Scenario: Agent worker reaches review
- **WHEN** the lead's agent committed in the worker's worktree
- **WHEN** the agent's transcript says it finished
- **THEN** the worker is completed with the agent's last message as its report
- **AND** one finished notice reaches the lead
- **AND** team_check, team_review and team_merge work as for any worker

#### Scenario: Lead reports after Mnemo finished the run
- **WHEN** Mnemo finished an agent worker's run
- **WHEN** the lead calls team_agent_done with a report for that run
- **THEN** the worker keeps its completed state with the lead's report
- **AND** no second finished notice is sent

#### Scenario: Gate lets the agent run
- **WHEN** an agent-tier worker is working
- **WHEN** the gate-agent hook asks about subagent_type general-purpose
- **THEN** the decision is allow

### Requirement: Agent workers are observable
Every Claude Code agent worker SHALL show its agent phase (awaiting_launch, running, stalled or finished) in team_status and Mission Control from the moment it is handed to an agent. The lead SHALL be able to register a running Claude Code agent for a worker with team_agent_started. Mnemo SHALL then report the agent's transcript, last activity, tool calls, tokens and finish state in team_status, flag it stalled after ten silent minutes and notify the lead once per stall. The agent MAY post progress notes with team_progress, which land in the worker's log and as notices.

#### Scenario: Agent not launched yet
- **WHEN** a dispatch lands on a tier that runs as a Claude Code agent
- **WHEN** the lead has not launched the agent
- **THEN** team_status and Mission Control show the worker waiting for its Claude Code agent

#### Scenario: Stalled agent
- **WHEN** an agent worker is registered
- **WHEN** its transcript has not changed for ten minutes
- **THEN** team_status shows it stalled
- **AND** the lead gets one notice

#### Scenario: Progress note
- **WHEN** the agent calls team_progress with a note
- **THEN** the note is in the worker's log and reaches the lead

