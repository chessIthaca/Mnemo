## ADDED Requirements

### Requirement: Agent prompts use no Mnemo tools
The prompt of an agent worker SHALL tell the agent to commit on its branch in its own checkout and SHALL NOT ask it to use Mnemo tools or to change directory into a checkout.

#### Scenario: Plain agent prompt
- **WHEN** an agent-tier step is dispatched
- **THEN** the claude_agent prompt names the branch
- **AND** it names no team_* or plan_* tool and no cd rule

