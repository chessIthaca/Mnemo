## ADDED Requirements

### Requirement: Laya needs one decision from the user
The Tiers page SHALL ask only how much Laya helps pick the tier, and SHALL NOT offer inputs for confidence gates, minimum words, fallback tier, kNN, nudges or per-tier dispatch effort.

#### Scenario: Leaving without edits
- **WHEN** the user opens the Tiers page and changes nothing
- **WHEN** the user picks another page
- **THEN** the page changes without asking about unsaved edits

### Requirement: The Laya check runs itself and shows progress
Mnemo2 SHALL check Laya when a project opens if the test set or checkpoint changed, SHALL show its progress like indexing, SHALL apply the suggested gates itself, and MUST write nothing when Laya does not answer.

#### Scenario: Stale check at project open
- **WHEN** a project opens with a Laya choice other than off and no matching check
- **THEN** the status line shows Checking Laya with done and total
- **AND** the gates and the result are saved when it ends

#### Scenario: Nothing changed
- **WHEN** a project opens and the last check's fingerprint matches
- **THEN** no check runs and the page shows the last result

#### Scenario: Laya does not answer
- **WHEN** the classifier stops answering during a check
- **THEN** the check shows as failed
- **AND** no record is written and the gates stay as stored

