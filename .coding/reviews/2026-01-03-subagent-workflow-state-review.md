## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes on wt/agenticcoding for plan 3fb064c4 "Parented subagents: dedicated Subagent workflow state". The restructure is correct and well-tested: the Subagent ⇒ allow-list invariant holds on every production path, spawn ordering is right, the model-resolver drop is deliberate and pinned, the mirror keeps current_plan + the UI staircase working, and the flaky-test fix is sound. Three low findings: two stale-comment clusters (doc sync) and one invariant-audit disclosure (unvalidated `skill_start` `target_state` can land a main-agent workflow in Subagent state without an allow-list — fail-loud panic, pre-existing class). Details below.

## Scope

Full `git diff HEAD` + untracked files on wt/agenticcoding: the Subagent restructure (src/workflow/mod.rs, src/tool/mod.rs, src-tauri/src/ipc/spawn.rs, src/agent/factory.rs, src/agent/prompt.rs, src/model_resolver.rs, src/runtime/agent.rs, src/agent/loop_impl.rs, src/tool/workflow/plan.rs, src-tauri/src/ipc/agent.rs, frontend types.ts/StatusBar.tsx/PlanProgress.tsx/ModelsSection.tsx, README.md, PLAN.md), the flaky-test fix (src/provider/openai.rs), and the auto-generated `.coding/knowledge/` records + plan file.

## Verified correct

**The core invariant — Subagent state ⇒ allow-list set (the audit the task asked for):**
- `enter_subagent_state()` has exactly ONE production caller: `spawn_agent_shared` (src-tauri/src/ipc/spawn.rs:155), gated on `parent_id.is_some()`. Every other caller is a test. No other path produces a Subagent-state workflow in production.
- `compute_subagent_allowlist` returns `Some` for EVERY parented spawn: reviewer role → `Some(out)`; unknown role → `Some(vec![])` (deny-all); no role → `Some(parent_allowed)`; parent loop missing from the map → `Some(vec![])` (fail-closed). `None` only when `parent_id` is `None` — and that path never stamps. The invariant holds.
- Spawn ordering is correct: forced model (137-139) → parented block: `set_plan_mutations_allowed(false)` ×2, `enter_subagent_state()` (155), `set_is_subagent(true)` (159) → allow-list set (172-185) → registration (191-203) → `agent_loops` insert (230) → first prompt (234-246). No turn runs before the allow-list is set. Between stamp and allow-list, nothing consults the child's `allowed_tools` — `compute_subagent_allowlist` locks the PARENT's workflow, and a Subagent-state parent itself carries an allow-list, so nested (grandchild) spawns are safe too.
- `allowed_tools()`: the allow-list branch returns first (374-379); the explicit `(WorkflowState::Subagent, _)` panic arm (393-395) fires only on invariant violation, mirroring the Skill case. Both new tests (`subagent_state_with_allowlist_uses_allowlist_filter`, `subagent_state_without_allowlist_fails_loudly`) pin the two sides.
- No production path undoes the stamp: `set_tool_allowlist(None)` appears only in tests; `load_latest`'s only production caller is `build_inner` (factory.rs:619), which runs BEFORE the stamp; `enter_subagent_state` does not persist and `StackSidecar` carries no state field, so nothing leaks into the shared main plans dir.
- Skill path: `skill_start` validates `is_available_in(skill, current)` (skill.rs:133) — skills declare `available_in` (Planning/Complete), so a Subagent-state workflow cannot start a skill unless one explicitly opts in; `abandon_skill` restores `pre_skill_state` (correctly Subagent if entered). See Finding 3 for the `target_state` caveat.

**Model resolver:** `WorkflowState::Subagent => None` is correct and deliberate; `[models.subagent]` applies via the `is_subagent` arm, else default. `subagent_state_never_resolves_a_lifecycle_model` pins both. Reviewer model routing is unaffected (the reviewer pin is applied at spawn time via `set_forced_model`, before resolution).

**Auto-continue:** EXCEPTION 2 comment (runtime/agent.rs:234-248) properly updated — historical mechanism, "Since 2026-01-03 …" note, belt-and-braces rationale. `workflow_expects_progress` = Executing|Reviewing only → Subagent false; the `is_subagent` guard is retained.

**Prompt:** `STATE_SUBAGENT` + the Subagent arm (prompt.rs:718-744) render the read-only PARENT PLAN mirror + GOAL + PROGRESS + role guidance. The removed Planning/Executing sub-agent notes are dead in production: the only production `set_plan_mutations_allowed(false)` sites are spawn.rs:143/146, always followed by the stamp (factory.rs:2033 is a test).

