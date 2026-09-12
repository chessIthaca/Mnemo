# Review: README "Why Mnemo exists: spec-driven development" section

Branch: feat/readme-spec-driven (working tree)
Reviewed change: `git diff HEAD` — README.md (+9) and `.coding/plans/stack.json` (plan bookkeeping sidecar, expected).

Scope: the new `## Why Mnemo exists: spec-driven development` section (README.md lines 9–16), placed between the intro/license badge and `## Key ideas`, replacing the originally-planned numbered "## Spec-driven development" list. Exactly one version of the section exists (grep for `spec-driven|Spec-driven|specification` matches only lines 9/11/16 — all within the new section; no leftover draft text, no duplicate heading, no stray numbering).

## Findings

### Low

1. **"Every session starts in Planning" slightly overstates crash-resume behavior — README.md:13.**
   The bullet says "Every session starts in Planning: the agent explores the codebase…", but the README's own documented behavior is that sessions resume an in-flight plan mid-step: "step-granularity markers let a restarted session resume mid-plan" (line 23), "Crash-safe resume: plans, workflow state, and memory markers persist across restarts" (line 31), and "`complete_step` checks them off on disk" with resume at the first unchecked step (line 58). The source confirms this (`src/workflow/mod.rs` `load_from_resumes_executing`; `tests/workflow_integration.rs:226` `resume_on_restart` — "Session 2: load the plan from disk — should resume at step 1 (Executing)"). So a restarted session with an in-flight plan resumes in Executing, not Planning.
   Suggested fix: qualify the claim, e.g. "Every piece of work starts in Planning" or "Work starts in Planning: …" (or add a parenthetical noting that sessions resume an in-flight plan after a restart). This also strengthens, rather than weakens, the motivation framing.

## Verified accurate (no findings)

- **Spec → plan equivalence.** The section's "spec" is the README's "plan" (the `.coding/plans/` artifact created by `create_plan`); "the two of you converge on what will be built — goal, constraints, context, ordered steps" matches the create_plan tool description, and the executor-recipe claims ("explicit file paths, exact actions, self-contained steps — a step must not assume the model remembers an earlier step") match the `create_plan` step-body description in `src/tool/workflow/plan.rs` (self-contained recipes for a less-powerful model, "must NOT assume the model remembers an earlier step").
- **State-machine claims match "The enforced workflow"**: "nothing is written until the spec exists" — Planning is read-only, no write tools (README lines 20, 57); "steps are completed and checked off one at a time" — `complete_step` checks them off on disk, one step at a time (line 58); "nothing ships without a review" — Reviewing gate is unskippable for implementation plans and findings may not be deferred (lines 22, 59); research plans skip review only because they produce no source changes (line 60), so "nothing ships" still holds for code.
- **Per-stage model routing matches the system**: `[models.planning|executing|reviewing|complete]` state overrides exist in `config.toml` (`src/model_resolver.rs:16`; `src/config/general.rs:895–903`); the "strong for Planning / cheap-fast for Executing" pattern matches the documented sample overrides (`planning = o3`, `executing = deepseek-v4-flash`); "different models for planning vs. executing vs. reviewing" is already stated in README line 44, so the section is internally consistent. "Fresh model for Reviewing (independent check of the diff)" matches the read-only reviewer subagent (line 22 / README line 44).
- **On-disk plans**: `.coding/plans/` path matches README line 92 and the plan-file machinery (`src/workflow/plan_file.rs`, `stack.json` sidecar).
- **Style/format**: heading matches the `## ` sentence-case pattern of neighboring sections; bold-lead bullets match "Key ideas" style; inline code (`.coding/plans/`, `[models]`, `config.toml`) is used correctly; em-dashes and `→` arrows match the existing README conventions.
- **Line endings**: LF throughout — the diff shows no `^M`/CR artifacts on new or context lines.
- **No duplicates/leftovers**: single section only; the `.coding/plans/stack.json` change is expected plan bookkeeping (noise from the active plan, not part of this change).
- **Motivation framing**: the "Why Mnemo exists" heading plus the conviction-driven intro paragraph deliver the stated intent clearly; the 4 bullets justify the architecture (spec loop, executor-level detail, per-stage models, state machine) as enforcement of that motivation.

## Verdict

One low-severity wording inaccuracy (line 13, "Every session starts in Planning" vs. documented crash-resume). Everything else is accurate, consistent, well-styled, and free of duplicates or typos. The change is safe to merge once the low finding is addressed.
