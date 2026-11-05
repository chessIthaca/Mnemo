## ADDED Requirements

### Requirement: Model prices have one editor
Mnemo2's settings dialog SHALL edit model prices only with each model on the Endpoints page, and the Savings page SHALL NOT offer a pricing table.

#### Scenario: Savings page points to Endpoints
- **WHEN** the user opens the Savings page
- **THEN** no pricing table is shown
- **AND** a line says prices are set with each model on the Endpoints page

