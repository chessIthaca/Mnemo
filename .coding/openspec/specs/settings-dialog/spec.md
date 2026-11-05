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
