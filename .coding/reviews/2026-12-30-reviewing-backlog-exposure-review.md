## Verdict: FINDINGS (0 high, 1 low)

Plan 8a16bc91 (expose backlog_add/backlog_status in Reviewing — 2026-12-30 reversal of the 2026-09-04 exclusion) is correctly and completely implemented: the Reviewing arm, prompt fragment, flipped/renamed tests, and the knowledge/memory supersede chain all verify clean, and the Reviewer role filter is untouched and stays read-only. One low-severity test-coverage finding (L1) on the renamed visibility tests. Full detail appended below.

### Scope reviewed

`git diff HEAD` — 4 files: `src/tool/mod.rs` (68 lines), `src/agent/prompt.rs` (+2/−1), `.coding/backlog.jsonl` (+1), `.coding/knowledge/decision/2026-08-23-reviewing-drops-backlog-add-backlog-status-user.md` (+1) — plus the untracked files (`.coding/knowledge/decision/2026-12-30-reviewing-drops-backlog-add-backlog-status-super.md`, `.coding/plans/8a16bc91.md`). Test evidence: root `cargo test` exit=0 and src-tauri `cargo test` exit=0, both under `#![deny(warnings)]` at the crate roots — warning-free build proven.

### Verified — all six requested checks

**1. Reviewing arm change correct and complete — PASS.**
- `ToolFilter::Reviewing`'s Workflow arm (src/tool/mod.rs:434-452) now allow-lists `backlog_add` and `backlog_status` alongside `backlog_list`, after `finish`/`abandon_plan`/`ask_user`/`current_plan`/`update_plan`. The new comment documents the reversal rationale (user-facing queue operations, never blocked mid-closing-sequence; 2026-12-30 reversal of the 2026-09-04 decision after two blocked requests and one silently-lost add; reviewer subagent still excluded via `ToolFilter::Reviewer`).
- The diff's only arm-region hunk is `@@ -437,11 +437,17 @@` — every other state arm is untouched and already exposes all three tools: Planning (:322-324), Executing (:361-363), ExecutingResearch (:393-395, "Unchanged from Executing"), Complete (~:489-495).
- Skill filter treatment unchanged (:499-514): `backlog_status` + `backlog_list` ride the always-available set inside skills; `backlog_add` stays allow-list-only — pinned by the tests' Skill assertions (`!visible(Skill(vec![]))` for backlog_add, `visible(Skill(vec![]))` for backlog_status).

**2. Reviewer ROLE filter untouched — PASS.**
- `ToolFilter::Reviewer` (:516-526) remains the strict allow-list (`current_plan` + only the named tools); `REVIEWER_BASE_TOOLS` (src-tauri/src/ipc/spawn.rs:254) names `backlog_list` but NOT `backlog_add`/`backlog_status`; spawn.rs is not in the diff.
- Pinned by tests: `reviewer_filter_is_a_strict_allow_list` (~mod.rs:1935) asserts the reviewer surface excludes `backlog_add` and `backlog_status`; `reviewer_filter_grants_nothing_implicitly` asserts `backlog_status` stays hidden under an empty reviewer list; the backlog_list visibility test asserts the reviewer sees it only when explicitly named. The backlog.rs:333 doc ("backlog_add / backlog_status stay out of the reviewer surface") remains accurate.

