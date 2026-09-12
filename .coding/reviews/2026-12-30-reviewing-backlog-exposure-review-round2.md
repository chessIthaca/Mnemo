## Verdict: PASS

Round-2 verification of the round-1 L1 fix for plan 8a16bc91 (expose backlog_add/backlog_status in Reviewing — 2026-12-30 reversal of the 2026-09-04 exclusion). The fix is correct and complete: all three renamed visibility tests now assert ExecutingResearch visibility, the assertions match the current arms, commit 61cc0ab contains exactly the plan's change plus the three one-line assertions and the round-1 report, and the tree is clean and green. No findings.

### Scope reviewed

- `git show 61cc0ab` (full diff) — HEAD of `wt/agenticcoding` (`.git/HEAD` → `refs/heads/wt/agenticcoding`; `git log -3` confirms 61cc0ab is HEAD, on top of a5eb009/b592d92).
- `git diff HEAD` + `git status --short` — both empty: the working tree is identical to the committed tree, so every line read below IS the shipped state.
- `src/tool/mod.rs` current state: the ExecutingResearch and Reviewing arms (:380-454) and the three visibility tests (:1741-1829).
- Round-1 report: `.coding/reviews/2026-12-30-reviewing-backlog-exposure-review.md` — the in-commit copy is identical to the working-tree file (clean tree).
- Test evidence: targeted `cargo test tool::tests::backlog` exit=0 (all three ok) and full root `cargo test` exit=0 after the fix, as reported in the review request. This reviewer's surface is read-only (no shell), so verification combines those runs with static proof that the assertions must pass (below).

### Check 1 — L1 fix correct and complete: PASS

All three tests carry the new assertion, exactly once each, in the correct test, placed between the Executing and Reviewing assertions (consistent style):

- `backlog_add_visible_in_all_states` — src/tool/mod.rs:1760
- `backlog_status_visible_in_all_states_including_skills` — :1791
- `backlog_list_visible_in_all_states_and_skills` — :1819

Each is `assert!(visible(ToolFilter::ExecutingResearch), "visible in ExecutingResearch");` — the round-1 fix prescription verbatim.

The assertions pass against the current arms: `ToolFilter::ExecutingResearch`'s Workflow arm (:385-396, "Unchanged from Executing") allow-lists all three tools — `backlog_add` (:393), `backlog_status` (:394), `backlog_list` (:395) — and the `visible` closure resolves through `r.schemas(&caps, &f)` (registry → filter → emitted schemas), the same end-to-end path the Reviewing-surface test pins. Statically the assertions cannot fail against these arms; the reported targeted run (all three ok) is the empirical confirmation. The L1 gap — names promising "all states" while not pinning ExecutingResearch — is closed: a future refactor that dropped a backlog tool from the ExecutingResearch arm now fails all three tests.

### Check 2 — the fix introduced nothing else: PASS

`git show 61cc0ab` touches exactly seven files, matching the described inventory one-for-one:

1. `src/tool/mod.rs` — the plan's change (Reviewing arm allow-list + reversal comment :440-451; Reviewing-surface test flips :1715-1720; the three test doc-comment rewrites with the backlog_add/backlog_list renames; the Reviewing flips in the backlog_add/backlog_status tests) PLUS the three one-line ExecutingResearch assertions. The complete hunk list (@@ -437,11 +437,17 / -1706,10 +1712,11 / -1731,16 +1738,16 / -1750,9 +1757,10 / -1761,11 +1769,11 / -1780,9 +1788,10 / -1791,12 +1800,13 / -1806,6 +1816,7) contains no other code change.
2. `src/agent/prompt.rs` — the STATE_REVIEWING sentence (+2/−1), unchanged from the round-1 review.
3. `.coding/backlog.jsonl` — +1 line: the af572504 add (the re-landed user request).
4. `.coding/knowledge/decision/2026-08-23-reviewing-drops-backlog-add-backlog-status-user.md` — +1 `status = "superseded"` frontmatter line.
5. `.coding/knowledge/decision/2026-12-30-reviewing-drops-backlog-add-backlog-status-super.md` — new supersede pointer file.
6. `.coding/plans/8a16bc91.md` — new plan file.
7. `.coding/reviews/2026-12-30-reviewing-backlog-exposure-review.md` — new round-1 report.

No other source files, no stray edits, no `#[allow]`, no formatting churn. The commit message accurately describes the full contents, including the round-1 L1 fix and the round-1 report citation.

### Check 3 — no regressions (root cargo test green): PASS

The fix is test-only: three `assert!` lines inside `#[cfg(test)] mod tests`. The working tree is clean, so the committed tree is exactly the tree the reported runs executed: targeted `cargo test tool::tests::backlog` exit=0 (all three ok) and full root `cargo test` exit=0 — the latter under `#![deny(warnings)]` at the crate root, so the build is also proven warning-free (the three added lines are plain assertions with string literals; nothing warning-worthy). src-tauri need not be re-run: it compiles the lib crate without its `#[cfg(test)]` module, so its compiled input is bit-identical to the round-1-verified state (round-1 src-tauri run exit=0 stands).

### Check 4 — nothing else regressed: PASS

- Clean tree (`git diff HEAD` and `git status --short` both empty) — nothing unreviewed ships.
- The round-1 report's other five checks (Reviewing arm correctness, reviewer ROLE filter untouched, no stale references, prompt fragment consistency, project checks) are unaffected by three test-module assertion lines; their round-1 PASS verdicts carry over, and the current-tree reads here re-confirm the Reviewing arm (:434-452) and the flipped Reviewing-surface assertions (:1715-1720).
- The reviewer ROLE filter remains read-only and correct — spawn.rs is not in the diff.
- No dangling references: the renames were verified consistent in round 1; the fix adds only assertions.

### Notes

- This reviewer's tool surface is read-only (no shell), so test verification = the reported exit=0 runs + static proof that the assertions match the arms read directly from the committed tree; given the change is three test lines against explicit allow-lists, residual risk is nil.
- The round-1 report ships inside the commit as reviewed (clean tree ⇒ in-commit copy = working-tree copy).
