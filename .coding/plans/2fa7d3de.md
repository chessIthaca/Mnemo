# Plan: Implement chain-instruction + retry-cadence prompt edits (① + ④)

## Goal
Implement the two recommended mitigations for agent mid-task stopping: (①) a positive "chain consecutive obvious steps" bullet in CODING_SYSTEM_PREAMBLE, and (④) a retry-cadence clause appended to the APP_RULES error-retry bullet. Both are cache-stable stable-head text edits. Fold factor 3's intent (tool results are continuations) into ①'s wording.

## Kind
implementation

## Context


## Steps
- [x] 1. **Add chaining bullet to preamble (① + ③-folded) — prompt.rs:33-34**
- [x] 2. **Add retry-cadence clause to APP_RULES error-retry bullet (④) — prompt.rs:101-103**
- [x] 3. **Reconcile + extend co-located tests in prompt.rs test module**
- [x] 4. **Run cargo test, fix any failures**
