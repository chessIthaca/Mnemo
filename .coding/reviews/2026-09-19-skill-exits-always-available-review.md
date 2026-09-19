## Verdict: FINDINGS (0 high, 3 low)

The core fix is correct, minimal, and genuinely regression-tested: adding `skill_end`/`abandon_skill` to the `ToolFilter::Skill` arm's always-available set closes the mid-skill trap exactly as diagnosed, and both new tests fail pre-fix (the unit test via the empty allow-list, the dispatch test via the gate). No behavioral, security, or platform findings. The three findings are documentation and bookkeeping: a stale model-facing authoring doc the plan missed, a stranded unrelated knowledge file in the working tree, and the missing BUG memory record despite plan step 2 claiming it was written.

## Scope

Full uncommitted diff on `wt/mnemo` (4 files, +118/−8: `src/tool/mod.rs`, `src/agent/factory.rs`, `src/tool/workflow/skill.rs`, `src/agent/tests.rs`) against plan `dff6780f` (bug_fixing, 4/4 steps). Also inspected the untracked files (`.coding/plans/dff6780f.md`, `.coding/knowledge/spec/2027-01-11-tool-card-timing-…md`), the filter's construction sites, the sub-agent spawn path, the injected skill prompt, and the memory store.

## Verified correct

1. **Filter fix (src/tool/mod.rs:855-861).** Both exits join the Skill arm's always-available name set (the `_ =>` catch-all covers their Workflow category), with a comment citing the never-stuck rule in the same spirit the Reviewing arm documents it for `abandon_plan` (mod.rs:401-402, 740-744). The two hard guards at the top of `allows` (`write_review_report` reviewer-only, `skill_create` Executing-only) are untouched — no widening.
2. **No base-state reach.** `ToolFilter::Skill` is constructed only in `Workflow::allowed_tools` (workflow/mod.rs:408-415): (a) `state == Skill` with an active overlay — the mid-skill case; (b) an in-memory non-reviewer sub-agent allow-list. In case (b) the sub-agent's own workflow has no active skill, so both tools error "failed to end/abandon skill: no skill is active" (workflow/mod.rs:1101-1122) — a clear, mutation-free error. All four base-state arms leave the exits unnamed; the new unit test pins each denial plus the Reviewer arm.
3. **No dangerous widening.** Both tools are AutoRun, no-arg, and only restore workflow state (`end_skill` → the validated lifecycle target; `abandon_skill` → the recorded pre-skill state). Neither touches files, plans, or the backlog.
4. **Schema advertisement is consistent end-to-end.** `schema_filter` (workflow/mod.rs:489-510) delegates to `allowed_tools()` mid-skill, so the exits now appear in the advertised tools array — required for the fix to work (the model must see them to call them) and consistent with the injected skill prompt (prompt.rs:770-775), which advertises both.
5. **Test quality — both regressions are genuine.** (a) `skill_exits_are_always_available_inside_a_skill` (tool/mod.rs:2396-2416) asserts both exits allowed under `Skill(vec![])` — the exact pre-fix failure shape — hidden under Planning/Executing/Reviewing/Complete, and not implicitly granted under `Reviewer(vec![])`. (b) `dispatch_allows_skill_end_even_when_the_allow_list_omits_it` (agent/tests.rs:6090-6157) registers `SkillEndTool` in a minimal registry so the unknown-tool branch cannot mask the gate (the neighboring `dispatch_denies_tool_outside_skill_allow_list`, tests.rs:6047, proves `execute_tool_call` enforces this filter), starts a skill whose allow-list omits the exit, and asserts success + "skill ended". Pre-fix it fails with "not allowed … Skill".
6. **Existing allow-list tests stay legal.** tests.rs:10630 and :10823, and workflow/mod.rs:3201 (`allowed_tools_returns_skill_filter_when_active`) list `skill_end` in allow-lists and are unaffected — naming the exits is redundant, not denied.
7. **Doc comments updated and accurate**: the Skill variant doc (mod.rs:468-475), the Complete arm comment (mod.rs:808-810), the factory doc (factory.rs:1056-1058), and SKILL_FILE_HEADER (skill.rs:299-314) all match the new behavior.
8. **Multi-platform neutrality** — pure name-matching logic and tempdir-based tests; no platform-specific code, paths, or shell syntax.
9. **File-tools-first** — no shell-mutation artifacts anywhere in the diff.
10. **README claim agreed** — README.md has zero skill mentions; the generated file header and the `skill_create` schema description are the skill-authoring docs (which is exactly why finding 1 matters).

