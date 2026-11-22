## ADDED Requirements

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

