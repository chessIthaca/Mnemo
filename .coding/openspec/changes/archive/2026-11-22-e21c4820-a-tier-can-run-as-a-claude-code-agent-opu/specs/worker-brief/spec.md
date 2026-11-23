## ADDED Requirements

### Requirement: The agent prompt carries the worker brief
The prompt handed to a Claude Code agent SHALL contain the same plan, step and brief a Mnemo worker receives, plus the worktree it must work in, the branch to commit on, and the rules never push, never git stash, never merge, and run the project checks before the last commit.

#### Scenario: Prompt contents
- **WHEN** a step is handed to a Claude Code agent
- **THEN** the prompt names the worktree path and branch
- **AND** the prompt contains the step text and the brief

