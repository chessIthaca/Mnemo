## Verdict: FINDINGS (0 high, 2 low)

The mode-gated fix is correct, minimal, and well-tested: unattended Planning auto-continues under the 12-turn bound, interactive Planning question-waits still park, the Suggestion arm is untouched, the prefix is a true single source of truth with byte-identical rendered output, and no caller was missed. Two LOW findings (one doc gap, one narrow inherited edge) — neither blocks.


## What was verified (all 7 review-focus areas)

**1. Mode-gating correctness — PASS.**
- `workflow_expects_progress(unattended)` (loop_impl.rs:1835-1841) = `Executing | Reviewing || (unattended && Planning)`. Complete and Skill are never covered — correct: the run-all turn-resolution gate (`plan_loop_allows_done`: Complete + changed_this_turn) needs the final turn to END so the forwarder can resolve the item; auto-continuing past a final summary would burn tokens and mutate state after the done stamp.
- Interactive Planning parks: `interactive_planning_parks_with_no_work_expected` asserts 1 call + `Parked{NoWorkExpected, "Planning", descendants=false, streak 0}` and re-asserts stability (still 1 call) after 400ms — the question-wait contract is pinned.
- Unattended Planning auto-continues under the bound: `plain_prompt_clears_unattended_mode` phase 2 drives the chain to the plateau — `BudgetExhausted` park at cap = MAX_AUTO_CONTINUE+1 = 13 calls. BudgetExhausted is the manual-input park reason that banners in the InputBar (pre-existing Parked-evidence UI, backlog 5c33e945 / merged a4a0d18), so an unattended stall stays visible.

