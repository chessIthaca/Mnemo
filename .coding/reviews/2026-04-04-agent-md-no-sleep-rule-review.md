# Review: Add 'never sleep-wait for a subagent' rule to agent.md

**Date:** 2026-04-04
**Plan:** Add 'never sleep-wait for a subagent' rule to agent.md
**Scope:** All uncommitted changes (`git status` / `git diff HEAD`)

## Files reviewed

- `agent.md` (substantive change — new bullet under "Standard plan closing sequence" → step 2 "Review")
- `.coding/plans/1623ba6f-...md` (bookkeeping — step 4 checkbox flipped)
- `.coding/plans/stack.json` (bookkeeping — stack pointer swapped to new plan id)
- `.coding/plans/7f94a0db-...md` (new untracked plan file)

## Findings

**No findings.**

### Verified

- **Correctness:** The rule is sound and clearly worded. "After `spawn_agent` returns, end the turn" + "the system sends a 'background agent finished' notification that resumes your turn" is factually accurate and matches the workflow's actual behavior. Co-locating it under step 2 (Review) is the right place — that's the step that triggers the `spawn_agent` call.
- **Markdown rendering:** The bullet nests correctly as a sub-item under numbered step 2. Step 2's marker `2. ` is 3 columns wide, so content begins at 3-space indent; the new bullet uses `   - ` (3-space indent) and its wrapped continuation lines use 5-space indent (3 + 2 for the `- ` marker) — both correct. No blank line is needed before the bullet and none is used, consistent with the surrounding continuation paragraphs (lines 58–68).
- **Consistency:** Tone, `**bold**` lead-ins, backtick code spans (`spawn_agent`, `Start-Sleep`, `sleep`), and em-dash usage match the surrounding constitution text. Including both `Start-Sleep` (PowerShell) and `sleep` (Unix) is appropriate given the cross-platform tooling context.
- **Bookkeeping files:** `stack.json` is valid JSON (single-line, no trailing newline — matches prior format). The two plan `.md` files are well-formed; checkbox states and stack pointer reflect normal workflow progression, not corruption.
- **Constitution compliance:** Docs-only change to `agent.md` itself; `cargo test` passes (441 tests), no code changed. No branch/commit violations — nothing committed yet.
