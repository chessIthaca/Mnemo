## Verdict: FINDINGS (0 high, 6 low)

Review of all uncommitted changes on wt/agenticcoding for plan b58439f4 "Plan resumability gate" (8 files, +675/−95). The gate is correctly implemented and well-tested on the main paths; all six findings are low-severity (docs sync ×3, one corner-case simulation divergence, one heuristic weakness, one commit-hygiene note). No correctness, security, or constitution violations on the shipped paths.


## Scope & method

Reviewed `git diff HEAD` (8 files: src/backlog.rs, src/tool/workflow/plan.rs, src/agent/factory.rs, src/agent/tests.rs, src/runtime/agent.rs, tests/integration/workflow_integration.rs, README.md, .coding/backlog.jsonl) plus the untracked files. Read the full validator implementations, both tool `execute` bodies, the engine's `Workflow::update_plan` (src/workflow/mod.rs:582-730) to check gate-vs-engine semantics, `body_carries_detail_marker` (src/backlog.rs:70-112), the context-budget test (src/agent/factory.rs:1798-1810), PLAN.md's workflow section, and the code-graph caller map for the engine's `create_plan` (81 incoming edges — all test functions plus internal delegations; production flows only through the tool layer, confirming the permissive-engine-by-design claim).

**Test caveat:** this reviewer has no shell tool (read-only surface), so `cargo test` could not be independently re-run. Verification relies on the parent's reported run (2211 lib + 16 integration passed, 0 failed, exit 0, warning-free under `#![deny(warnings)]`), cross-checked statically: every changed fixture was hand-verified gate-compliant (contexts ≥40 chars; steps carry path-ish tokens or no-code markers), and no src-tauri test constructs plans through the tool (only the production registration site, src-tauri/src/ipc/spawn.rs:791), so the root-crate run covers the gate.

## Verified correct

