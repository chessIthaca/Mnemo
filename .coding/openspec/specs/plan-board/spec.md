# plan-board

## Purpose
How the plan board ties plans to the project's OpenSpec current-state specs: plan_create and plan_update refuse steps that touch a capability's governed files unless the plan carries a spec delta or an explicit no-spec-impact declaration.

## Requirements

### Requirement: Spec-impact gate on plan creation
plan_create SHALL refuse a plan whose steps touch files governed by a capability's file map unless the plan carries a spec delta for that capability or an explicit no-spec-impact declaration with a reason; the refusal SHALL name the capability and both remedies. A spec delta that creates a new capability SHALL declare its governed files. plan_update add_steps SHALL apply the same gate to steps added after creation.

#### Scenario: A governed-file plan without a delta is refused
- **WHEN** a plan's steps touch a capability's governed files
- **WHEN** no spec delta for that capability is attached
- **WHEN** no explicit no-impact declaration is carried
- **THEN** plan_create refuses
- **AND** the refusal names the capability and both remedies

#### Scenario: A delta or an explicit declaration passes
- **WHEN** the same plan carries a spec delta for the capability
- **WHEN** or an explicit no-spec-impact declaration with a reason
- **THEN** plan_create accepts the plan

#### Scenario: New capabilities declare their governed files
- **WHEN** a spec delta adds a new capability without a governed-files entry
- **THEN** the plan is refused until the files are declared

#### Scenario: Steps added later are gated too
- **WHEN** plan_update add_steps names a governed file without a delta or declaration
- **THEN** the same refusal fires
