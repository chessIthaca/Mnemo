## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification for plan 3fb064c4 "Parented subagents: dedicated Subagent workflow state" (commits 5f395f1 + 590afc2 on wt/agenticcoding, HEAD = 590afc2 confirmed). All three round-1 LOW findings are resolved in the committed code — the five comment rewrites are in place and (with one one-word exception, below) factually accurate, and the `skill_start` `target_state` validation is correct, covers both the args override and the spec TOML, and blocks no legitimate path. One new LOW: the rewritten `auto_resume_does_not_fire_for_subagents` comment says the test "hand-builds an Executing-state subagent loop" — the test actually builds a **Reviewing**-state workflow. Comment-only, one-word fix.

## Scope and method

- `git diff HEAD`: one uncommitted change — a single-line `.coding/backlog.jsonl` status update for item c5ded15d (sanctioned closing-sequence bookkeeping; not part of the code change set). HEAD = 590afc2; the change set is 5f395f1 (flaky-test fix) + 590afc2 (the restructure + all three round-1 fixes, per the commit message and verified hunk-by-hunk below).
- Method: re-read the round-1 report; verified each fix in `git show 590afc2` AND in the current tree (src/workflow/mod.rs, src/runtime/agent.rs, src/tool/workflow/skill.rs); cross-checked every rewritten comment's claims against the code it describes (`compute_subagent_allowlist` in src-tauri/src/ipc/spawn.rs, `workflow_expects_progress` in src/agent/loop_impl.rs, the test bodies); audited all `start_skill` callers via the code graph; checked the shipped skill TOML and the embedded `SHIPPED_SKILLS` copy.

## Round-1 finding verification

### Finding 1 (LOW, doc sync — stale comments in src/workflow/mod.rs): RESOLVED

All three sites rewritten in 590afc2 and verified in the current tree; each cross-checked for accuracy:

- `tool_allowlist` field doc (:150-166): now "…the constraint holds even though the sub-agent shares the main agent's plan, whose state it never inherits (a parented sub-agent is stamped `WorkflowState::Subagent`, a role state, never a lifecycle phase)…". Accurate.
- `allowed_tools` doc (:345-359): now "…its workflow state is `WorkflowState::Subagent` (a role state stamped by the spawn path), and the allow-list — not any lifecycle state — defines its surface." Accurate.
- `allowed_tools` inline note (:368-370): now "Sub-agents carry a spawn-time allow-list (the intersection of the parent's current permissions) and simply cannot mutate plans." Accurate — verified against `compute_subagent_allowlist` (spawn.rs:335-426): reviewer role = `REVIEWER_BASE_TOOLS` ∩ parent-allowed (+ the two always-safe role tools), no role = exactly the parent's current allowed set, unknown role = deny-all, missing parent loop = deny-all (fail-closed). The allow-list is in every case a subset of the parent's current permissions; the parenthetical is a fair summary of that model.

### Finding 2 (LOW, doc sync — stale present-tense test comments): RESOLVED (one wording defect → R2-1 below)

- `tool_allowlist_overrides_state_derived_filter` (workflow/mod.rs:1690-1697): now "(Pre-2026-01-03 the reviewer inherited the main agent's Reviewing state via the shared plan; since then its state is Subagent — either way the allow-list, not the state, defines the surface.)" Past-tense history + current model; matches the test (which hand-builds a Reviewing-state workflow and asserts the allow-list wins). Accurate.
- `auto_resume_does_not_fire_for_subagents` (runtime/agent.rs:5687-5705): the stale present-tense mechanism description is gone — now "historically, tool-spawned subagents shared the main plans dir…" plus the "Since 2026-01-03 the spawn path stamps WorkflowState::Subagent … belt-and-braces" close. The restructure framing is exactly right; one word in the closing sentence is wrong — see Finding R2-1.

### Finding 3 (LOW, invariant audit — unvalidated skill_start target_state): RESOLVED

- Validation present at skill.rs:146-161 (current tree): after `target_state = args.target_state.unwrap_or(spec.target_state)` and before `start_skill`, a `matches!(Planning | Executing | Complete)` gate returns `ToolResult::error("invalid target_state '{target_state}': must be one of Planning, Executing, Complete")`. Because it validates the RESOLVED value, it covers both the args override and the spec's TOML — as claimed.
- Regression test `start_skill_rejects_non_lifecycle_target_state` (skill.rs:382-406): asserts both "subagent" and "skill" are rejected with the "invalid target_state" error AND that the workflow stays in Planning with no active skill (no partial entry). The test genuinely exercises the new path: `WorkflowState` serde is `rename_all = "lowercase"` (workflow/mod.rs:25), so both strings parse — had serde rejected them the output would be "invalid arguments: …", failing the `contains("invalid target_state")` assert. Green tests therefore prove the rejection comes from the validation. The pre-existing "skill" variant is closed by the same gate.
- No legitimate path blocked:
  - The only skill file, `.coding/skills/merge_to_main.toml`, has `target_state = "planning"`, `available_in = ["complete", "planning"]` — passes. It is also the only `SHIPPED_SKILLS` entry (embedded via `include_str!` from that same file, skill/mod.rs:136-139), so no shipped/built-in skill is affected.
  - The schema enum hint `["planning", "executing", "complete"]` is unchanged (skill.rs:95) — the validation now enforces exactly what the schema always advertised.
  - "reviewing" is also rejected, but it was never in the schema enum, no skill or test targets it, and Reviewing is entered via `complete_step` — not a legitimate `skill_end` target.
  - Existing green tests that pass target_state ("complete" in `start_skill_overrides_prompt_and_target`; Planning in the test registry) all pass the gate.