**current_plan (round-3 warning #1):** gates on plan presence, not state; `current_plan_on_subagent_state_returns_mirrored_plan` pins the mirror + "state: Subagent" echo.

**Frontend (round-3 warning #3):** serde `rename_all = "lowercase"` → "subagent" matches the types.ts union; StatusBar label arm, PlanProgress slate badge, ModelsSection hints all updated; every other state consumer found (StatusBar 533-544, PlanProgress 211-212, useAgentEvents 418) has a fallback — no exhaustive switch breaks.

**Flaky-test fix (openai.rs):** bounded-range assert `(1234..1234 + 10_000)` for the sliver-augmented `prep_ms`, exact for `compact_ms` (no sliver); comment documents the root cause and observed values; BUG record exists. Correct and robust.

**Multi-platform neutrality:** all changes are platform-neutral Rust core + TypeScript; no Windows-only APIs, paths, or shell syntax anywhere in the diff.

**Docs:** README.md and PLAN.md hunks accurately describe the new model routing and the plan-ownership enforcement list; the knowledge records and plan file are present and accurate.

## Findings

### 1. LOW (doc sync) — stale pre-change comments in `src/workflow/mod.rs`

Three comment sites still describe sub-agents as sharing/deriving the main agent's lifecycle state — exactly the "now-stale references" the constitution's doc-sync check targets:
- `tool_allowlist` field doc (~lines 156-157): "even though the sub-agent shares the main agent's plan (and thus its `Reviewing` state)" — post-change the sub-agent is in Subagent state, never Reviewing.
- `allowed_tools` doc (~lines 354-356): "even though it shares the main agent's plan (and thus its `Reviewing` state)" — same staleness.
- `allowed_tools` inline note (~lines 365-366): "Sub-agents get the normal state-derived filter and simply cannot mutate plans." — factually wrong post-change: sub-agents get the spawn-time allow-list filter (`ToolFilter::Skill`/`Reviewer` seeded from the parent's set), never the state-derived filter. The surrounding rationale (don't collapse to Planning) is historical.

Suggested fix: reword to the current model — the allow-list is set for every parented spawn and overrides the (now Subagent) state; plan-mutation denial is enforced separately.

### 2. LOW (doc sync) — stale present-tense test comments

- `src/runtime/agent.rs:5688-5691` (`auto_resume_does_not_fire_for_subagents`): "tool-spawned subagents share the main plans dir and load_latest() derives their workflow state from the main plan on disk (…)" — describes the pre-change mechanism as current behavior. The EXCEPTION 2 comment received the "Since 2026-01-03 …" update; this regression-test comment (the test that guards that very path) deserves the same treatment, e.g. noting the state is now Subagent and the test exercises the belt-and-braces `is_subagent` guard with a hand-built Executing workflow.
- `src/workflow/mod.rs:1690-1691` (test comment): "even though it inherits the main agent's Reviewing state" — stale for the same reason.

### 3. LOW (invariant audit) — unvalidated `skill_start` `target_state` can produce a Subagent-state workflow without an allow-list

`SkillStartTool::execute` (src/tool/workflow/skill.rs:144-146) takes `args.target_state` (and the TOML spec's `target_state`) with no validation beyond serde parsing. The tool schema advertises `enum: ["planning", "executing", "complete"]`, but schemas are hints — serde accepts any `WorkflowState` string. Post-change, `"subagent"` parses, so `skill_start {skill, target_state: "subagent"}` + `skill_end` lands a MAIN-agent workflow in Subagent state with NO allow-list → `allowed_tools()` panics on the next turn's schema build, killing the agent loop (recoverable only by closing the tab).

Assessment: LOW — this is the same fail-loud class as the pre-existing `"skill"` string (which panics via the `from_state(...).expect(...)` arm), so the change widens an existing hole rather than creating a new vulnerability class, and the panic is the designed invariant response (no silent escalation). But the task explicitly asked for every path that can produce a Subagent-state workflow, and this one bypasses the spawn path. Related, one step removed: a skill TOML listing `available_in = ["subagent"]` would let a subagent start a skill and `skill_end` into a lifecycle state (allow-list stays set, so no panic — display/model-tier drift only).

Suggested fix: in `SkillStartTool::execute`, validate the resolved `target_state` is one of Planning/Executing/Complete and return a `ToolResult::error` otherwise (this also closes the pre-existing `"skill"` variant); optionally validate the spec's `target_state` at registry load.

## Test results

Acknowledged as reported and consistent with the code read: root `cargo test` 1954 passed / 0 failed; src-tauri `cargo test` 186+4 passed / 0 failed; frontend `tsc --noEmit` clean + vitest 789 passed (55 files). The new tests meaningfully pin the changed behavior (invariant both sides, stamp sequence, mirror, resolver, prompt section).
