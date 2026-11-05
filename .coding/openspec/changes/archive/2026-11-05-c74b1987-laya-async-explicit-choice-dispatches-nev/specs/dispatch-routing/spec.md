## ADDED Requirements

### Requirement: Non-gating classification never blocks dispatch
When a dispatch carries an explicit lead or user tier or model choice, the system SHALL start the worker without waiting for the routing classification, SHALL run the classification in the background under the same permit and timeout, and SHALL attach its label to the worker's routing record when it completes, logging it as calibration and training data.

#### Scenario: Explicit-tier dispatch starts without the classification
- **WHEN** a lead dispatches with an explicit tier
- **WHEN** the routing classifier is still pending
- **THEN** the worker starts immediately
- **AND** the routing block shows routed_by lead with a null Laya label

#### Scenario: The background label lands on the record
- **WHEN** the background classification completes
- **THEN** the worker's routing record carries the Laya tier, confidence and cleared flag
- **AND** the training data records the override label

#### Scenario: Non-explicit dispatches still gate on the classification
- **WHEN** a dispatch carries no explicit tier or model choice
- **THEN** an unsure classifier refuses a Suggest+auto dispatch
- **AND** a cleared decision runs Laya's tier in Auto mode
- **AND** offline or too-short text produces the same skip reasons as before