**3. No stale references to the exclusion — PASS.**
- The only Rust hit for "backlog mutations" is spawn.rs:252 — the reviewer-surface doc, which describes current CORRECT behavior (the reviewer excludes them).
- "not available in Reviewing" / "no backlog work during the closing sequence" appear in no source file — only inside `.coding/plans/8a16bc91.md` (the plan's own historical context/step text; plans are immutable history).
- The 2026-09-04 citations inside the new arm comment and test doc comments are historical citations of the reversed decision — expected and correct.
- Old test names have zero code references. Hits are historical plan documents only, plus `spawn_agent_visible_in_all_base_states` — a DIFFERENT test whose "base states" name remains accurate (spawn_agent is Skill-hidden, so it is genuinely base-states-only).

**4. Prompt fragment consistent — PASS.**
- `STATE_REVIEWING` (src/agent/prompt.rs:296-307) gained: "backlog_add / backlog_status are available too — a user request to queue or update a backlog item is never blocked mid-review." — exactly matching the arm (both mutation tools; `backlog_list` was never enumerated in the fragment and remains a read-only query available everywhere).
- `reviewing_state_block_points_at_app_rules_without_duplicating_them` (prompt.rs:1363) is unaffected — it pins the closing-sequence pointer strings and the absence of head-duplicated verdict/bug-checklist text; the added sentence is neither. Passed in the final run.

**5. Project checks — PASS.**
- **Documentation sync:** the knowledge supersede chain is clean — the 2026-08-23 `-user.md` record now carries `status = "superseded"` in frontmatter; the new pointer file `2026-12-30-...-super.md` (untracked, ships with the commit) records the supersede with accurate history. The memory chain is clean: the old DECISION record is superseded, the new DECISION record is live, and both carry-over records (the PLAN-request and the QUEUED plan-steps item) are marked superseded. No other knowledge record describes the exclusion as current (the strict-reviewer-surface spec concerns the Reviewer ROLE — unchanged and still accurate).
- README.md/PLAN.md need no update: PLAN.md's only backlog mention is the jsonl-format row. README.md:76 ("update their status from any workflow state via `backlog_status`") was slightly stale BEFORE this change and is now literally true — the change brings the code in line with the docs. README.md:20's "in Reviewing only the closing tools" is the pre-existing conceptual one-liner about state-machine gating (Reviewing has always exposed agent/browser/memory tools for the fix-commit loop); the backlog tools are part of that closing surface, so the sentence is not made false.
- **Multi-platform neutrality:** pure Rust name-comparison logic, string constants, and test assertions — no paths, no platform APIs, no shell syntax.
- **Warning-free build:** root + src-tauri `cargo test` exit=0 under `#![deny(warnings)]` at both crate roots.

**6. Test quality — PASS with one gap (L1 below).**
- The Reviewing-surface test's two flipped `!contains` → `contains` assertions pin the new surface end-to-end (registry → `ToolFilter::Reviewing` → emitted schemas) — reverting the arm would fail them.
- The three visibility tests genuinely pin the new behavior: `backlog_add_visible_in_all_states` (Planning/Executing/Reviewing/Complete visible + Skill allow-list-only), `backlog_status_visible_in_all_states_including_skills` (all four states + `Skill(vec![])` visible), `backlog_list_visible_in_all_states_and_skills` (all states + Skill + reviewer named-only semantics). Doc comments updated consistently with the renames.
- Renames are consistent everywhere — no dangling references to `backlog_add_visible_in_all_base_states` / `backlog_list_visible_in_all_base_states_and_skills` in any source file.

### Findings

**L1 (low) — the renamed "all_states" tests never assert the ExecutingResearch filter variant.**
`backlog_add_visible_in_all_states` (~mod.rs:1748-1762), `backlog_status_visible_in_all_states_including_skills` (~:1778-1790), and `backlog_list_visible_in_all_states_and_skills` (~:1806-1815) assert the four base-state filters plus Skill, but never `ToolFilter::ExecutingResearch` — whose Workflow arm exposes all three tools (mod.rs:393-395, verified). The rename in this diff broadened the names from "all_base_states" to "all_states", so the names now promise more than the assertions pin: a future refactor that dropped a backlog tool from the ExecutingResearch arm would leave all three tests green. Pre-existing gap (the old tests didn't assert it either), surfaced by this diff's rename. Fix: add `assert!(visible(ToolFilter::ExecutingResearch));` to each of the three tests — the assertions pass against the current arms.

### Verified non-issues (no action needed)

- `priority_class` (mod.rs:885-927): backlog_add/backlog_status remain class 2 ("state-dependent workflow and mutation tools"). Still valid on the mutation axis; class-2 tools live in the mutable tail, so the class 0+1 byte-stable-prefix property is unaffected and `state_transition_preserves_class0_and_class1_prefix` passes. The doc's Class-1 example "backlog_*" was already loose before this change (only `backlog_list` is in `UNIVERSAL_BASE`) and is untouched by the diff.
- The codegraph index still resolves the OLD test names — that is the gitignored, rebuildable cache lagging the working tree, not a code issue; it converges on the next rebuild.
- The +1 line in `.coding/backlog.jsonl` (item af572504, plan-steps headline/collapsible details) is the user's queued request re-landed through the now-unblocked tool — consistent with the change's motivation and the superseded QUEUED carry-over record; it should ship with the commit.
- Plan kind is implementation (not bug_fixing), so the bug-plan extras (regression-test naming, BUG: memory) don't apply; the flipped assertions serve as the behavior pin.
- Security: no surface change — both tools are AutoRun, sandboxed to `.coding/` state, and were already exposed in four other states; the untrusted-ish reviewer surface still excludes them.