## Findings

### LOW 1 — `skill_create`'s model-facing schema description still documents the old always-available set (src/tool/workflow/skill.rs:432)

The description ends: "The memory tools, ask_user, current_plan, skill_reload and the backlog tools are always available inside a skill and need not be listed in `tools`." The plan updated SKILL_FILE_HEADER — the file-comment twin of this sentence — but missed the schema description, the other authoring doc the model reads when writing a skill. After this change the sentence is stale/incomplete.

Fix: name the exits there too, mirroring the header's new phrasing, e.g. "…skill_reload, the backlog tools and the exits skill_end/abandon_skill are always available inside a skill and need not be listed in `tools`." Doc-only — behavior is now more permissive than documented, so no trap results — but per the project's documentation-sync rule a change that ships with a stale doc is incomplete.

Secondary, same class (optional touch-up): workflow/mod.rs:407's shorthand "any other allow-list is ToolFilter::Skill (permissive: memory/ask/backlog auto-granted, as before)" already elided current_plan/skill_reload before this change and now also the exits.

### LOW 2 — unrelated stranded knowledge file rides the working tree (.coding/knowledge/spec/2027-01-11-tool-card-timing-duration-wall-clock-next-to-the.md, untracked)

This is the SPEC record for the tool-card timing feature (plan 850862a7, landed in commit 4466223 on the old wt/mnemo line) — unrelated to this bug fix. It was written after that commit and never committed, so it has never traveled with git despite the side-car policy. Make a conscious commit decision: commit it (ideally its own commit, or noted as a rider) so it converges across instances — rather than letting a blanket `git add -A` silently mix it into the bug-fix commit unexamined. (`.coding/plans/dff6780f.md`, the other untracked file, is this plan's own file and belongs in the commit.)

### LOW 3 — the BUG memory record is missing despite plan step 2 marked complete

Plan step 2 ("Document root cause — memory_write a BUG: record") is checked, but no BUG record for this defect is findable in the memory store: a record_type=bug search for the defect and a literal "skill_end" search both return only unrelated records (Anthropic 400, reviewer-prompt rider, plan panel, DeepSeek, finish gate; skill-framework plans/specs). The bug-plan closing checks require the BUG record (symptom → root cause → fix + regression test names). Fix: memory_write it now — symptom (mid-skill skill_end/abandon_skill denied unless the allow-list named them, while the injected skill prompt advertised both), root cause (the ToolFilter::Skill arm fell through to the allow-list), fix (both exits join the always-available set), regression tests (`skill_exits_are_always_available_inside_a_skill`, `dispatch_allows_skill_end_even_when_the_allow_list_omits_it`). If the finish gate's auto-capture is relied on instead, note that — but the step-2 checkbox currently overstates what was done.

## Verified non-issues (recorded so they aren't re-litigated)

- **Sub-agent inheritance:** a sub-agent spawned mid-skill now inherits skill_end/abandon_skill in its allow-list via `compute_subagent_allowlist`'s parent-intersection (src-tauri/src/ipc/spawn.rs:551-592) — harmless: its workflow has no active skill, so the calls error clearly with no mutation. The same shape existed pre-fix whenever a skill's allow-list named the exits.
- **Exits stay hidden in base states** (pinned by the new unit test) — only meaningful mid-skill, matching the corrected Complete-arm comment.
- **Test-suite claim:** I could not re-run `cargo test` myself (read-only reviewer); the claimed 2489-passed / deny(warnings) run is consistent with the diff (no unused code, no platform imports). The main agent should keep the canonical run green before commit as usual.
