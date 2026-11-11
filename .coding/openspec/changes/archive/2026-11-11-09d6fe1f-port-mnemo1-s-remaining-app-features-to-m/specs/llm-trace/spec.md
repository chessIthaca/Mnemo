## ADDED Requirements

### Requirement: Steering notes are counted in the Trace tab
The Trace tab SHALL show, for the current app run and across the lead and every worker, how often each steering note in a tool result was shown and how often the agent's next call followed it, plus file edits made with file tools versus through the shell.

#### Scenario: Notes were shown
- **WHEN** the agents received steering notes in tool results during this app run
- **WHEN** the user opens the Trace tab's Insights
- **THEN** each note kind that was shown appears with how often it was shown and followed
- **AND** the total followed share is shown as a percentage

#### Scenario: No notes yet
- **WHEN** no steering note fired during this app run
- **WHEN** the user opens the Trace tab's Insights
- **THEN** the section says there are no steering notes yet this run

