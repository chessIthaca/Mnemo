# Settings dialog

## Purpose
How Mnemo2's settings dialog protects unsaved edits when the user closes it or moves to another page.

## Requirements

### Requirement: Leaving with unsaved edits asks first
The settings dialog SHALL ask for confirmation before closing or switching pages while any page holds unsaved edits, and SHALL close or switch without asking when none does.

#### Scenario: Close with unsaved edits
- **WHEN** a settings page has an unsaved edit
- **WHEN** the user presses the close button, Escape or the backdrop
- **THEN** the dialog stays open and asks to discard unsaved changes
- **AND** Keep editing keeps the edit
- **AND** Discard changes closes the dialog

#### Scenario: Switch page with unsaved edits
- **WHEN** a settings page has an unsaved edit
- **WHEN** the user picks another page in the rail
- **THEN** the page does not change until the user chooses Discard changes

#### Scenario: No unsaved edits
- **WHEN** no settings page has an unsaved edit
- **WHEN** the user closes the dialog
- **THEN** the dialog closes at once

### Requirement: Model prices have one editor
Mnemo2's settings dialog SHALL edit model prices only with each model on the Endpoints page, and the Savings page SHALL NOT offer a pricing table.

#### Scenario: Savings page points to Endpoints
- **WHEN** the user opens the Savings page
- **THEN** no pricing table is shown
- **AND** a line says prices are set with each model on the Endpoints page

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
