## ADDED Requirements

### Requirement: The agent prompt sets the working rules
The prompt handed to a Claude Code agent SHALL tell it to start every shell command with a cd into its worktree, to keep shell commands in the foreground unless they run long, and to post a one-line team_progress note after each milestone.

#### Scenario: Prompt rules
- **WHEN** a step is handed to a Claude Code agent
- **THEN** the prompt carries the cd, foreground and progress rules

