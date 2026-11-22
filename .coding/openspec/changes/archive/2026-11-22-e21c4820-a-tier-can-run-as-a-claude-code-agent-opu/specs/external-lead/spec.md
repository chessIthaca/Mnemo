## ADDED Requirements

### Requirement: The lead finishes a Claude Code agent worker
A worker handed to a Claude Code agent SHALL become finished only through team_agent_done, which records the report, marks the worker completed or failed and delivers the finished notice to the external lead. While such a worker is working, the agent gate SHALL allow the lead's Agent calls without asking.

#### Scenario: Agent worker reaches review
- **WHEN** the lead's agent committed in the worker's worktree
- **WHEN** the lead calls team_agent_done with a report
- **THEN** the worker is completed and a finished notice reaches the lead
- **AND** team_check, team_review and team_merge work as for any worker

#### Scenario: Gate lets the agent run
- **WHEN** an agent-tier worker is working
- **WHEN** the gate-agent hook asks about subagent_type general-purpose
- **THEN** the decision is allow

