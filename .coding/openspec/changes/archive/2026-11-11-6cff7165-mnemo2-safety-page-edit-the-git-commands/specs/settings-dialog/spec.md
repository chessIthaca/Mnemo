## ADDED Requirements

### Requirement: The Safety page edits the git commands that always ask
The Safety page SHALL show the git subcommands that always make the lead ask before running them ([git] core_operations) as an editable list, and saving it SHALL apply to the open lead's next git command without a restart.

#### Scenario: Default list
- **WHEN** the user opens Settings → Safety on a config without a [git] section
- **THEN** the git field shows merge, push

#### Scenario: Saving a new list
- **WHEN** the user enters merge, push, rebase and saves
- **THEN** config.toml holds core_operations = ["merge", "push", "rebase"] under [git]
- **AND** the open lead asks before its next git rebase without a restart

#### Scenario: A malformed entry is refused
- **WHEN** the user enters push --force and saves
- **THEN** the page shows an error naming the entry
- **AND** config.toml is unchanged

