+++
title = "mitigate agent mid-task stopping — chain-instruction + retry-cadence (evaluated)"
created = "2026-08-31"
+++

Evaluation of 4 proposed mitigations for the agent stopping mid-task instead of chaining obvious consecutive steps (2026-12-04). Grounded in src/agent/prompt.rs (CODING_SYSTEM_PREAMBLE L26-42, APP_RULES L92-126, error-retry bullet L101-103) + turn.rs tool-result framing (role:Role::Tool at L1402/1540/1699/1865).

VERDICTS:
1. Positive "chain consecutive steps" instruction — RECOMMEND. Directly fixes factor 1's asymmetry (prompt has many "stop" triggers, zero "continue" triggers). Trivial cost: one bullet in the preamble. Low risk (bounded by "obvious / no user input"). Cache-stable (part of stable head). ALSO folds factor 3's intent into its wording (see below).
2. Per-session throughput signal — DEFER. Highest cost (new per-turn plumbing, not a static edit), highest risk (rush/skip the read-before-write + test-before-complete discipline; conflicts with careful plan-first ethos), uncertain payoff (base model has no strong throughput prior). Adds volatile per-turn content → cache churn, which fights the project's heavy cache-stability investment. Revisit only if 1+4 don't move the needle; measure with the existing trace-log methodology.
3. Mark tool results as continuations not new questions — FOLD INTO #1. Premise only PARTIALLY holds: tool results are already role:Role::Tool (structurally continuations, NOT role:user), so the API already signals "continue." The residual "yield the floor" is a generation tendency, not a message-role bug. A per-turn "continue" injection (the realistic version) is volatile → cache churn. So capture factor 3's goal as WORDING inside #1's bullet instead of a separate mechanism.
4. Retry-cadence clause ("immediately, without pausing to narrate") — RECOMMEND. Directly fixes factor 4. The rule (L101-103) says WHAT but not CADENCE. Formalizes the already-identified HOW memory 0af9a3a2 ("RETRY IMMEDIATELY — do NOT stop to explain"). Trivial cost: append a clause. Cache-stable.

EXACT PROPOSED TEXT (for the follow-up implementation plan):
- Preamble (after L33 "Complete one step at a time; complete_step checks each one off."):
  "- Chain consecutive obvious steps: when the next action needs no user input and no decision, take it in the same turn (retry a failed call with corrected args, re-run a test after a fix, continue to finish after a commit). Tool results are continuations of your turn, not new questions — keep going until you hit a real choice point or need the user."
- APP_RULES error-retry bullet (L101-103), append:
  " ... re-issue corrected immediately — in the same turn, without pausing to narrate the error first."

IMPLEMENTATION COST: trivial text edits + reconcile co-located test markers in prompt.rs test module (asserts substrings at L721 "Plan before you code", L780 "Never resend a failed tool call" — pattern documented in plan 2fb308d6). Both edits are in the byte-stable head, so cache-friendly.

CROSS-CUTTING RATIONALE: the project has invested heavily in cache-stable prompt sections (stable head, byte-stable; multiple cache-hit optimization rounds per recalled PLAN memories). This is a strong project-specific reason to prefer the two stable-head text edits (#1, #4) over the two volatile/per-turn mechanisms (#2, #3-as-injection).

STATUS: evaluation only — no source changes made. Implementation of #1+#4 pending user approval as a follow-up implementation plan (with review + test-marker reconciliation).
