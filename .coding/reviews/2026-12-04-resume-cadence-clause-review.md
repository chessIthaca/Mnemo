## Verdict: PASS

Clean, minimal, correct change. One system-prompt text edit + one co-located regression assertion in `src/agent/prompt.rs`. No findings.

### Scope reviewed
- `git diff HEAD` on `wt/agenticcoding`: 1 file, +11/-1 (`src/agent/prompt.rs`).
- Untracked side-car files (`.coding/plans/e0e93c8b.md`, `.coding/knowledge/bug/2026-08-31-reviewer-finish-resume-stall-continuation-gap-no.md`) are bookkeeping, not source — out of scope for code review but confirm the plan + bug record exist.

### Correctness
- **Lands in the right place.** The clause is appended to the reviewer-spawn bullet in `APP_RULES` → `CLOSING SEQUENCE` → item 2 (Review), immediately after "After spawning, END the turn — the finish notification resumes you; never sleep/poll." (prompt.rs:121–125). Exactly the reviewer-spawn bullet, matching the plan goal.
- **String-continuation syntax is correct.** `APP_RULES` is a `&str` using `\` line-continuations (strip newline + leading whitespace). All four edited lines end with ` \` (space + backslash). Traced joins: `…sleep/poll. When it resumes you, immediately continue the closing sequence — read the report, fix findings, re-run tests, commit, finish — don't stall or wait for the user. Reviewer failed…` — single spaces throughout, no double/missing spaces, no broken continuation. Em-dashes are valid in a UTF-8 `&str`.
- **Test matches prompt.** `stable_head_contains_universal_closing_rules` asserts `head.contains("immediately continue the closing sequence")` (prompt.rs:811–814), which appears verbatim in the new clause. The phrase is unique to this clause, so the assertion fails without the edit and passes with it — a valid regression test. Placed directly after the retry-cadence assertion, the natural co-located spot.

### Bugs
None. No escaping issues, no continuation breaks, no test/assertion mismatch.

### Constitution
- **Documentation sync.** Internal compiled-prompt text change; not user-facing. No README/PLAN.md/`endpoints.toml` update required. Plan + bug knowledge files exist. ✓
- **Multi-platform neutrality.** Prompt text is platform-agnostic — no OS-specific APIs/paths/syntax. ✓
- **Warning-free build.** Crate is `#![deny(warnings)]`. Change adds only string content + one `assert!`; no new imports, dead code, or unused `mut`. Reported suite (1705 passed, 0 failed, 16 ignored) confirms zero warnings. ✓

### Cache stability
The edit is inside the `APP_RULES` const, which is part of `build_stable_head` — byte-stable across turns, no per-turn/volatile content (no workflow state, no recalled memories). The new test asserts the clause is present in the stable head, confirming placement. Consistent with the prior retry-cadence clause (mitigation ④), which lives in the same const and is tested the same way. ✓

### Side effects / consistency
- **No conflict with "After spawning, END the turn."** The two clauses cover different turns: "After spawning, END the turn" governs the *spawn* turn (don't keep going / sleep / poll after `spawn_agent`); "When it resumes you, immediately continue…" governs the *resume* turn (the finish notification coming back). The phrase "When it resumes you" explicitly marks the temporal transition, and "it" anaphorically refers to "the finish notification" from the preceding clause ("the finish notification resumes you") — grammatically coherent, no contradiction.
- **Consistent with the chaining bullet (mitigation ①).** ① frames continuations around *tool results*; this clause frames continuations around *resume notifications*. Complementary triggers, not conflicting — both say "this is a continuation of your turn, keep going."
- **Accurate step enumeration.** "read the report, fix findings, re-run tests, commit, finish" mirrors CLOSING SEQUENCE steps 2→5 from the resume point (spawn already done). Reinforces the numbered list rather than contradicting it — intentional, same reinforcement pattern as the retry-cadence clause.

### Verdict
PASS — ship it.
