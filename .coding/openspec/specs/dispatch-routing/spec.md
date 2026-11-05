# Dispatch routing

## Purpose
How Mnemo2 picks a worker's tier when the lead dispatches an implementation task, and how model changes become training labels for Laya.

## Requirements

### Requirement: Routing modes
The system SHALL support the routing modes off, shadow, suggest and auto, stored in the `[routing]` table of the global `mnemo2.toml`, with off as the default.

#### Scenario: Default mode
- **WHEN** `mnemo2.toml` has no `[routing]` table
- **THEN** dispatch routing runs in off mode

#### Scenario: Mode saved on click
- **WHEN** the user picks a routing mode on Settings > Tiers
- **THEN** the mode is saved to `mnemo2.toml` at once

### Requirement: One classification per dispatch
The system SHALL classify each implementation dispatch at most once, from the step text plus the brief, before the merge lock is taken and within a 3 second budget; a missing answer or a timeout counts as offline. Audits SHALL never be classified.

#### Scenario: Laya times out
- **WHEN** Laya does not answer within 3 seconds
- **THEN** the dispatch proceeds as if Laya were offline

#### Scenario: Audit dispatch
- **WHEN** the lead dispatches a read-only audit
- **THEN** no routing classification runs and no route row is logged

### Requirement: Explicit choice wins
The system SHALL use an explicit `tier` or `model` given by the lead over any routing answer; only auto mode applies the classified tier.

#### Scenario: Lead names a tier in auto mode
- **WHEN** routing mode is auto and the lead dispatches with tier `routine`
- **THEN** the worker runs on the routine tier

#### Scenario: Suggest mode
- **WHEN** routing mode is suggest and the lead dispatches with tier `auto`
- **THEN** the dispatch is refused

### Requirement: Confidence gates read the top probability
The system SHALL gate a routing answer on Laya's top label probability, with default minimums of 0.40 for four labels, 0.475 for three and 0.60 for two, each overridable in `mnemo2.toml` `[routing]`; below the gate the answer is treated as unsure.

#### Scenario: Unsure answer
- **WHEN** Laya's top probability is below the configured four-label minimum
- **THEN** the routing decision is not cleared and auto mode uses the fallback tier

### Requirement: kNN overlay over the routing log
The system SHALL first ask a kNN index over labelled rows of `dispatch_routing.jsonl` within a 500 ms sub-budget, using its answer only when at least `knn_min_rows` (30) labelled workers exist and the top label share is at least `knn_min_share` (0.80); otherwise Laya answers. Each route row SHALL record its source (knn or laya).

#### Scenario: Too few labels
- **WHEN** fewer than 30 workers carry a label
- **THEN** Laya answers and the route row's source is laya

### Requirement: Model changes are training labels
The system SHALL append a reroute row to `dispatch_routing.jsonl` for every user or lead model change on a worker, in every routing mode, after provisioning succeeded and stamped with the real worker id.

#### Scenario: Routing off
- **WHEN** routing mode is off and the user switches a worker's model
- **THEN** one user reroute row is appended
- **AND** team_status shows the destination tier

### Requirement: Escalation and fix-round step-down
The system SHALL escalate a worker one tier up on a second failed review round on the same finding, a route-fix round that leaves its finding open, a strike streak, or a high-severity audit finding after a fix round. A stepped-down fix round that leaves its finding open SHALL return the worker to its previous tier, and auto SHALL never lower a high-severity or shared-contract finding.

#### Scenario: Escalation outside auto mode
- **WHEN** routing mode is shadow and a worker's finding stays open after a route-fix round
- **THEN** the lead gets a notice suggesting team_resume with the next tier up

#### Scenario: Step-down fails
- **WHEN** a route-fix round on a lower tier leaves its finding open
- **THEN** the worker returns to its previous tier and a fix_downshift_restore reroute row is logged

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
