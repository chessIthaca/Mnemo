+++
title = "four mitigations for agent mid-task stopping (catalog + verdicts)"
created = "2026-08-31"
+++

Catalog of the four mitigations identified (2026-12-04 session) for the agent stopping mid-task instead of chaining obvious consecutive steps. Source-grounded in src/agent/prompt.rs (CODING_SYSTEM_PREAMBLE L26-42, APP_RULES error-retry bullet L101-103) + turn.rs (tool results are role:Role::Tool at L1402/1540/1699/1865).

THE FOUR MITIGATIONS + VERDICTS:
1. Add a positive "chain consecutive obvious steps" instruction to the system prompt — RECOMMEND (ship). Fixes factor 1 (asymmetric stopping rules: many "stop" triggers, zero "continue"). Trivial, cache-stable, low risk.
2. Add a per-session throughput signal — DEFER. Highest cost (per-turn plumbing), highest risk (rushes read-before-write/test-before-complete), uncertain payoff, cache-hostile. Revisit only if 1+4 don't move the needle.
3. Mark tool results as continuations not new questions — FOLD INTO #1. Tool results are ALREADY role:Role::Tool (not role:user), so the API already signals "continue"; the residual floor-yielding is a generation tendency, not a message-role bug. A per-turn injection is cache-hostile. Capture factor 3's goal as wording inside #1's bullet.
4. Add a cadence clause to the error-retry rule ("immediately, without pausing to narrate") — RECOMMEND (ship). Fixes factor 4 (error-narration gap). Formalizes HOW memory 0af9a3a2 into the stable head. Trivial, cache-stable.

SHIP: 1 + 4. FOLD: 3 into 1. DEFER: 2.

PROPOSED TEXT (exact):
- Preamble, after "Complete one step at a time; complete_step checks each one off." (L33):
  "- Chain consecutive obvious steps: when the next action needs no user input and no decision, take it in the same turn (retry a failed call with corrected args, re-run a test after a fix, continue to finish after a commit). Tool results are continuations of your turn, not new questions — keep going until you hit a real choice point or need the user."
- APP_RULES error-retry bullet (L101-103), append:
  " ... re-issue corrected immediately — in the same turn, without pausing to narrate the error first."

TEST RECONCILIATION: prompt.rs test module asserts substrings at L721 ("Plan before you code") and L780 ("Never resend a failed tool call"); these strings are preserved (not removed) by the edits, so existing assertions still pass. Add NEW assertions for the chaining bullet + cadence clause to lock the change. Pattern documented in plan 2fb308d6.

See also DECISION memory (same date) for the full evaluation rationale.
