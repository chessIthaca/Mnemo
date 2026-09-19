## Verdict: PASS

All three round-1 findings are verified fixed in the committed tree (HEAD `2460cda` on `wt/mnemo`, working tree completely clean), each fix matches exactly what its finding asked for, and the fix deltas introduce no new issues — they are two doc-string/comment edits and two new markdown knowledge files, with no code-path changes.

## Scope

Round-2 verification of the fixes for the three LOW findings in `.coding/reviews/2026-09-19-skill-exits-always-available-review.md` (plan `dff6780f`, bug_fixing, 4/4 steps). Inspected: the full diffs of commits `c7a8709` and `2460cda`; the committed tree at HEAD (the working tree is clean — `git diff HEAD` and `git status --short` both empty — so every file read IS the committed state); the memory store (record_type=bug search for the defect); and a repo-wide sweep for any remaining stale "always available" enumeration.

## Round-1 findings — all three verified fixed

### LOW 1 — `skill_create` schema description + workflow/mod.rs shorthand comment: FIXED

- `src/tool/workflow/skill.rs:432` (committed): the description now ends "The memory tools, ask_user, current_plan, skill_reload, the backlog tools and the exits skill_end/abandon_skill are always available inside a skill and need not be listed in `tools`." — exactly the round-1 suggested phrasing, mirroring SKILL_FILE_HEADER (skill.rs:304-306, updated by the core fix).
- `src/workflow/mod.rs:407` (committed): "(permissive: memory/ask/backlog and the skill exits auto-granted)" — the exits are no longer elided.
- **Accuracy against the actual filter:** the `ToolFilter::Skill` arm (src/tool/mod.rs:831-866) grants exactly what the docs enumerate — the Memory category (all memory tools), ask_user, current_plan, skill_reload, skill_end, abandon_skill, backlog_add/status/list, plus the allow-list. Base states still hide the exits: the unit test `skill_exits_are_always_available_inside_a_skill` (src/tool/mod.rs:2396-2416) pins the Planning/Executing/Reviewing/Complete denials and the Reviewer arm's strictness, and the Complete arm's code does not name them. The two sentences are accurate, not merely present.

### LOW 2 — stranded knowledge file: FIXED

- Commit `c7a8709` ("knowledge: commit the tool-card timing SPEC record") adds ONLY `.coding/knowledge/spec/2027-01-11-tool-card-timing-duration-wall-clock-next-to-the.md` (+10 lines, new file) — a conscious, dedicated commit whose message cites review LOW 2 and plan dff6780f. It sits between `4466223` (the feature commit the SPEC documents) and the bug-fix commit, so it did NOT ride `2460cda` unexamined — exactly the "own commit" option round 1 asked for.
- The working tree is now completely clean (`git diff HEAD` and `git status --short` both empty) — no stranded files remain.

### LOW 3 — BUG memory record: FIXED

- The memory store returns the record: "BUG: skill_end/abandon_skill denied mid-skill unless the allow-list named them — FIXED (plan dff6780f)" (record_type=bug search for the defect; id e91a3cc5-35b8-5e96-8408-12dba409a291).
- The knowledge file `.coding/knowledge/bug/2027-01-11-skill-end-abandon-skill-denied-mid-skill-unless.md` is committed in `2460cda` (+12 lines) and carries every required element: symptom (the 2027-01-24 user report; mid-skill denial while the injected prompt advertised both), root cause (the Skill arm auto-granted only memory/ask_user/current_plan/skill_reload/backlog; the exits fell through to the allow-list), fix (both exits joined the always-available set; docs updated; listing stays legal), and BOTH regression test names — `skill_exits_are_always_available_inside_a_skill` (verified present at src/tool/mod.rs:2396) and `dispatch_allows_skill_end_even_when_the_allow_list_omits_it` (verified present at src/agent/tests.rs:6090) — each with an accurate one-line description. Plan step 2's checkbox no longer overstates what was done.

## No new issues from the fix deltas

- LOW 1's delta is two doc-string/comment edits (a string literal in `ToolSchema::new` and a `//` comment) — no code path, no behavior. No test asserts the schema description text (a repo-wide search finds the phrasing only in skill.rs itself, the round-1 report, and knowledge/plan files).
- LOW 2's and LOW 3's deltas are pure new-file adds of markdown knowledge files — no code.
- Repo-wide "always available" sweep: no remaining stale enumeration of the skill's always-available set anywhere in source (benign hits listed under non-issues).

## Verified non-issues (recorded so they aren't re-litigated)

- **SkillSpec doc (src/skill/mod.rs:46-47)** says only "Memory tools are always available regardless of this list" — a memory-tools-only statement that never enumerated the set, so the exits change does not make it stale (same shape it already had when skill_reload was added; accurate as far as it goes).
- **The Skill arm's leading comment (src/tool/mod.rs:832-842)** enumerates "Memory tools + ask_user + current_plan + the backlog tools" without skill_reload/exits, but the inline comments at each grant (847-859) document skill_reload and the exits with the never-stuck rationale — the arm's established pattern (leading comment = core, inline = additions), verified correct in round 1.
- **Historical texts quoting the old phrasing** (the 2026-09-17 round-2 review, plan dff6780f's pre-fix anchors, the round-1 report itself) are immutable history, not live docs.
- **Stale code-graph index:** graph_search could not resolve the dispatch test symbol — the index predates the commit (34 files on disk newer than the index). A cache state, not a code issue; the literal search confirms the test at src/agent/tests.rs:6090.
- **Shipped skill files** (.coding/skills/*.toml) carry no "always available" phrasing — nothing to update there.

## Test-suite claim

Not re-runnable by this read-only reviewer; the claimed post-fix run (2489 passed, 0 failed, 5 ignored, +16 integration, exit 0) is consistent with the deltas — doc strings, a comment, and two markdown files cannot affect compilation or test outcomes, and the round-1-verified core fix (filter arm + both regression tests) is unchanged in the committed tree.
