## Verdict: PASS

All five round-1 fixes landed correctly in commit 737c866 (HEAD on wt/agenticcoding) and introduced no new issues. Each verified against the commit diff (`git show 737c866`), the current file state, and git history.

## Per-finding verification

### F1 — growth attribution corrected to 077d375 ✓

All five new dated comments in src/agent/factory.rs now credit the plan resumability-gate commit 077d375, with the correct per-filter tool attribution:

- **Planning** (factory.rs:1704-1713): 16_000 → 16_700, "+626 of schema growth since the 15_751 baseline (2026-09-10), from the plan resumability-gate commit 077d375 (create_plan's schema documents the gate; create_plan rides Planning)" — and it includes the required explanation verbatim in substance: "The growth crosses the workspace-scale ceiling while the standalone scale still passes (~15_946 < 16_000), which is why plain `cargo test` stayed green while `cargo test --workspace` failed."
- **Executing** (1752-1758): 28_000 → 28_800, 077d375 "create_plan + update_plan schema docs; both ride Executing".
- **ExecutingResearch** (1783-1789): 22_800 → 23_600, 077d375 "create_plan's schema docs; create_plan rides the research filters".
- **Reviewing** (1815-1821): 24_500 → 25_200, 077d375 "update_plan's schema docs; update_plan rides Reviewing while create_plan does not — matching the smaller delta".
- **Complete** (1831-1836): 16_000 → 16_700, 077d375 "create_plan's schema docs; create_plan rides Complete".

Cross-checks: git log confirms 077d375 is "feat: plan resumability gate — reject plans too thin to survive recompile+restart (b58439f4)" and precedes 737c866. No eb202c6 reference remains in any new comment — the surviving eb202c6-era text is the historical 2026-09-10 raise comments (be16ea36/memory_amend + file_edit batch growth), whose baselines legitimately include eb202c6, exactly the archaeology round 1 established. Arithmetic is internally consistent: 15_751+626=16_377; 27_696+757=28_453; 22_484+758=23_242; 24_164+615=24_779; Complete=Planning=16_377; and the standalone figure ~15_946 = 16_377 − 431 (the load_tools workspace-unification delta documented in the adjacent 2026-09-08 comments). Every measured value ≤ its ceiling with 323-421 chars headroom, matching the dated-comment convention's modest-headroom range.

### F2 — PlanFrozen budget-guard entry ✓

factory.rs:1759-1765: `(ToolFilter::PlanFrozen, 29_900)` sits directly after the Executing entry, with the dated comment "Measured at 29_565 chars (25 tools) on the 2027-01-10 pass" plus the rationale ("the production surface for every implementation/bug_fixing plan — the largest array the app sends (Executing ∪ finish) — so it needs its own ceiling, not coverage-by-transitivity"). Consistency: 29_565 − 28_453 (Executing) = 1_112 chars for finish's schema; 25 tools = Executing's 24 + finish — and the L4 backlog item itself says "25 tool schemas ride in the stable head". Headroom 335, within the convention's 62-421 range. The guard loop measures and asserts every entry, this one included.

### F3 — README precision ✓

README.md:32 now reads exactly: "…in Reviewing only the closing tools are callable (during a plan the advertised array is frozen for prefix-cache stability; out-of-state calls are still rejected at dispatch); the agent cannot skip ahead." — the exact expected wording, replacing the imprecise "in Reviewing only the closing tools".

### F4 — commit hygiene ✓

`git show --stat 737c866` lists exactly the 10 declared files — src/tool/mod.rs, src/workflow/mod.rs, src/agent/turn.rs, src/agent/tests.rs, src/agent/factory.rs, PLAN.md, README.md, .coding/plans/8911500b.md, the SPEC knowledge file, and the round-1 review report — and nothing else: no .coding/backlog.jsonl, neither stray status-bar file. The drift remains out of the commit and uncommitted in the tree (git status: backlog.jsonl modified, the two status-bar files untracked) — the "consciously exclude" resolution round 1 prescribed.

### F5 — underscore param ✓

src/agent/tests.rs:2402 — `tools: &[ToolSchema],` (no leading underscore) at the signature; :2420 — `serde_json::to_string(tools)` at the capture site. Both renamed; no `_tools` reference remains in the impl.

## Sanity checks on the commit as a whole

- **No `#[allow(...)]`** in any of the 10 committed files (repo-wide search hits only vendor/tao and two pre-existing doc-comment *mentions* elsewhere in src/).
- **No dead code**: `build_with_tool_sets` is called by both `build` and `new_with_tool_sets`; `new_with_tool_sets` by both new tests; `captured_tool_sets` is written in `complete()` and read by both tests. The `_tool_sets` binding in `build`'s destructure is genuinely unused — the correct underscore use (unlike F5's used-but-underscored case).
- **No new-warning risk**: the diff adds no `use` statements; all new test code uses symbols already imported and exercised by adjacent tests. Under `#![deny(warnings)]` nothing in the diff can warn.
- **Commit message accurate**: "Budget guard: PlanFrozen entry added (measured 29,565 chars); five ceilings raised for the 077d375 resumability-gate schema growth that crossed workspace-scale budgets (dated comments)" — matches the code exactly.
- The committed round-1 report is byte-identical to the working-tree copy.

## Notes

- The "cargo test --workspace green (2,262+ tests, 0 failed)" claim is the parent's; as a read-only reviewer I could not run tests. Static verification found nothing that would fail: all budget ceilings ≥ measured values, the renamed param compiles, no unused bindings or imports.
- Post-commit, the tree carries uncommitted bookkeeping for this plan (a SPEC amendment paragraph documenting the five fixes — every claim in it checks out against the code — and the plan-file step-5 checkbox flip), plus the pre-existing backlog.jsonl drift and the two stray status-bar files from other sessions. None of that is in 737c866, which is what F4 required; the bookkeeping presumably lands with the round-2 commit.
