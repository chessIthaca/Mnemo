# Plan: Fix Enter not running slash commands (/compact, /new)

## Goal
Enter on a fully-typed argument-less slash command (/compact, /new, /clear, /panel, /help — including the completed "/compact " state with trailing space) runs the command immediately, via a data-driven takesArgument flag so future argument-less commands cannot regress.

## Kind
bug_fixing

## Context
Root cause verified by reading frontend/src/components/layout/InputBar.tsx and frontend/src/lib/slash.ts in full. (1) handleKeyDown's menu-open Enter branch (InputBar.tsx:549-564) only runs the input via handleSend when hasArgs (whitespace in text.slice(1).trim()); otherwise it calls completeSlashCommand. (2) completeSlashCommand (:179-203) hardcodes the immediate-run arm to clear|panel|help; compact and new are argument-less but fall to the default arm, which completes to "/name " (trailing space — meant for arg-taking /model, /provider, /load). (3) On the next Enter the hasArgs check trims that trailing space away, so Enter re-completes to "/compact " forever — an infinite loop; Enter can never run /compact or /new. The Send button works because it calls handleSend directly, and parseSlash("/compact ") parses to {type:"compact"}. Note: the immediate-run arm invokes handleSlashCommand DIRECTLY (not via handleSend) to bypass handleSend's steer-mode interception — /clear, /compact, /new are designed to work while the agent runs (interrupt/deny_all/compact-mid-turn). The fix must preserve that. Frontend test gate: vitest node env with an EXPLICIT include list in frontend/vitest.config.ts (no src/lib/slash.test.ts entry yet — must be added); InputBar.test.ts is a ?raw source-contract suite (established pattern). No Rust change (the Rust slash module is the console REPL's line-based parser — unaffected).

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
Typing a slash command like /compact (or /new) and pressing Enter never sends it — Enter re-completes the text to "/compact " (trailing space) and every further Enter re-completes identically; only the Send button runs the command. /clear, /panel, /help work via Enter.

## Regression test
resolvesExactLoopStates