- **Gate wiring (create):** runs after the shape checks (blank title/goal, bug param, steps-empty) and before the lock/ownership check — the same layering as the existing blank-title/blank-goal guard. A sub-agent with a thin plan sees the gate error before the ownership error; consistent with the established guard order, and the factory.rs sub-agent fixture was made gate-compliant precisely so the ownership assertion still holds.
- **Gate wiring (update):** after `plan_mutations_allowed`, reads the active plan (top of stack — the same frame the engine's `update_plan` mutates). New steps checked per-step; context checked as the effective post-update text. Empty/whitespace context is ungated and the engine also treats it as a no-op (mod.rs:643) — consistent. Title-only / regression_test-only calls ungated as documented.
- **Engine consistency:** the append separator `\n\n` matches the engine (plan.rs:891 vs mod.rs:647); bug_fixing steps are exempt in the gate so the engine's locked-skeleton refusal (mod.rs:621-628) owns that path with its specific error.
- **Shared vocabulary:** `body_carries_detail_marker` → `pub(crate)` with an accurate doc comment; the path regex + no-code markers are platform-neutral (both `/` and `\`, drive prefixes, no Windows-only APIs).
- **Tests:** 9 pure-validator unit tests (accept, thin, blank, 39/40 boundary, path-free naming, path-detection variants incl. the "and/or" prose negative, bug-design requirement, design-in-detailed-steps, error numbering) + 8 tool-level tests (sparse reject, complete accept, no-code-marker accept, offending-step naming, bug-design reject/accept, update steps reject, update thin-context-replace reject, update append-onto-substantive pass). Assertions updated to match the new fixtures.
- **Constitution checks:** no shell-based file mutation in non-test code; no `#[allow]`; no Windows-only APIs/paths in library code; README bullet added and accurate; module doc added; create_plan schema descriptions (context/steps/kind) updated.

## Findings

### L1 (docs sync — PLAN.md): the enforced-workflow section doesn't mention the gate

PLAN.md's `## Enforced plan-first workflow` section (lines 357-383) documents create_plan/update_plan semantics in detail — chunked writes, plan kinds, the bug_fixing skeleton — but not the resumability gate. README has the bullet; the module doc and schemas are updated; PLAN.md (the technical-decisions doc the project constitution names for reviewer doc-sync checks) is the one stale surface. Suggested: one sentence after the chunked-writes paragraph (~:362), e.g. "Both tools enforce a resumability gate: context ≥40 chars (symptom/root-cause, file anchors, verification commands), every step names file path(s) or a no-code marker, bug_fixing plans name the regression-test design; update_plan validates the effective post-update text."

### L2 (doc comment): `context_issue`'s doc comment is self-contradictory

src/tool/workflow/plan.rs:227-243 — the doc comment opens with the overall-gate paragraph ending "Returns every violation found (empty = the plan passes) so the retry fixes all of them in one shot." (that is `validate_plan_resumability`'s contract) and closes with "Returns the first violation, if any." (the actual contract). The first paragraph belongs on `validate_plan_resumability` (which carries its own shorter doc at :306-308); as attached it's an editing artifact that misleads a maintainer. Fix: move the overview paragraph to `validate_plan_resumability`, keep only the context-side description on `context_issue`.

### L3 (corner-case divergence): update_plan's append simulation doesn't mirror the engine's empty-existing behavior

The gate simulates append unconditionally as `format!("{existing}\n\n{new_ctx}")` (plan.rs:889-891), but the engine REPLACES — no separator — when the existing context is empty/whitespace (`if append && !frame.plan.context.trim().is_empty()` → else-branch replace, src/workflow/mod.rs:644-650). Reachable path: a grandfathered pre-gate plan (resumed via load_latest) with an empty context + `append=true` + a 38-39-char chunk passes the gate (simulated ≥40 via the two separator chars) while the engine persists only the 38-39 chars — below the gate's own bar. Under-enforcement only; no data loss. Fix: mirror the engine's condition — `let effective = if args.append && !existing.trim().is_empty() { format!("{existing}\n\n{new_ctx}") } else { new_ctx.to_string() }`.

### L4 (heuristic weakness): the bug-design substring check matches unrelated words

plan.rs:265 — `lower.contains("test") || lower.contains("regression")` is satisfied by "la**test**", "a**test**ed", "con**test**", "pro**test**". A bug_fixing context like "reproduce with the latest driver; root cause: unwrap in src/foo.rs:42" passes without naming any test. Under-enforcement only (no false rejections — a plan that does name its test always contains the substring). Suggested tightening: word-boundary match (e.g. `\btest` / `\bregression` via the existing regex dep, or `lower.split_whitespace().any(|w| w.starts_with("test") || w.starts_with("regression"))`).

### L5 (docs — schema): update_plan's `steps` description doesn't mention the per-step requirement

plan.rs:832-839 — create_plan's steps description gained "Resumability gate: every step names file path(s) or a no-code marker." but update_plan's steps description (which says "Same shape as create_plan's steps") doesn't. An agent replacing steps via update_plan learns the bar only from the rejection. The rejection error is actionable, so impact is small; one clause ("every step names file path(s) or a no-code marker — the resumability gate") closes it.

### L6 (commit hygiene): untracked build logs must not be committed

`tmp_build_frontend.txt` and `tmp_build_tauri.txt` sit untracked in the repo root (leftover build verification logs). Stage the changed files explicitly (plus `.coding/plans/b58439f4.md`, which SHOULD be committed — plans travel with git — and this review report) rather than `git add -A`, or delete the two tmp files before committing.

## Notes (no action required)

- **Planning budget headroom:** the tools-array context-budget test reports Planning at 15,976/16,000 — 24 chars of headroom. The test guards the boundary with an actionable failure message, so future growth trips it deliberately; just be aware the next schema edit on the Planning surface will likely need a trim or a deliberate ceiling raise.
- **Strictness friction is by design:** a legitimate step like "Run cargo test and confirm green" (no path token, no no-code marker) is rejected until a path is added — that is the designed bar (the error tells the caller exactly what to add), noted here only so it isn't re-litigated as a bug.
- The `.coding/backlog.jsonl` change is the app's own status flip (pending → in_flight with plan linkage) — expected bookkeeping, not a code change.
