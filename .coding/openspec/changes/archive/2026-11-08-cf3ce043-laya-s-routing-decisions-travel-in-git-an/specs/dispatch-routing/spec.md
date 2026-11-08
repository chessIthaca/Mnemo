## ADDED Requirements

### Requirement: Routing decisions travel with the repository and the installer
A project that keeps a `.coding/laya/` folder SHALL carry its own routing decisions in `.coding/laya/dispatch_routing.jsonl`, as verbatim rows of the routing log whose plan belongs to that project, and the Mnemo2 installer SHALL ship that file. When a project opens, the system SHALL import a shipped copy by appending only, SHALL import silently when this computer has no decisions the copy lacks, and SHALL ask the user before importing when it has.

#### Scenario: A fresh computer imports silently
- **WHEN** a project opens on a computer whose routing log lacks the shipped rows and has none of its own
- **THEN** the rows are appended to the routing log without a question

#### Scenario: A conflicting copy asks
- **WHEN** a project opens and this computer has decisions the shipped copy lacks, and the copy has decisions this computer lacks
- **THEN** the user is asked whether to update
- **AND** updating appends the missing rows and removes nothing
- **AND** declining leaves the log unchanged and the same copy does not ask again

#### Scenario: Only opted-in projects export
- **WHEN** a worker merges in a project without a .coding/laya folder
- **THEN** no decision rows are written into that project

