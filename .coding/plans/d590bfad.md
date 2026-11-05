# Plan: OpenSpec spec merges keep one blank line and write AND for extra outcomes

## Goal
Merging spec changes keeps exactly one blank line between requirements no matter how many merges run, and a scenario's second and later outcomes are written as AND, as OpenSpec does.

## Kind
bug_fixing

## Context
Observed 2026-11-05 on the first live OpenSpec close: .coding/openspec/specs/tier-ladder/spec.md got a doubled blank line before each requirement after the modified one, and the delta's scenario 'Lead picks escalate for fresh work' rendered '- **THEN** ...' twice.

Root cause 1 (blank lines), src/openspec/spec.rs: `Spec::parse` keeps each requirement's original Markdown in `Requirement::original`, sliced from its `### Requirement:` heading to the START of the next heading, so it includes the trailing blank line(s). `Spec::render` returns the whole original document when nothing changed (byte-stable, must stay so), but once a delta clears `Spec::original` it rebuilds: `out.push('\n'); out.push_str(&requirement.render());` and `Requirement::render` returns that original verbatim including its trailing blank line. Result: two blank lines before every preserved requirement, growing by one per merge. Fix: when `Spec::render` assembles from parts, emit each requirement block with its trailing whitespace trimmed and exactly one '\n' at its end (internal content of the block stays verbatim, including CRLF inside it if present); do the same normalization for extra (unknown) sections so a blank line separates them exactly once. `Requirement::render` called on its own may keep returning the original; the normalization belongs in `Spec::render` (or a private helper it uses). Unchanged documents must still render byte-identical to their input (existing round-trip tests).

Root cause 2 (THEN/AND), same file: `Scenario::render` prints every entry of `then` as THEN. Fix: the first `then` entry is THEN, every further `then` entry is AND, then the `and` entries as AND. WHEN entries are unchanged (all WHEN). Parsing is unchanged (an AND bullet goes to `and`), so a rendered scenario with then=[a, b] parses back as then=[a], and=[b]; that is the intended OpenSpec form. Check the other readers of THEN in plan files still work: src-tauri/src/mnemo2/team.rs around line 8781 (high-risk gate scenario detection, needs at least one WHEN and one THEN) and src/memory/indexer/openspec.rs (indexes then and and). Plan files render deltas through the same Scenario::render with shifted heading levels (src/app/plan_board.rs); a plan_board test asserts '- **THEN**' is present, which stays true.

Regression tests (both fail before the fix), in src/openspec/delta.rs tests (delta apply + render):
- `modified_delta_keeps_one_blank_line_between_requirements`: parse a spec with three requirements (A, B, C, each with a scenario, separated by single blank lines as in the existing REALISTIC fixture style), apply a MODIFIED delta to A, render; assert the output contains no "\n\n\n"; then parse that output, apply a second MODIFIED delta to A (different text), render again and assert again no "\n\n\n" and that B's and C's blocks appear exactly once and unchanged. Before the fix the first assertion fails (two blank lines before B and C).
- in src/openspec/spec.rs tests: `scenario_renders_second_outcome_as_and`: Scenario::new("S", vec!["w"], vec!["t1", "t2"], vec!["a"]).render() == "#### Scenario: S\n- **WHEN** w\n- **THEN** t1\n- **AND** t2\n- **AND** a\n". Before the fix it renders '- **THEN** t2'.

Also clean the already-affected file: .coding/openspec/specs/tier-ladder/spec.md has doubled blank lines before 'Retired tier ids', 'Serialized configuration writes' and 'One Tiers page', and the scenario 'Lead picks escalate for fresh work' has '- **THEN** no worker is provisioned' which must become '- **AND** no worker is provisioned'. Edit only those lines.

Verification: `cargo test --lib openspec` (root crate; kernel module src/openspec), `cargo test --lib plan_board`, `cargo test --lib memory::indexer::openspec`, `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team` (gate detection), `cargo clippy --all-targets -- -D warnings` (root) — report test counts.

## Acceptance scenarios
- Applying two MODIFIED deltas in a row to a three-requirement spec renders with single blank lines between requirements and the untouched requirements byte-identical
- A scenario with two THEN values renders as THEN then AND
- An unchanged spec still renders byte-identical to its input
- tier-ladder/spec.md has no doubled blank lines and reads AND for the second outcome

## Steps
- [x] 1. **Normalize requirement spacing on render and write AND for extra outcomes** — files: src/openspec/spec.rs, src/openspec/delta.rs (tests only), .coding/openspec/specs/tier-ladder/spec.md (cleanup only). 1. Write the two regression tests from the context first and run them to see them fail (report the failure messages). 2. In `Spec::render` (src/openspec/spec.rs, the branch after `if let Some(original) = &self.original { return original.clone(); }`): for each requirement push '\n' then `requirement.render().trim_end()` then '\n'. For extra sections: ensure exactly one blank line before each (`out` ends with "\n\n" before pushing) and push `section.trim_end()` followed by '\n'. Do not change the early return for unchanged specs. 3. In `Scenario::render`: render `when` entries as WHEN; for `then`, index 0 as THEN and indices 1.. as AND; then every `and` entry as AND. Update the doc comment. 4. Fix any existing test whose expectation relied on the old behavior (expected doubled blank lines or a second THEN); list each one you change in your report with why. 5. Clean .coding/openspec/specs/tier-ladder/spec.md as the context says (three doubled blank lines to single, one THEN to AND), nothing else. 6. Run the verification commands from the context, all must pass; report counts. 7. Commit (`openspec: one blank line between requirements after merges; extra outcomes render as AND`) and report the commit.

## Bug
Closing a plan whose spec changes modify one requirement adds an extra blank line before every other requirement in the capability's spec.md; each later merge adds another. And a scenario with two outcomes is written as two '- **THEN**' bullets instead of THEN followed by AND.

## Regression test
modified_delta_keeps_one_blank_line_between_requirements

## Progress
- 2026-11-05 step 1 done: step 1 merged: specSpacing01 (specSpacing01) on deepseek-v4.1-flash:1017, merge commit 7391bd47; review: Spec reassembly trims each requirement block and extra section and adds exactly one newline, so merges no longer add blank lines; extra THEN outcomes render as AND; unchanged specs still render byte-identical; tier-ladder/spec.md cleaned.
