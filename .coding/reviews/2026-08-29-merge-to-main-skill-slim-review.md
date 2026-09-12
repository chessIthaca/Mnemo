## Verdict: PASS

Review of plan b1b45b69 ("Slim the merge_to_main skill (prompt trim + skill_start echo de-dup"), branch wt/agenticcoder, repo C:\AgenticCoder\AgenticCoder. All uncommitted changes reviewed via git diff HEAD: `.coding/skills/merge_to_main.toml` (prompt rewrite + expanded header comment), `src/tool/workflow/skill.rs` (echo trim + regression test), plus riding-along `.coding/` bookkeeping (plan b1b45b69.md, knowledge records, backlog.jsonl entries — normal for this repo).

### 1. Operative-rule preservation — ALL rules survive (the core risk, verified line-by-line against the removed prompt lines)

| Old rule (removed lines) | New location | Status |
|---|---|---|
| (a) Never stash; commit ALL uncommitted work incl. `.coding/` (plans, knowledge, reviews, backlog.jsonl must land in the merge) | New step 1 (line 47); stash rationale in header comment lines 14-16 | ✅ |
| (b) `git checkout main` → `git merge --no-ff <branch>`; main receives only merge commits | New step 2 (line 48) | ✅ |
| (c) Conflicts: file_edit, `git add` + `git commit`; backlog.jsonl union merge driver; rest of `.coding/` plain text | New step 2 (line 48) | ✅ |
| (d) VERIFY with BOTH `npm run build` AND `cd src-tauri && cargo build`; fix + re-run both until green; bin-crate rationale | New step 3 (line 49); why (E0425/E0027, Record<...> tab-key example) in header lines 17-22 | ✅ |
| (e) `git branch -d` only after merge commit in main AND both builds green | New step 4 (line 50) | ✅ |
| (f) Memory supersession: memory_recall branch name + pre-merge tip; memory_list prefix "SHIPPED"; working-tier "ACTIVE:"/"STEP MARKER:" crash markers; successor with EXACT phrase "MERGED into main at <sha> (`git rev-parse main`) on <date>"; never other branches' records; skip cleanly; never hand-edit/re-derive the DB; memory tools available inside skill | New step 5 (line 51) | ✅ Exact phrase "MERGED into main" present — pinned test `shipped_skill_files_parse_and_merge_to_main_cleans_up_memories` (src/skill/mod.rs:261-277) passes unchanged |
| (g) skill_end → Planning; abandon_skill on unresolvable conflicts; optional approval-gated push | New step 6 (line 52) | ✅ |
| Old footer: merge/push always approval-gated even in Autonomous mode | Header comment lines 26-29 + compiled APP_RULES ("Core operations (git merge, git push) are ALWAYS approval-gated; no safety mode or skill bypasses this") + agent.md:7 preamble. Enforcement is in dispatch (never_auto_for), independent of prompt text. | ✅ Not lost — gate is always in context and enforced in code |

Only prose/rationale was removed (ten-orphan-stashes story, error-code examples, exact hint-string format); every operative instruction survives, most verbatim. The header comment now carries the rationale (never injected — TOML comments are not parsed, confirmed by the loader reading only `name`/`available_in`/`target_state`/`tools`/`prompt`).

### 2. TOML validity — safe

Multi-line basic string (line 45 `"""\` opener, line 52 trailing `\` before line 53 `"""`). Scanned every line: the ONLY backslashes are the two line-ending continuations — no `\n`/`\"`/`\\` escape sequences or stray backslashes in content; no `"""` inside the string. Embedded newlines between steps 46-51 are preserved (valid; consistent with the old file's structure). Covered by tests: `shipped_skills_embedded_entries_parse_and_carry_their_name` parses the embedded copy (include_str! at src/skill/mod.rs:144) and `shipped_skill_files_parse_and_merge_to_main_cleans_up_memories` loads the file from disk — both green (full suite 1629 passed per plan).

### 3. Rust — clean

- `prompt` remains used: borrowed by `wf.start_skill(&args.skill, &prompt, ...)` (skill.rs:146) — no dead code, `#![deny(warnings)]` safe.
- New result "started skill '{skill}' (target: {target}). Goal injected into the system prompt — follow it." names skill + target and points at the system prompt, exactly as planned. Inline comment (lines 147-150) explains why.
- Regression test `start_skill_result_does_not_echo_the_prompt` (skill.rs:342-361) is a true regression test: with the old code the result contained "Drive toward the goal: Merge the branch into main." → the `!r.output.contains("Merge the branch into main.")` assertion fails; with the fix it passes. It also asserts the workflow still records the full prompt (`skill.prompt == "Merge the branch into main."`), pinning both halves of the contract. Fixture prompt in `make_registry` matches.
- No public-item doc comments touched; no platform-specific code (multi-platform: ✅ — pure Rust + text content).

### 4. Documentation sync — no stale docs

README.md:45-46, PLAN.md:146+232, agent.md:51-54 all describe topology/merge-hygiene/supersession at the behavioral level; none quote the prompt text or the old "Drive toward the goal" result string (repo-wide search confirms the string survives only in backlog/plan bookkeeping, not code/docs/tests). Header comment's pointer claims verified: topology in agent.md:51-52 ✅; approval gate referenced in agent.md:7 preamble + compiled APP_RULES ✅; memory auto-convergence in agent.md:54 ✅. Nothing stale.

### 5. Known-accepted caveats (confirmed, not findings)

- Seed is write-if-missing (src/skill/mod.rs:147-158, Project::init) so existing projects keep their old copy; the embedded SHIPPED_SKILLS copy (include_str!) IS the new trimmed file, so new/self-healed projects get it. Deliberate; noted in DECISION memory.
- Uncommitted `.coding/` bookkeeping rides along — normal for this repo.

### Observations (non-findings)

- New prompt step 1 merges old steps 1+2 ("check status" + "commit") — no rule lost.
- The exact unmerged-hint string format ("branch <name> @ <sha> (unmerged…)") is trimmed to "finish digests carry an unmerged-branch hint" — descriptive detail, not an operative rule; the operative search instructions (recall branch+tip, list prefix SHIPPED) are intact.

No findings. Ready to commit.
