## ADDED Requirements

### Requirement: Settings has a Machine page
The Settings dialog SHALL have a Machine page under All projects that edits how many heavy build and test commands may run at once across every Mnemo2 instance on the machine, and SHALL show whether a shared Rust build cache (sccache) was found and the shared model host's status.

#### Scenario: Changing the build limit
- **WHEN** the user sets Builds and tests at once to 1 and saves
- **THEN** config.toml carries [general.machine] heavy_commands = 1
- **AND** every running instance applies it to the next heavy command

### Requirement: The Models page says what the embedding models are for
The Models page SHALL describe the memory embedding model as serving memory search over specs, decisions, bugs and HOW records, plan, review and backlog summaries, and routing and failure-triage examples, and SHALL describe the code-search model separately.

#### Scenario: Reading the Models page
- **WHEN** the user opens Settings > Models
- **THEN** the embedding section names memory search, routing examples and code search with their models