- Caller audit (code graph on `start_skill`): exactly two production callers — `SkillStartTool::execute` (now validated) and the IPC command `enter_skill` (src-tauri/src/ipc/agent.rs:771). All other callers are tests. See "Disclosed residual" for `enter_skill`.

## Findings

### R2-1. LOW (doc sync) — rewritten test comment misstates the workflow state: "Executing-state" should be "Reviewing-state"

`src/runtime/agent.rs:5703-5704` (the `auto_resume_does_not_fire_for_subagents` comment rewritten in 590afc2): "this test hand-builds an Executing-state subagent loop to exercise the is_subagent guard as pure belt-and-braces". The test actually hand-builds a **Reviewing**-state workflow: `create_plan_with_kind(…, PlanKind::Implementation, …)` → Executing, then `complete_step(0)` completes the single-step ROOT plan → Implementation + complete + unreviewed → **Reviewing** (workflow/mod.rs complete_step root-plan arm). The test's own older inline comment a few lines below (agent.rs:5722-5723) says exactly this ("complete + unreviewed → Reviewing"), and Reviewing — the closing-sequence reviewer's derived state in the observed-live incident — is the progress-expecting state that makes the `is_subagent` guard meaningful here. The new sentence contradicts the code it annotates. (The slip originated in round-1's suggested wording — "a hand-built Executing workflow" — which the fix copied verbatim.) Comment-only, no behavioral impact: the test itself is correct and green. Fix: "Executing-state" → "Reviewing-state" (or "lifecycle-state (Reviewing)").

## No new issues (beyond R2-1)

- Comment-accuracy audit of every rewritten site: the three workflow/mod.rs sites (finding 1), the EXCEPTION 2 comment (runtime/agent.rs:233-248 — "historically … Since 2026-01-03 … belt-and-braces", consistent with `workflow_expects_progress` = Executing|Reviewing only), the `tool_allowlist_overrides_state_derived_filter` comment, and the new Subagent enum doc / `enter_subagent_state` doc / panic-arm comment all check out against the code they describe. R2-1 is the sole defect found.
- The skill.rs validation is the only behavior change in the fixes: it converts a previously-panicking-later (or never-legitimate) input into a clean tool error. No test, TOML, or production path regresses — confirmed by the caller audit and the test counts below.
- The `.expect` message change ("from_state is Some for all lifecycle states") is cosmetic and correct.
- Multi-platform neutrality: the fixes touch only platform-neutral Rust in `src/` — no Windows-only APIs, paths, or shell syntax.
- Docs: the fixes themselves require no documentation updates (comment-only plus one validation whose contract the tool schema already advertised); the commit message documents the validation.

## Disclosed residual (pre-existing, NOT a finding — round-1's explicitly optional suggestion)

`enter_skill` (the UI-initiated path, src-tauri/src/ipc/agent.rs:771-842) passes `spec.target_state` to `start_skill` at :808 without the new validation. A hand-edited skill TOML with `target_state = "subagent"` (or the pre-existing `"skill"`) could therefore still land a main-agent workflow in a bad state via the UI skill button. Not a finding because: (a) pre-existing — unchanged by this fix, the same fail-loud class round-1 itself assessed as LOW; (b) not LLM-reachable — the registry loads at startup and the path requires the user to click the skill button (the agent-driven `skill_start` tool path is fully validated, covering both the args override and the TOML); (c) it is exactly the "optionally validate the spec's target_state at registry load" hardening round-1 marked optional, which was not taken; (d) no shipped skill is affected (merge_to_main targets "planning"). Recommend a backlog item (one `matches!` gate in `SkillRegistry::load_dir` or in `enter_skill`) to close the invariant end-to-end.

## Test results

Acknowledged as reported and consistent with the code read: root `cargo test` 1955 passed / 0 failed — exactly +1 vs round-1's 1954, accounting for the new regression test; src-tauri `cargo test` 186+4 passed / 0 failed — unchanged, correct since the fixes added no src-tauri tests; frontend unchanged since its green run (tsc clean + vitest 789 passed) — correct since the fixes touched no frontend files. The regression test exercises the changed path and pins both rejected variants plus the no-partial-state guarantee.
