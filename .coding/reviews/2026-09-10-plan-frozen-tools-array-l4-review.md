## Verdict: FINDINGS (0 high, 5 low)

The plan-frozen advertisement is correctly implemented and safely separated from enforcement — every hard rule holds (verified by direct reading of both filter arms, dispatch, and the workflow transitions, plus git archaeology on the ceiling raises). The 5 low findings: wrong growth-attribution in the new ceiling comments (F1), PlanFrozen missing from the budget test (F2), a README precision gap (F3), unrelated uncommitted drift in the tree (F4), and an underscore-param style nit (F5). None block landing; F1/F2 should be fixed before commit.


## What was verified correct

**The freeze itself (src/tool/mod.rs, src/workflow/mod.rs)**

- `ToolFilter::PlanFrozen`'s `allows()` arm is exactly `Executing ∪ {finish}`, verified category-by-category and name-by-name against the adjacent Executing arm: Agent/Memory/Browser are `true` in both; the Workflow name list is identical plus `finish`. The unit test `plan_frozen_differs_from_executing_only_by_finish` pins this over the registered surface.
- The hard-rule check at the top of `allows()` (write_review_report denied unless `ToolFilter::Reviewer`) runs before the match, so PlanFrozen can never admit it — `plan_frozen_never_admits_write_review_report` pins it.
- `PlanFrozen` is absent from `from_state()` (tool/mod.rs:281-285), so no enforcement path can ever resolve to it.
- `schema_filter()` keying is right: `tool_allowlist` wins first (reviewer/skill sub-agents keep their constructor-granted surface — `schema_filter_allowlist_wins`); Executing|Reviewing with an active plan → `active_plan_kind()` (top-of-stack — the same key `allowed_tools()` already uses for its research narrowing, workflow/mod.rs:419-423) → Research ? ExecutingResearch : PlanFrozen; otherwise per-state. finish/abandon pop the plan → per-state filter; a sub-plan push re-keys on the new top-of-stack plan (the pre-existing `research_plan_selects_the_research_filter` test documents the same semantics). Research plans never enter Reviewing (completing all steps pops to Complete — `schema_filter_research_plan_uses_executing_research`), and even if one did, ExecutingResearch ⊂ Reviewing enforcement is the safe direction.

**Advertisement/enforcement separation (src/agent/dispatch.rs, src/agent/turn.rs)**

- Dispatch re-checks `wf.allowed_tools()` per call (dispatch.rs:101) before approval/execution — verified; `mcp_reveal_allowed` uses the same enforcement filter. No path lets a frozen-advertised tool execute out of state.
- Strong property verified: **PlanFrozen ⊇ the enforcement surface of both frozen states.** Every Reviewing workflow tool (finish, abandon_plan, ask_user, current_plan, update_plan, backlog_add/status/list) is present in the PlanFrozen arm, and Agent/Memory/Browser are `true` in both. The freeze therefore never hides a callable tool — it only ever shows extra ones (finish during Executing; complete_step/create_plan during Reviewing), which dispatch rejects. One wasted turn at most, exactly as documented in PLAN.md.
- turn.rs feeds the same `tool_filter` to the request schemas, the hidden-groups index, and token accounting — all frozen consistently; the sub-agent plan-mutation strip (turn.rs:306-315) still applies after the filter, and dispatch's main-agent-only finish gate is unchanged.
- `schema_filter`'s only production consumer is the turn.rs per-request schema build (no src-tauri consumer exists).

**Tests**

- The byte-stability test drives a real Executing→Reviewing transition (complete_step ×2, state asserted) and asserts the tools array, the stable head, and the CONTEXT_FOOTER tail byte-identical. The head assertion is meaningful: the hidden-groups index embedded in the head derives from the same filter, so it would have drifted under the old `allowed_tools()` build.
- The cross-plan test asserts research (no file_write, has file_read) vs implementation (has file_write) arrays differ — the re-keying contract.
- The four workflow tests assert exactly what their names claim; CapturingProvider captures the serialized array per `complete()` call, so the assertion target is the real request payload.

**Hygiene**

- prompt.rs untouched (CONTEXT_FOOTER discipline intact); no `#[allow(...)]` anywhere in the diff; no Windows-only APIs/paths; no shell-based file mutation; the PLAN.md paragraph is accurate (re-keying, dispatch re-check, advertisement/enforcement split all match the code).
- Ceiling arithmetic: every measured value ≤ its new ceiling with 323-421 chars headroom (16,377≤16,700; 28,453≤28,800; 23,242≤23,600; 24,779≤25,200; 16,377≤16,700), consistent with the dated-comment convention's "modest headroom" (prior raises ranged 62-421). The raises fix a real base-tree failure that I could independently confirm from git history — see F1 for the corrected attribution.


## Findings

### F1 (low) — src/agent/factory.rs: the five new ceiling comments mis-attribute the growth

The new dated comments credit "the file-tool reliability commit eb202c6 (the most recent schema-touching change)" / "file_read's schema, which Planning carries, documents the EOL-agnostic matching surface". Git history contradicts this on three counts:

