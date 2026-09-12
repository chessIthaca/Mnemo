## Verdict: PASS

Round-2 verification of plan 041dd8d0 "Steer feature-scale bug fixes to implementation plans" (backlog 51ee41c1), branch wt/agenticcoding, commit eb73f9c (= HEAD; working tree clean — `git diff HEAD` and `git status --short` both empty). All four round-1 LOW findings are fixed correctly in the committed source, the post-round-1 delta is exactly the four fixes (nothing else changed relative to the round-1-reviewed tree), and the carve-out wording is coherent across all five guidance surfaces. No new findings.

## 1. The four round-1 fixes — verified present and correct in the committed source

**LOW-1 — PLAN.md (:420-424).** The `bug_fixing` bullet now opens "CONTAINED bug fixes use this kind (never `implementation` for a contained defect). A bug-triggered FEATURE (the fix adds capabilities, new dependencies, or spans multiple modules) belongs in `implementation` with the bug documented as motivation in goal/context (2027-01-11, backlog 51ee41c1 — cf. plan c87093c1)." — exactly the claimed text. The rest of the bullet (locked skeleton, checkable sub-items, required `bug` param, regression-test validation, finish capture) is intact, and the superseded "bugs ALWAYS use this kind" restatement is gone.

**LOW-2 — README.md (:36).** The "Bugs get their own plan kind" bullet now scopes `kind: "bug_fixing"` to CONTAINED defect fixes, names the alternative ("a bug-triggered FEATURE (the fix adds capabilities, new dependencies, or spans multiple modules) belongs in `kind: "implementation"` with the bug documented as motivation in goal/context instead"), and mentions the advisory create_plan steering note with the AND heuristic ("more detailed steps than the locked skeleton AND spanning ≥3 modules"). Matches the claimed fix.

**LOW-3 — run_all.rs RUN_ALL_STEER rule (5) (:311-319).** The preamble rule now carries the carve-out: "…create the plan with kind \"bug_fixing\" and put the symptom in the required `bug` parameter — the reproduce→root-cause→fix→verify skeleton is then enforced from the first plan — EXCEPT bug-triggered FEATURES (the fix adds capabilities, new dependencies, or spans multiple modules): file those as kind \"implementation\" with the bug documented as motivation in goal/context; bug_fixing is for contained defect fixes; otherwise the default implementation kind applies." The steer test `run_all_prompt_steers_defect_items_to_bug_fixing` (:1004-1021) asserts two literals — `bug_fixing` and `` `bug` parameter `` — both present verbatim in the amended text (:313-314), so the test passes unchanged; the other preamble-test literals ("never commit work to main", "auto-forks") live in rules (1)-(4), untouched. The trailing "otherwise the default implementation kind applies" now follows the carve-out clause — both parses (else-branch of the DEFECT conditional, or else-branch of "contained") resolve to the same behavior, so no ambiguity is introduced.

**LOW-4 — the AND-legs pin (plan.rs :2656-2725).** `create_plan_bug_fixing_and_legs_pin_independently` has exactly the two claimed cases, each with exactly ONE leg firing:
- Case 1 (9441d776 shape): 4 steps + context referencing `src/workflow/plan_file.rs`, `src/workflow/mod.rs`, `src/tool/workflow/plan.rs`, `src/agent/factory.rs` → distinct modules {workflow, tool, agent} = 3 ≥ 3 (module leg FIRES) while 4 > 4 is false (step leg FAILS) → asserts no NOTE.
- Case 2 (single-module shape): 6 steps all in `src/codegraph/extract.rs` → modules {codegraph} = 1 < 3 (module leg FAILS) while 6 > 4 (step leg FIRES) → asserts no NOTE.

Both cases assert `result.success` and `!output.contains("NOTE (backlog 51ee41c1)")`. The legs are now independently pinned — LOW-4 closed.

## 2. No regressions