**2. Re-derivation semantics — PASS.**
- Prompt arm (agent.rs:694): `self.unattended = text.starts_with(UNATTENDED_PROMPT_PREFIX)` — plain assignment, so a plain user prompt (takeover) clears it. The `plain_prompt_clears_unattended_mode` test pins the full chain: unattended plateau → plain prompt while IDLE (the test's phase-3 comment correctly explains why a mid-chain prompt would be folded as a steer and bypass the Prompt arm) → cap+1 calls → `NoWorkExpected` park with streak 0, stable.
- Suggestion arm (agent.rs:770-832): resets `auto_continue_streak = 0` only — does NOT touch `unattended`. A child-completion Suggestion mid-run-all keeps unattended=true (machine progress, not user presence). Mid-turn steers (the `StopReason::Steer` arm) also never touch the flag — correct, a steer is guidance and the run is still unattended.

**3. Prefix coupling — PASS.**
- Single source of truth: `UNATTENDED_PROMPT_PREFIX` (channels.rs:139, core crate) is the only literal; all 12 usage sites verified — run_all.rs imports it for `run_all_prompt` (:251) and the prefix test (:451); agent.rs imports it for the Prompt arm (:694) and the two test prompt builders (:6067, :6245). No duplicated string anywhere.
- Byte-identical rendering: old = `RUN_ALL_STEER` (began `"[backlog run-all — unattended mode] You are …"`) + `\n\n---\n\n` + item_text; new = `UNATTENDED_PROMPT_PREFIX` + `" "` + `RUN_ALL_STEER` (now begins `"You are …"`, continuation text unchanged in the diff) + `\n\n---\n\n` + item_text. Concatenation reproduces the old bytes exactly, including the em-dash and single separating space.
- `run_all_prompt` has exactly one production caller (`run_all_dispatch_next`, run_all.rs:1868). The single-dispatch path (`dispatch_item`, backlog_cmds.rs:401-407) deliberately sends raw `item.text` with NO preamble — the module doc (backlog_cmds.rs:29-36) defines auto-feed/manual dispatch as "the user is watching" mode, only Run-All embeds the preamble. The flag's reach therefore exactly matches the preamble's reach — coherent, no gap.

**4. No missed callers — PASS.** Graph + literal search: the only production caller of `workflow_expects_progress` is `run_turn_with_retry` (agent.rs:331, passing `self.unattended`); the only other reference is the factory.rs Subagent-state test (:2209, passing `false` with a justifying comment — subagents are never run-all dispatched). No other call sites exist.

**5. Bug-plan checks — PASS.**
- Regression test `unattended_planning_auto_continues` exercises the changed path (fresh Planning workflow — asserted in setup — + prefix-marked prompt → calls >= 2). Logically red pre-fix: the old zero-arg gate returned false for Planning, so the None arm parked after 1 call and the 10s deadline would panic. Plan step 1 is marked done with "confirm it FAILS".
- Root cause documented: `.coding/knowledge/bug/2027-01-07-premature-planning-turn-ends-park-unattended-run.md` (symptom → root cause → fix + regression test name) plus the plan file's exploration context. BUG memory written (id 04d9f7b1, confirmed in the memory index).
- Plan file 3ddf7041.md: kind bug_fixing, bug param present, 4/4 steps. Backlog dispatch stamp present (a6a7727a → in_flight, plan_id 3ddf7041).

**6. Constitution — PASS.**
- Doc-sync: doc comments updated at every touch point (channels.rs const doc; agent.rs field doc, EXCEPTION 3 block, Prompt-arm comment, unattended-note comment; loop_impl.rs method doc; run_all.rs merged docs; factory.rs test comment). The HOW record file is updated (diff) and memory row 71611004 exists. PLAN.md:519's Subagent-state mention ("`workflow_expects_progress` is false for it") remains true — unaffected. README's backlog section documents dispatch/resolution, not auto-continue internals — the internal-mechanism → doc-comments-only precedent (bbd712c9) applies; no stale text found.
- Multi-platform neutrality: pure Rust logic — no cfg(windows), no paths, no platform APIs.
- Warning-free: no `#[allow]` added; `deny(warnings)` at both crate roots means the reported green gates (root 2082 passed / 0 failed; src-tauri 245+4+2 passed / 0 failed) prove zero warnings.

**7. Security/edge — assessed.** See findings. `starts_with` means mid-text mentions or quotes of the prefix never misfire; the correction-detection hook and `build_user_content` run on the raw text unchanged; the prefix rides visibly in the transcript exactly as before (byte-identical rendering).

## Findings

### LOW 1 — the verbatim-prefix edge is acceptable but NOT actually documented anywhere

A user interactively typing `[backlog run-all — unattended mode] …` as their own prompt gets unattended semantics (Planning auto-continue + the "no user input is coming" note). Assessment on merit: **acceptable** — (a) the semantics match what the user literally typed (the preamble's own body says "You are running unattended"); (b) the effect is bounded (MAX_AUTO_CONTINUE=12 → BudgetExhausted park, which banners in the InputBar); (c) no security boundary is crossed — the flag only widens the auto-continue gate and changes the note text, no approval bypass, no tool-visibility change. However, the review brief called this a "documented edge" and it is not: the `unattended` field doc, the channels.rs const doc, and the BUG record all describe the mechanism but none states that a user-typed prefix verbatim produces the same mode. Fix: one sentence in the `unattended` field doc (agent.rs:56-67), e.g. "A user interactively typing the prefix verbatim gets unattended semantics too — accepted: the text itself says unattended, and the effect is bounded + bannered."

### LOW 2 — a Prompt buffered during /compact summarization bypasses the Prompt arm, so it re-derives neither the streak (pre-existing) nor `unattended` (new, same site)

`compact_context`'s buffered-command re-injection (agent.rs:1078-1086) pushes a Prompt that arrived during the summarization LLM call as a plain user message — it never passes through the `run` Prompt arm, so it does not reset `auto_continue_streak` (a pre-existing gap at this site) and does not re-derive `unattended` (the new field inherits the same gap). Narrow window: the prompt must land exactly during a compaction's summarization call of a previously-unattended session; worst case, a user takeover during a mid-turn compaction leaves `unattended=true`, so a subsequent Planning question-wait could be self-answered. This mirrors the pre-existing streak behavior at the identical site — same class, not a regression introduced by this change — but the new flag slightly widens its consequence. Fix (optional, small): in the buffered-Prompt arm of `compact_context`, also assign `self.unattended = text.starts_with(UNATTENDED_PROMPT_PREFIX)` (and reset the streak) before pushing the message — or leave as documented-known-edge with a comment at the re-injection site.

## Test-quality notes (no action required)

All three new tests follow the established `CountingMockProvider` idiom exactly (dual-handle `calls` Arc, fanin drained every loop iteration, deadline loops, cleanup `select!` racing the handle against a 5ms drain — the deadlock-prevention pattern from the HOW record). The phase-3 "send while IDLE" reasoning in `plain_prompt_clears_unattended_mode` is correct and well-commented. The unattended continue note's Rust line-continuations collapse to the intended single-line string. No flakiness risks spotted beyond the idiom's standard 10s deadlines.
