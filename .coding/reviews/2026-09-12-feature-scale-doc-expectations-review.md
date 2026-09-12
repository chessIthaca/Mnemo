## Verdict: FINDINGS (0 high, 1 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 7d333b1d "Feature-scale bug-fix documentation expectations (e5a84ce9)" — 6 source files (agent.md, src/workflow/plan_file.rs, src/workflow/mod.rs, src/tool/workflow/plan.rs, README.md, PLAN.md) plus `.coding/` bookkeeping (backlog flip + the plan file with its dogfooded "Landed design —" amendment). The landed_design flag, its plan-file persistence, the update_plan plumbing, the finish gate, the skeleton text, and the constitution/docs clauses are correct and mutually consistent. One low docs-sync gap in a tool schema description.

### Verified correct

**Gate logic (src/tool/workflow/plan.rs:1772-1814)**
- Fires only for bug_fixing plans — the check sits inside the `plan.kind == PlanKind::BugFixing` block (line 1778) — and only when the flag is set (`if plan.landed_design`, line 1797).
- Marker check is `!plan.context.contains("Landed design")` (line 1798): case-sensitive, on the CONTEXT field only. The flag's own `## Landed design` section is a separate `PlanFile` field and can never satisfy it; a hand-written `## Landed design` header inside the context would be consumed by the parser as a section switch (not accumulated into context), so that misreading fails closed — the code comment at 1790-1796 documents exactly this.
- Position: after the regression_test presence check (1779-1789) and before the code-graph gate (1815+) — fail-fast ahead of the slow re-index work.
- Success note (1807-1813) joins into the finish output via `notes.join(" · ")` (line 2051) and the `notes` data field (line 2057).
- The blocking error names the exact remedy (update_plan append, "Landed design —" paragraph, the four content categories) and agent.md 'Documentation expectations'.

**Parse/serialize (src/workflow/plan_file.rs)**
- `landed_design: bool` with `#[serde(skip)]` (112-120) — off the frontend wire shape (one-way UI serialization; the on-disk path is the manual serialize/parse, so nothing is lost). `PlanFile::new` defaults false (191).
- Header match `"landed design" => Section::LandedDesign` (229) under the parser's standard lowercase normalization — same treatment as every other section. Body accepts "yes"|"true" (297-303); serialize emits "yes" (356-358). Roundtrip test `landed_design_flag_roundtrips` (830-865) covers true-roundtrip, default-false, and a hand-edited "true" body.

**update_plan plumbing (src/workflow/mod.rs:629-786)**
- The 7th param threads through all 24 call sites: 2 production (UpdatePlanTool passes `args.landed_design` at plan.rs:1033; FinishTool's regression_test recording correctly passes `None` at plan.rs:1705 — finish's param concerns the test name, and update_plan remains callable in Reviewing so there is no finish↔update_plan deadlock for the flag) + 22 test call sites in mod.rs (19 pre-existing updated, 3 new). No call site should pass a real value instead of None.
- `Some(v)` sets/un-sets `frame.plan.landed_design` and counts for the no-op guard (713-720): `Some(false)` un-sets a mistaken declaration; a landed_design-only call is not a no-op error; landed_design-only updates are allowed in Reviewing (asserted in the extended `update_plan_regression_test_allowed_in_reviewing`, mod.rs:2408).
- Skeleton-lock error text (672), no-op error text (776), and the doc comments (620-628) all name the field.

**Skeleton text (src/tool/workflow/plan.rs:60-70)**
- steps[3] carries the full feature-scale instruction; its marker strings match the gate exactly ("Landed design —" prose paragraph, landed_design=true recorded BEFORE completing the step, SPEC memory + BUG-record amendment at finish, agent.md 'Documentation expectations', the finish gate blocking clause). The skeleton test extension (2582-2587) pins both "landed_design" and "Landed design" in steps[3].text.

**Constitution ↔ machinery consistency**
- agent.md '## Documentation expectations' (49-62) is well-placed (after '## Code style', before '## File mutation policy') and matches the house style. Its marker ("Landed design") and section name ('Documentation expectations') are byte-identical across the gate error, the success note, the skeleton text, and the update_plan schema property — the instruction is actionable everywhere it appears.

**Docs sync**
- README.md:36 and PLAN.md:433-437 clauses are accurate (flag + amendment + gate + constitution pointer) and read coherently in their surrounding sentences.

**Multi-platform neutrality / file-tools-first**
- Rust-only change; no platform APIs, paths, or shell syntax anywhere in the diff; no shell-based file mutation (all edits went through the file tools).

**Tests**
- 4 new + 2 extended, each exercising the changed path: `finish_blocks_landed_design_without_context_amendment` fails without the gate (asserts the error text + state stays Reviewing); `finish_accepts_landed_design_with_amendment` asserts the SPEC/BUG reminder note in the output + Complete; `landed_design_flag_roundtrips` fails without the parse/serialize; `update_plan_sets_and_unsets_landed_design` fails without the engine field handling. The main agent's full `cargo test` (2303 + 16 passed, 0 failed, 0 warnings under deny(warnings)) is consistent with the code as read; not re-run by this reviewer (read-only).

### Findings

**LOW 1 — finish's schema description omits the new bug_fixing gate.** src/tool/workflow/plan.rs:1645-1647. The description enumerates the bug_fixing finish requirements ("also need the regression test name recorded (either via update_plan regression_test, or pass it directly here) and the symbol present in the code graph") but not the new conditional one: landed_design set → 'Landed design' context amendment required. An agent that hits the gate still receives a fully actionable blocking error, so nothing traps — but the finish description is the natural pre-flight read for what finish needs and is now stale relative to the machinery (the project's own docs-sync bar: a feature that ships with its docs not updated is an incomplete change). Same theme, optional touch-up: the update_plan tool description (plan.rs:927-929) names only regression_test as the bug-plan field ("record the verify step's test name via regression_test") — the landed_design property description (937) is complete, so that one is cosmetic. Suggested fix: one clause in the finish description, e.g. "…and, when landed_design was recorded, a 'Landed design' context amendment (agent.md 'Documentation expectations')".

### Documented residuals (not findings — accepted design, recorded in the plan context)

- Self-reported flag: an agent that never sets it skips the gate — same exposure as the constitution rule alone; the flag catches the honest-forgetful case (the realistic failure mode from 85368a1f).
- The `contains("Landed design")` substring check can in principle be satisfied by an unrelated context mention — unlikely, documented residual.
- The SPEC memory + BUG-record amendment are not gate-verifiable (memory writes are invisible to the plan gate) — carried by the constitution rule + the gate's success-note reminder, as designed.