1. **eb202c6 is already measured into the baselines the comments grow from.** eb202c6 (2026-09-10 05:13) is the be16ea36 landing commit, and the 2026-09-10 raises were FOR its changes — the recorded baselines include it exactly (Planning: 15,241 workspace + 510 memory_amend = 15,751; Executing: 25,450 + 510 + ~1,736 file_edit batch/append = 27,696). Growth "riding eb202c6" on top of a baseline that already contains eb202c6 is impossible.
2. **eb202c6 never touched file_read.rs** (its stat lists file_edit/file_write/file_append/memory only), and the EOL-agnostic matching docs live in file_edit's schema — which Planning doesn't even carry.
3. **eb202c6 is not the most recent schema-touching commit.** 077d375 (plan resumability gate, 2026-09-10 06:04 — 51 minutes after eb202c6) is: +771 lines in src/tool/workflow/plan.rs, commit message "docs: tool schemas" (create_plan/update_plan resumability-gate documentation). That is the actual source of the +626/+757/+758/+615 growth: create_plan rides Planning/Executing/ExecutingResearch/Complete; update_plan additionally rides Reviewing — which cleanly explains Reviewing's +615 without create_plan.

The "workspace-unification drift" framing is also off: the workspace unification (+431 load_tools) was already in the baselines (the 2026-09-08 pass established workspace-scale figures). The real story: 077d375's schema growth crosses the workspace-scale ceilings while the standalone scale still passes (Planning standalone ≈ 15,946 < 16,000) — which is why 077d375's author saw green with plain `cargo test` while `cargo test --workspace` fails. The git-stash base-tree confirmation is consistent with this. The measured values and ceilings themselves check out; only the attribution narrative is wrong, and it defeats the dated-comment convention's purpose (auditable raises). **Fix:** reword the five comments to credit 077d375 (create_plan/update_plan resumability-gate schema docs) as the growth source.

### F2 (low) — src/agent/factory.rs: PlanFrozen missing from tools_array_stays_within_context_budget

The frozen array is the production surface for every implementation/bug_fixing plan — the entire point of this change — and the largest array the app sends (Executing ∪ finish ≈ 28,453 + finish's schema, likely above the 28,800 Executing ceiling). Every tool in it is transitively pinned by other entries (Executing covers all but finish; Reviewing covers finish), so bloat cannot fully escape detection, but the per-array budget guard doesn't cover the one array the L4 work exists for. **Fix:** add a `(ToolFilter::PlanFrozen, N)` entry with a measured baseline (expect ≈ Executing + finish's schema; it needs its own ceiling, likely ~29,000+).

### F3 (low) — README.md:32: "in Reviewing only the closing tools" is now imprecise

Enforcement is unchanged (dispatch rejects complete_step/create_plan in Reviewing — "the agent cannot skip ahead" remains true), but during an active plan the Reviewing request now advertises complete_step/create_plan (the frozen surface); a user inspecting the Trace tab will see non-closing tools in a Reviewing request. **Fix:** a parenthetical, e.g. "…in Reviewing only the closing tools are callable (during a plan the advertised array is frozen for prefix-cache stability; out-of-state calls are still rejected at dispatch)".

### F4 (low) — unrelated uncommitted drift in the tree; keep it out of this plan's commit

The uncommitted diff includes `.coding/backlog.jsonl` gaining `"deleted_at":1789039782` (≈2026-09-10) on item 65d79539 ("Debounce trace mirror file rewrites", L6 from the 2026-09-09 perf review) — pre-existing drift from an earlier session (it predates this session by ~4 months), not in the declared change set, and the work never landed in src/provider/trace.rs (no debounce commit there; the only debounce in src/ is the codegraph watcher). Also untracked: `.coding/reviews/2026-09-10-status-bar-tok-sec-rolling-window-review-round2.md` (another session's leftover). **Fix:** make the commit deliberate — include the declared files plus `.coding/plans/8911500b.md`, the SPEC knowledge file, and this review report; consciously exclude (or separately commit with justification) the backlog deletion and the stray review file.

### F5 (low) — src/agent/tests.rs:2402/2420: `_tools` is now used but keeps the underscore prefix

`CapturingProvider::complete`'s parameter was `_tools` when unused; the change now serializes it (`serde_json::to_string(_tools)`) without renaming. Legal and warning-free, but the underscore prefix conventionally marks an intentionally-unused binding (the trait's parameter is already `tools`). **Fix:** rename the impl's parameter to `tools`.

## Notes

- The "cargo test --workspace green (2262+ tests, 0 failed)" claim is the parent's; I could not run tests (read-only reviewer). Static verification found nothing that would fail.
- The tool/mod.rs test registry and make_registry don't register ask_user/current_plan, so `plan_frozen_differs_from_executing_only_by_finish` covers the registered surface only — consistent with the module's existing convention, and the two arms were verified identical-except-finish by direct reading.
- The content index is stale for the freshly edited files (a `schema_filter` search surfaces only the doc-comment mention in tool/mod.rs:231); all schema_filter conclusions above come from direct file reads.
