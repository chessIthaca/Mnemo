## Verdict: PASS

Round-2 delta re-review of plan 32f8da56 (backlog 834ec126) on wt/mnemo, base b2ae486 (= HEAD; the entire delta is the uncommitted working tree: 11 modified files + 3 untracked bookkeeping files). All four round-1 fixes verified present and sound.

## What I read (empty-diff rule)

The full `git diff` was archived and `expand_result` is not in my read-only surface, so I verified the delta by reading the changed hunks directly at their sites: the Skill arm + its comment + the `ToolFilter::Skill` variant doc + `skill_state_grants_the_full_read_surface` (src/tool/mod.rs :861-884, :475-481, :2442-2503), `skills_get_the_whole_read_surface_at_the_registry_level` (src/agent/factory.rs :2952-3045) cross-checked against `build_registry`, `register_codegraph_tools` (:1458-1477), the `ListModelsTool` conditional (:1130-1136), the constructor (:310-384) and `make_factory` (:1610-1642), the sibling `write_review_report_visible_only_under_reviewer_filter` (:2712-2774, Skill(vec!["write_review_report"]) denied at :2756), `SKILL_FILE_HEADER` + the `skill_create` schema (skill.rs :301-317, :437), `SkillSpec.tools` doc (skill/mod.rs :49-52), the MCP naming path (mcp/tool.rs :36/:54/:121/:552) + the `LoadToolsTool` reveal path, PLAN.md :303-310, docs/FEATURES.md :20, all four repo-skill TOMLs, the BUG knowledge record, and the two backlog lines.

## Fix verification

1. **HIGH — trusted MCP tools auto-granted: FIXED and airtight.** The Skill arm now reads `ToolCategory::Agent if safety == SafetyLevel::AutoRun && !name.starts_with("mcp__") => { true }` — the literal-prefix idiom is *exactly* the Planning/Complete arms' (they use the same literal, so this mirrors the base arms even more faithfully than the constant would). Every MCP tool name is minted by `mcp_tool_name` = `format!("{MCP_TOOL_PREFIX}{server}__{tool}")` with `MCP_TOOL_PREFIX = "mcp__"` (mcp/tool.rs :36/:54), prompt tools included (:121), and pinned by a unit test (:552). The `load_tools` reveal path materializes adapters under those same names and dispatch routes through `allows` with the real name, so a revealed trusted tool is excluded with no alternative naming route; the only way an `mcp__` tool runs inside a skill is an explicit reviewed allow-list entry (pre-fix behavior, intended). The `load_tools` "widens nothing" comment (:909-916) is true again as-is.
2. **LOW — tautological audit: FIXED and meaningful.** The test sorts `reads` and asserts exact equality with the 7-element set. That set matches what this harness's registry really holds: `make_factory` leaves `model_resolver`/`codegraph`/`mcp` unwired (factory.rs :352/:360/:375), so `list_models` is omitted by the :1134 conditional ("nothing to list" without a resolver) and the graph quartet by the :1468 conditional — both documented at their registration sites, and both still covered by *name* in the filter test, so combined coverage is complete. The image family and `spawn_agent` (Agent+NeedsApproval) are asserted denied, and the NeedsApproval branch's message demands classification of any future Agent+AutoRun tool. Exact-equality on a deterministic harness: no over- or under-fitting.
3. **LOW — stale TOML comments: FIXED.** new_release.toml:43 and post_merge_sync.toml:42 read "redundant-but-explicit: the Skill state grants the read surface (backlog 834ec126)"; merge_to_main.toml:75 and create_skill.toml's header (lines 20-29) are consistent with the new semantics.
4. **LOW — BUG record: WRITTEN.** `.coding/knowledge/bug/2027-01-11-read-only-tools-denied-inside-skills-skill-arm-n.md` carries symptom → root cause → fix (incl. the mcp__ exclusion and untouched guards) → both regression test names, matching what shipped.

## Doc-sweep for (c)

No remaining claim anywhere that trusted MCP tools ride the read grant: the arm comment, the `ToolFilter::Skill` variant doc, `skill_create`'s schema ("trusted mcp__ tools excluded"), `SkillSpec.tools`, PLAN.md :307-309, and create_skill.toml all state the exclusion. No doc site still implies reads need allow-list entries — FEATURES.md:20 and all four TOMLs now phrase the read surface as always-available. `src/agent/prompt.rs`'s Skill-state line makes no read-surface claim (only injects the skill prompt), so nothing to sync there.

## Filter-test read set for (d)

Matches the arm's real semantics: `read_files`, `search`, `search_read`, `expand_result`, `git_read`, `web_fetch`, `list_models`, and the graph quartet are all Agent+AutoRun non-mcp names (allowed under `Skill(vec![])`); `mcp__server__tool` is denied with a message pinning the rationale; the 8 NeedsApproval writes are denied.

## Constitution + bookkeeping (one line each)

- **Multi-platform neutrality:** pure filter/test/doc logic, no platform-specific code in the delta — clean.
- **File-tools-first:** no shell-based mutation in the delta.
- **Warning-free:** no `#[allow(...)]` in the delta; the main agent's green `cargo test` under `#![deny(warnings)]` carries the static proof (my verification is static, as scoped).
- **Documentation sync:** complete — every site the behaviour touches was updated in the same delta (verified above).
- **Bookkeeping accuracy:** backlog.jsonl (1b4dfad3 → done, 834ec126 in flight with plan linkage) matches the branch state; the BUG record matches what shipped, with one nit noted below.

## Process remarks (not findings)

- The filter test's comment at :2494-2495 ("naming it in a skill allow-list must still not grant it") explains why `write_review_report` (Agent+AutoRun) is absent from the asserted read list — the assertion itself lives in the pre-existing sibling test (:2756) and the factory audit, so coverage is real; likewise the BUG record's parenthetical attributes "denied even when named" to the filter test while that specific assertion lives in the sibling test — suite-level coverage is intact.

Reviewed-state: b2ae486665d339a1410d7f8e55533c47def27716
