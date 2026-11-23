## ADDED Requirements

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