Read-only reviewer (no shell): verified statically rather than by execution. Every pinned literal checks out against the amended texts:
- `stable_head_carries_bug_fixing_habit` (prompt.rs :1387-1410): all four literals — `kind:"bug_fixing"`, `never "implementation"`, `bug:{symptom}`, `BUG: memory auto-captured at finish` — remain in the amended PLAN KIND text (the carve-out is inserted after "at finish" with an em-dash, keeping the substring intact).
- `stable_head_carries_feature_scale_carve_out` (:1412-1435): all three literals — "bug-triggered FEATURES", "bug_fixing is for CONTAINED", "documented as motivation in goal/context" — present.
- `run_all_prompt_steers_defect_items_to_bug_fixing`: both literals present (above).
- The fire-path test's payload (5 steps; modules {codegraph, tool, agent}) trips both legs → NOTE fires, plan still files as BugFixing; the silent-path and both AND-legs payloads each fail at least one leg → no NOTE. Module counting hand-verified for every case.
- No other test pins the PLAN KIND or RUN_ALL_STEER rule-(5) text (searched; the only other "ALWAYS kind" hits are the prompt's own base rule — now followed by EXCEPT — and historical problem-statements in backlog.jsonl / the plan file, which correctly quote the pre-fix state).

The full-workspace green run (2324 + 16 + 297 + 4 + 2 + 2 passed, 0 failed) is attested by the commit message; nothing in the committed code contradicts it.

## 3. Post-round-1 delta is exactly the four fixes

eb73f9c touches 8 files (318+/9−): the three expected `.coding` bookkeeping files (backlog.jsonl 51ee41c1 pending→in_flight with plan_id; new plan file 041dd8d0.md; the round-1 report) plus PLAN.md (LOW-1), README.md (LOW-2), src-tauri/src/ipc/run_all.rs (LOW-3), src/agent/prompt.rs, and src/tool/workflow/plan.rs. Cross-referencing the round-1 report's reviewed-file list (prompt.rs + plan.rs + `.coding` only): the prompt.rs and plan.rs hunks in eb73f9c match round-1's verified descriptions exactly — plan.rs carries exactly round-1's three tests PLUS the new AND-legs pin; prompt.rs only the round-1 amendment + carve-out test — and run_all.rs / PLAN.md / README.md appear for the first time (the three doc/steer fixes). So the post-round-1 changes are precisely: the PLAN.md bullet, the README bullet, the run_all.rs steer text, and the AND-legs test. Nothing else regressed.

## 4. Carve-out consistency across surfaces

Same rule, same escape hatch on all five guidance surfaces (plus the steering note itself):
- Compiled prompt (prompt.rs :84-90): "EXCEPT bug-triggered FEATURES (the fix adds capabilities, new dependencies, or spans multiple modules): file those as kind:\"implementation\" with the bug documented as motivation in goal/context; bug_fixing is for CONTAINED defect fixes."
- Kind schema description (plan.rs :527): "'bug_fixing' is for CONTAINED bug fixes — never 'implementation' for a contained defect … A bug-triggered FEATURE (the fix adds capabilities, new dependencies, or spans multiple modules) belongs in kind=implementation with the bug documented as motivation in goal/context."
- Run-all steer (run_all.rs :315-318): same criteria, same escape hatch, "bug_fixing is for contained defect fixes".
- PLAN.md (:420-424) and README.md (:36): same criteria, same escape hatch.
- Steering note (plan.rs :803-817): "consider re-filing as kind=implementation with the bug documented as motivation in goal/context — bug_fixing is for contained defect fixes."

The three feature-scale criteria (adds capabilities / new dependencies / spans multiple modules) and the escape-hatch phrasing (bug documented as motivation in goal/context) are identical in substance everywhere; only register differs per surface (prompt imperative, schema declarative, docs explanatory). Coherent.

## Notes (considered, no finding)

- **docs/why-mnemo-deck.md slide 10 (:624)** still says "Defects **must** use `kind: bug_fixing`" without the carve-out. Checked and accepted: the deck is a pitch artifact outside the sanctioned docs-sync surfaces (README, PLAN.md, module docs, config examples), round-1 — which set the docs-sync bar for this change — did not flag it, and the slide's claim remains true for the case it presents (contained defects, the locked-skeleton discipline). A one-line deck tweak can be queued if pitch-level precision is wanted.
- **prompt.rs :1389's test comment** ("Phase 4: bugs ALWAYS use kind=bug_fixing, never implementation") is a historical phase note on a test whose four assertions all still hold; the carve-out test directly below (:1412) documents the exception. Not stale enough to require a commit.
- **Multi-platform neutrality:** the round-2 delta is docs + prompt/steer text + a pure-logic test (tempdir + string assertions, same harness as the existing tests) — no platform APIs, paths, or shell syntax. **File-tools policy:** no shell-mutation patterns in the diff. Clean.
