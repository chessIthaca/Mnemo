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

### Requirement: The lead sees stale bases and uncommitted lead files
team_status SHALL report, for an unmerged implementation worker, how many lead-branch commits its branch lacks, and team_check and team_merge SHALL repeat that report without refusing. A dispatch SHALL warn when the lead checkout has uncommitted files under .coding/ outside the plans, reviews, knowledge and laya folders.

#### Scenario: Stale worker
- **WHEN** a worker has commits
- **WHEN** the lead branch has two commits the worker lacks
- **THEN** team_status shows stale_base 2
- **AND** team_merge still merges

#### Scenario: Uncommitted lead notes
- **WHEN** the lead checkout has an untracked .coding/notes.md
- **WHEN** the lead dispatches
- **THEN** the dispatch result warns and names .coding/notes.md

### Requirement: Mnemo runs the full checks for the lead
The lead tool team_run_checks SHALL run every .coding/checks.toml command in a worker's checkout, or in the lead checkout, through the machine heavy-command slots, record each result on the worker's tip and report through a notice. Mnemo SHALL start this run itself after a review with no open findings whose tip lacks passing checks, and team_merge SHALL refuse while it runs.

#### Scenario: Clean review triggers the checks
- **WHEN** the lead records a review with no open findings
- **WHEN** the tip has no passing checks
- **THEN** Mnemo runs the checks and sends a notice
- **AND** team_merge succeeds once they passed

#### Scenario: Merge waits
- **WHEN** Mnemo is running a worker's checks
- **WHEN** the lead calls team_merge
- **THEN** the merge is refused until the notice

### Requirement: The lead can read the live tier setup
The lead tool team_tiers SHALL return every tier's models in order with their health, effort, run as and current resolution. Mnemo SHALL tell the external lead when the tiers changed since it last read them, and its tool descriptions SHALL refresh through a tools/list_changed notification when the tool list changes, including after Mnemo2 restarts.

#### Scenario: Tier change reaches the lead
- **WHEN** Claude Code leads
- **WHEN** the user sets High to run as a Claude Code agent and saves
- **THEN** the lead receives tools/list_changed
- **AND** the next dispatch result says the tiers changed

#### Scenario: Health is visible
- **WHEN** a model hit its usage limit
- **THEN** team_tiers shows it unavailable until a time

### Requirement: Agent workers get their warm checkout through Claude Code's worktree isolation
When Claude Code leads and launches an agent worker's Agent with isolation "worktree", Mnemo's WorktreeCreate hook SHALL hand Claude Code the worker's Mnemo pool checkout and SHALL register the agent with the worker. Outside Mnemo work the hook SHALL create the folder Claude Code would create itself.

#### Scenario: Isolated agent worker
- **WHEN** an agent worker waits for its agent
- **WHEN** the lead launches an Agent with isolation "worktree"
- **THEN** the agent runs in the worker's pool checkout on its branch
- **AND** the worker records the agent's id

#### Scenario: Other isolated agent
- **WHEN** no agent worker waits for its agent
- **WHEN** an Agent starts with isolation "worktree"
- **THEN** the hook creates a folder under .claude/worktrees
