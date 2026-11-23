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

### Requirement: Drift nudges read only new guidance
The system SHALL run the drift check only on a lead or user message that can carry new work. It SHALL NOT classify text Mnemo2 generated itself (a route-fix brief or a plan-changed notice) or the first lead or user message a read-back worker receives, so those messages never produce a drift nudge and never spend the worker's one nudge. Any other lead or user message of at least `min_words` words SHALL still be classified in the background, and SHALL nudge the lead when the answer clears on a tier above the worker's.

#### Scenario: Read-back answer
- **WHEN** a worker dispatched with read_back receives its first lead message of at least min_words words
- **THEN** Laya is not asked about that message
- **AND** no drift nudge is sent
- **AND** the worker's one nudge stays available

#### Scenario: Plan notices before the read-back answer
- **WHEN** a read-back worker has received only Mnemo2 plan-changed notices
- **WHEN** the lead then sends its answer
- **THEN** Laya is not asked about the answer

#### Scenario: Route-fix prompt
- **WHEN** team_route_fix delivers a fix brief to a worker
- **THEN** Laya is not asked about the brief as drift
- **AND** no drift nudge is sent

#### Scenario: A later message adds scope
- **WHEN** a worker receives a lead message of at least min_words words that is not its read-back answer
- **WHEN** Laya clears the message on a tier above the worker's
- **THEN** the lead gets a "this got harder" notice
- **AND** a nudge row with reason drift is logged

### Requirement: Stale-read redirects are not strikes
The system SHALL NOT count a failed tool result that is a stale-read gate redirect (EDIT INTERCEPTED) or an unknown read receipt error toward a worker's strike streak, and SHALL NOT let it reset the streak either. An exact repeat of the previous call SHALL still count as a strike.

#### Scenario: Intercepted edit followed by a re-read
- **WHEN** a worker's edit is intercepted by the stale-read gate
- **WHEN** the worker re-reads the file and edits again
- **THEN** no strike nudge is sent for those calls

#### Scenario: Repeated identical intercepted call
- **WHEN** a worker sends the same intercepted edit call three times in a row
- **THEN** the repeats count as strikes as before

### Requirement: Agent tiers are decided before fold-down
Whether a dispatch goes to a Claude Code agent SHALL be decided from the tier routing settled on (the lead's explicit tier or Laya's cleared tier) before an empty tier folds down.

#### Scenario: Empty agent tier
- **WHEN** the high tier has claude_agent opus and no models
- **WHEN** Claude Code leads and dispatches on high
- **THEN** the step is handed to the Claude Code agent

### Requirement: Fix rounds on agent workers are recorded
team_route_fix on a Claude Code agent worker SHALL record the fix round as for a Mnemo worker and return the fix text with the SendMessage instruction for the registered agent; team_agent_done SHALL close the round.

#### Scenario: Routed fix
- **WHEN** the lead routes a finding to an agent worker
- **THEN** the worker is Fixing with the round recorded
- **AND** the answer carries the SendMessage instruction and the fix text

### Requirement: Usage-limited models are skipped
When a worker fails because its model hit a provider usage limit, Mnemo SHALL treat that model as unavailable for six hours, so tier resolution and fold-down skip it. In routing mode auto Mnemo SHALL resume the worker on its re-resolved tier; otherwise the failure notice SHALL name the resume call. Fix-round prompts SHALL ask for the narrowest covering tests instead of the full checks.

#### Scenario: Weekly limit
- **WHEN** a worker fails with a weekly usage limit error
- **THEN** its model is unavailable
- **AND** the next dispatch on that tier folds to the next model

#### Scenario: Auto mode continues
- **WHEN** routing mode is auto
- **WHEN** a worker fails with a usage limit
- **WHEN** its tier folds to another model
- **THEN** the worker is resumed on that model with its transcript

#### Scenario: Narrow fix rounds
- **WHEN** a finding is routed to its worker
- **THEN** the fix prompt asks for the narrowest covering tests, not run_checks

### Requirement: Pinned models that bypass an agent tier are flagged
A dispatch from the external lead that pins a model on a tier which runs as a Claude Code agent SHALL warn that the model bypasses the agent, and SHALL still start the Mnemo worker.

#### Scenario: Pinned model on an agent tier
- **WHEN** high runs as a Claude Code agent opus
- **WHEN** the lead dispatches with tier high and an explicit model
- **THEN** the result warns that the model bypasses the agent
- **AND** a Mnemo worker starts on the pinned model

### Requirement: Escalation honours run as
When Claude Code leads, moving an implementation worker onto a tier that runs as a Claude Code agent, by team_resume, auto escalation or usage-limit auto-resume, SHALL stop its Mnemo session and hand the step to the lead's agent with a claude_agent block. Without an external lead the move SHALL switch models as before.

#### Scenario: Resume onto an agent tier
- **WHEN** Claude Code leads
- **WHEN** the lead resumes a worker on a tier that runs as a Claude Code agent
- **THEN** the result carries a claude_agent block
- **AND** the worker has no Mnemo session

#### Scenario: In-app lead
- **WHEN** no external lead is attached
- **WHEN** a worker is resumed on that tier
- **THEN** it switches to the tier's model

### Requirement: Agent worker dispatches wait for the previous launch and are capped
A dispatch or hand-off that would leave a second agent worker waiting for its agent SHALL be refused, and so SHALL one that would make more than three agent workers run at once.

#### Scenario: Second waiting agent
- **WHEN** one agent worker waits for its agent
- **WHEN** the lead dispatches another agent-tier step
- **THEN** the dispatch is refused and names the waiting worker

#### Scenario: Cap of three
- **WHEN** three agent workers are Working or Fixing
- **WHEN** another agent-tier dispatch arrives
- **THEN** it is refused
