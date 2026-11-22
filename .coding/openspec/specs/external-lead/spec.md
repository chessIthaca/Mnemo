# External lead

## Purpose
How a Claude Code session becomes Mnemo2's lead for a project through the mnemo2-mcp helper, keeps that role while any of its helpers is connected, and hands it back to the in-app lead.

## Requirements

### Requirement: Tool calls attach the external lead
The system SHALL attach the external lead to a project's team on the first MCP tools/call for that project, and SHALL NOT attach it for initialize, tools/list, ping or a notices read.

#### Scenario: Opening Claude Code
- **WHEN** Claude Code starts in a registered project and lists the Mnemo tools
- **THEN** the in-app lead keeps receiving worker notices

#### Scenario: First tool call
- **WHEN** a helper forwards a tools/call for the project
- **THEN** the external lead is attached and worker notices queue for it

### Requirement: A closing helper keeps the lead while another helper led
The system SHALL remember every helper id that made a tools/call while the external lead is attached, and a helper's detach on close SHALL release the lead only when no other remembered helper remains. A detach that names no helper SHALL release the lead.

#### Scenario: Second helper closes
- **WHEN** helpers a and b both made tool calls
- **WHEN** helper b closes
- **THEN** the external lead stays attached and its waiter keeps waiting

#### Scenario: Last helper closes
- **WHEN** helper a is the only remembered helper
- **WHEN** helper a closes
- **THEN** the external lead is released
- **AND** unread notices go to the in-app lead

#### Scenario: Anonymous detach
- **WHEN** a detach arrives without a helper id
- **THEN** the external lead is released

### Requirement: Idle external leads are released
The system SHALL release an external lead that made no Mnemo call for 10 minutes while no waiter runs, regardless of remembered helpers, and SHALL forget every remembered helper on any release.

#### Scenario: Crashed helper
- **WHEN** the only leading helper crashed without detaching
- **WHEN** 10 minutes pass with no call and no waiter
- **THEN** the external lead is released

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
