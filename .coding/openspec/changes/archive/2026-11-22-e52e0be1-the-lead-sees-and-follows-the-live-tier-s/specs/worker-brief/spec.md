## ADDED Requirements

### Requirement: A worker handed to an agent mid-step continues from its checkout
When a worker moves to a Claude Code agent tier mid-step, the agent prompt SHALL carry the worker brief plus where the previous worker left off: its previous model, and the instruction to read git status, the log since the base commit and the diff before changing anything.

#### Scenario: Continuation prompt
- **WHEN** a worker on a Mnemo model is resumed onto an agent tier
- **THEN** the agent prompt names the previous model and the base commit

