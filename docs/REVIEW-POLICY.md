# Review policy

How Mnemo2 decides that a worker's branch may merge. The lead's operating rules
are in [agent.md](../agent.md) (the Mnemo2 team merge policy); the native gate
lives in `src-tauri/src/mnemo2/` and is mirrored for the UI in
`mnemo2/src/lib/review.ts`.

## Acceptance scenarios first

A plan that will dispatch `review_risk: "high"` carries a `## Acceptance
scenarios` section with at least one observable bullet (a command, a persistence
state, a concurrency outcome, a security property, or a UI behavior) written
before implementation: `plan_create` `acceptance_scenarios`, or `plan_update`
`add_acceptance_scenarios`. Other plans are encouraged to include it.
`team_dispatch` rejects a high-risk dispatch whose plan has none, workers
receive the scenarios with their plan, and the lead re-checks them on the
integrated tree before the plan closes.

## Unanchored pass, then evidence

The lead reads the target diff first (`git_read` op `diff`/`show`, the **Diff**
tab, or `git diff <base>..<tip>`) and forms an opinion *before* reading the
worker's summary, findings, or test claims. It then records one evidence object
per review:

```text
evidence: {
  unanchored_pass: boolean,
  intent: string,
  execution: string,
  integration: string,
  test_quality: string,
  residual_risk: string,
  independent_reviewer_id?: string | null,
  audit_adjudication?: string | null
}
```

`intent` says what the change was meant to do and whether the diff matches the
plan; `execution` covers the real changed call sites and error paths;
`integration` covers contracts, persistence, concurrency, and neighbouring
modules; `test_quality` asks whether tests genuinely exercise the change
(including a red-before/green-after regression test for a defect fix);
`residual_risk` names what remains unverified or assumed. A **standard** review
requires `unanchored_pass: true` and all five assessment strings nonempty. A
no-findings result still needs a substantive rationale — "looks good" or a
restated summary fails the gate.

## Checks on the exact commit

The lead runs the verification commands in the worker's checkout and records
each one with `team_check` (name, passed or failed, detail). Checks bind to the
worker's current commit: any later commit (a `review_fix`, a worker fix, a
routed fix round) clears them and the review, so both are repeated.

## Findings

Every open finding (title, detail, file, line, severity high or low) blocks the
merge, whatever its severity. A finding closes as fixed with evidence, or as
wontfix with a written justification. The lead can apply a tiny exact fix
itself with `review_fix` (one replacement in one non-test source file); larger
fixes go back to the worker with `team_route_fix`, which hands it every open
finding at once.

## Risk and independent audit

The lead marks shared contracts, persistence, concurrency, security, and large
or cross-cutting changes `review_risk: "high"`. A high-risk worker cannot merge
on lead review alone. `team_audit_dispatch { worker_id, reviewer_id, tier?,
model?, reasoning_effort?, brief? }` creates a fresh read-only audit worker
against the implementation worker's branch/tip and plan context, with no plan
step and **without** the implementation worker's summary or conclusions. The
audit runs on the `high` tier unless `tier` names another. The audit worker
cannot edit files, commit, merge, spawn, ask the user, or tick a plan step; its
completion notice asks the lead to adjudicate the target review, not merge the
reviewer. Fresh context is required for independence; a different model is
preferred when available but not required. The lead links the completed audit
as `independent_reviewer_id` and writes `audit_adjudication` — what was accepted
or rejected and why. The host validates kind, target, exact tip, and terminal
non-failed state at review and merge.

## Staleness

A fix commit (`review_fix`, a worker fix, or a routed fixer) invalidates the
prior lead review and makes the previous audit tip stale; evidence and any
audit must be repeated at the new tip. Worker records carry `kind`
(`"implementation"`/`"audit"`, default implementation), `review_risk`, and
optional `audit_target`/`audit_tip`. Legacy ledger records deserialize as
implementation/standard with no evidence, so they need a fresh review — and an
audit where high risk — before merge.

## Merge eligibility

The worker must be in review; every finding must be fixed or `wontfix` with a
written justification; the structured review evidence must be complete for the
current tip; a high-risk worker must have a completed audit linked to that same
tip and a nonempty adjudication; and every branch-tip check must have passed. A
fix or fixer commit replaces the check result and must be re-run before merge.
The lead is the reviewer of record and remains accountable for the
adjudication. The merge is a local `--no-ff` merge into the lead branch, one
worker at a time. It halts on a conflict, never pushes and never targets
`main`. After a clean merge Mnemo2 closes the worker's session and deletes its
checkout and branch (`git branch -d`, so a branch that is not fully merged is
kept).

## Closing a plan

After the last step merges, the lead re-checks the plan's acceptance scenarios
on the integrated tree. The plan closes when the team review passes, and a
review report is written to `.coding/reviews/`.

## Landing on main

Work reaches `main` through a pull request from the lead branch; the `check` job
of `.github/workflows/ci.yml` must pass first. See **Branch policy** in
[agent.md](../agent.md).
