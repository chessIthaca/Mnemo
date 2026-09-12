# Review: ask_user question system + prompt history + steer soft-stop + plan step header

**Date:** 2026-05-20
**Scope:** All uncommitted changes in the working tree (`git diff HEAD` + untracked files).
**Reviewer:** background subagent (read-only)

The change set implements four features in one plan:
1. `ask_user` question system (Rust tool + dispatch interception + IPC `PendingQuestions` + Tauri `answer_question` command + frontend `QuestionPrompt`).
2. Prompt history (up/down) in `InputBar` — `promptHistory.ts` singleton.
3. Steer soft-stop: a mid-turn `Suggestion` now soft-stops the turn at the next break point (keeping partial output) and carries the steer as `TurnOutcome.pending_steer`, which `AgentTask::run_turn_with_retry` runs as a follow-up user message + turn.
4. Plan step `header: Option<String>` (bold `**Header**` extraction) shown in the StatusBar toolbar + PlanProgress.

---

## Summary

The implementation is solid and well-tested. The `ask_user` pause/resume correctly mirrors the approval gate; the steer soft-stop is correct and tested end-to-end; the header extraction is backward-compatible. I found **one minor bug** (a dropped tool-call batch on soft-stop is actually NOT dropped — verified correct), **one low-severity correctness nit** (soft-stop during a multi-call batch delays the stop until the whole batch finishes, which is acceptable but undocumented in one spot), **one dead-code warning risk**, and **a few minor robustness/consistency notes**. No security issues. No constitution violations.

---

## Findings by severity

### Correctness

**C1 (low) — Soft-stop during a tool-call batch runs the *entire* batch before stopping (by design, but the mid-batch steer is delayed).**
`src/agent/turn.rs:725-881`. When a steer arrives during tool call #1 of a 3-call batch, the `for tc in &tool_calls` loop continues through calls #2 and #3 (each `execute_tool_call` buffers the steer via its `cmd_rx` select, setting `soft_stop=true`), and only after the loop does `if soft_stop` (turn.rs:869) return. This is **correct** (no calls are dropped — verified by reading the loop), and matches the comment "the batch just completed". However, a steer arriving mid-batch is delayed until the whole batch finishes, which for a long batch (e.g. 3 file writes) could be several seconds. This is an acceptable trade-off (interrupting mid-batch would leave partial side effects), and the existing `Interrupt` path is the escape hatch. **No change required** — noting for completeness. The mid-stream soft-stop (turn.rs:538) and the post-batch soft-stop (turn.rs:869) both correctly preserve partial output and carry `pending_steer`.

**C2 (low) — `pending_steer` follow-up turn does not emit a `Started` event before the follow-up.**
`src/runtime/agent.rs:65-87`. When `run_turn_with_retry` loops on a `pending_steer`, it pushes the steer as a user message and calls `run_turn_attempt` again. `run_turn` emits `Started` at its top (turn.rs:84-86), so the follow-up turn *does* emit `Started`. The frontend `reduceStarted` clears `pendingQuestion` (agentEventReducer.ts:103) — but a pending question can't coexist with a steer soft-stop (the turn is blocked on the oneshot, not streaming), so this is fine. **No issue** — verified the `Started` event fires for the follow-up turn.

**C3 (verified OK) — `into_serializable` return-type change (2-tuple → `SerializedEvent` struct).**
`src/runtime/channels.rs:280-460`. All callers updated: `events.rs:176-180`, `tests/ipc_bridge.rs` (5 sites), `channels.rs` internal tests (6 sites). The struct destructuring `SerializedEvent { event, approval_sender, question_sender }` is used consistently. No caller was missed. **No issue.**

**C4 (verified OK) — One-question-at-a-time enforcement.**
`src/agent/dispatch.rs:255-343`. The `ask_user` helper blocks on the oneshot (`answer_rx`) inside a `tokio::select!` loop until the user answers. Since `run_turn` is single-threaded per agent (it awaits each `execute_tool_call` sequentially in the `for tc in &tool_calls` loop), a second `ask_user` cannot run until the first resolves. **Correct.**

**C5 (verified OK) — Steer soft-stop keeps partial output.**
`src/agent/turn.rs:538-559`. The mid-stream soft-stop pushes the accumulated `text` as an assistant message (turn.rs:539-547) before returning `TurnOutcome { text, pending_steer }`. The test `mid_turn_steer_soft_stops_and_carries_pending_steer` (tests.rs:2040+) asserts `outcome.text == "partial"`. **Correct + tested.**

**C6 (verified OK) — No infinite loop / no dropped steer in `run_turn_with_retry`.**
`src/runtime/agent.rs:65-87`. The loop breaks on `None` (final provider failure) or `pending_steer == None` (normal turn end). A `Some(steer)` pushes the user message and loops again. A chain of steers each soft-stops + resumes (the comment documents this). The steer is never dropped: it's always pushed as a user message before the next attempt. **Correct.**

### Bugs

**B1 (low) — `isNavigating()` is exported but unused (dead code → `dead_code` lint may fire under strict configs).**
`frontend/src/lib/promptHistory.ts:113-116`. `isNavigating` is exported but has no caller in the codebase (only `historyLength` is used, and only in tests). Unlike `PendingApprovals::len`/`is_empty` which carry `#[allow(dead_code)]` (approval.rs:53,59), this TS export has no such suppression. Under TypeScript this is harmless (no lint failure), but it's dead code. **Suggestion:** either use it in `InputBar` (e.g. to show a history-position indicator) or remove it. Low priority.

**B2 (low) — `pushPrompt` does not trim; the local `trimmed` variable is misleadingly named.**
`frontend/src/lib/promptHistory.ts:48-64`. `const trimmed = prompt;` — the variable is named `trimmed` but no trimming occurs (the comment says "dedupe consecutive duplicates"). An empty/whitespace-only prompt is rejected by `trimmed.length === 0` only when truly empty (`""`); a whitespace-only prompt (`"   "`) passes the guard and is stored. Since `InputBar.handleSend` already guards `if (!text.trim() ...)` before calling `pushPrompt` (InputBar.tsx:169), this is unreachable in practice, but the guard is weaker than intended and the variable name is misleading. **Suggestion:** rename to `entry` and/or guard on `prompt.trim().length === 0`. Low priority.

**B3 (low) — `extract_bold_header` matches the first `**...**` anywhere after leading whitespace, including mid-sentence.**
`src/workflow/plan_file.rs:319-330`. `extract_bold_header_impl` does `trim_start()` then `strip_prefix("**")` then `find("**")`. For a step like `"Use **bold** for emphasis"` it would extract `Some("bold")` — a false positive (the step doesn't *start* with a header, it just contains bold). The intent (per the doc comment + the `create_plan` schema description) is that a header is a leading `**Header**` punchline. A step authored as `**Header** — body` is the intended form; a step that merely *contains* bold later is not. **Impact:** low — the toolbar would show a misleading "header" for such a step, but the full text is still shown in PlanProgress. The `bold_header_extracted_from_step` test only covers the intended form. **Suggestion:** require the closing `**` to be followed by whitespace, `—`, `:`, `-`, or end-of-string (i.e. the bold run is a leading token, not mid-sentence). Optional hardening.

**B4 (low) — PlanProgress body-strip regex assumes the `**Header**` is followed by ` — ` / `-` / `:` separator; a header-only step (`**Header**` with no body) leaves the regex unmatched and shows the full `**Header**` as the body.**
`frontend/src/components/views/PlanProgress.tsx:178`. `{step.text.replace(/^\*\*[^*]+\*\*\s*[-—:]?\s*/, "")}`. For a step `"**Header**"` (no body), the regex matches `**Header**` + trailing whitespace, leaving `""` — so the body div renders empty. That's acceptable (empty body), but the `[^*]+` won't match a header containing `*` (e.g. `**C:\foo\bar**` is fine, but `**a*b**` would fail to match and show the raw `**a*b**` as the body). Edge case; the `extract_bold_header` Rust side has the same `[^*]`-equivalent behavior via `find("**")` (it finds the *next* `**`, so `**a*b**` → header `"a*b"` works on the Rust side but the TS regex `[^*]+` does not). **Inconsistency:** the Rust extractor handles `**a*b**` (header `"a*b"`) but the TS body-stripper does not strip it. Low impact (headers rarely contain `*`). **Suggestion:** align the TS regex with the Rust logic, or document the assumption.

**B5 (verified OK) — Lock ordering: `PendingQuestions` uses `std::sync::Mutex`, same as `PendingApprovals`.**
`src-tauri/src/ipc/questions.rs:15,35`. Both pending maps use `std::sync::Mutex` (not `tokio::sync::Mutex`). The `answer_question` command (agent.rs:226-233) locks only `state.questions`; the event forwarder locks `pending_questions` independently. There's no nested locking across `questions` + `approvals` + the async `AgentManager` `tokio::Mutex` in a single code path — `resolve`/`insert`/`cleanup_for_agent` each take only the one `std::sync::Mutex` and release it before any async work. **No deadlock risk.** The `std::sync::Mutex` is held only for trivial HashMap ops (never across `.await`), matching the approval twin. **Correct.**

### Security

**S1 (verified OK) — `answer_question` resolves by `question_id` only (no agent id).**
`src-tauri/src/ipc/agent.rs:218-233` + `questions.rs:72-86`. This mirrors `approve` (agent.rs:200-214 + approval.rs:94-108), which resolves by `tool_call_id` only. A `question_id` is the tool-call id, which is globally unique per `ask_user` call (the provider/gateway assigns unique call ids). `resolve` scans for the single matching key regardless of agent — safe because the id is unguessable and single-use. **No issue** — consistent with the existing approval security model.

**S2 (verified OK) — No secret leakage in question/options.**
The `ask_user` tool carries only `question` + `options[{label, description}]` — all user-authored agent content, no secrets, file paths, or tool outputs. The `UserAnswer` (Choice index / Freeform text) is user input. **No issue.**

**S3 (verified OK) — `ask_user` is `SafetyLevel::AutoRun` and never approval-gated.**
`src/tool/workflow/ask_user.rs:126-129`. Asking a question is not a mutation, so it's correctly never approval-gated, and `ToolFilter` exposes it in every state (Complete/Planning/Executing/Skill) via `name == "ask_user"` (tool/mod.rs:194,212,234,240). A skill's allow-list does NOT gate it (tool/mod.rs:240: `name == "ask_user" || allowed.iter().any(...)`). **Correct** — the agent can always clarify with the user.

### Constitution compliance

**CC1 (verified OK) — Doc comments on all new public functions/types.**
- `QuestionOption`, `UserAnswer`, `SerializedEvent`, `AgentEvent::UserQuestion`, `SerializableAgentEvent::UserQuestion` — all have doc comments (channels.rs:25-34, 467-489, 281-291, 153-170, 258-265).
- `PendingQuestions` + its methods (`new`, `len`, `is_empty`, `insert`, `resolve`, `cleanup_for_agent`) — all documented (questions.rs:33-99).
- `answer_question` Tauri command — documented (agent.rs:217-225).
- `AskUserTool`, `parse_ask_user_args`, `AskUserArgs`/`AskUserOption` fields — documented (ask_user.rs:55-66, 148-155, 36-53).
- `Step.header` field + `extract_bold_header`/`extract_bold_header_pub` — documented (plan_file.rs:30-44, 299-318).
- `TurnOutcome.pending_steer` — documented (loop_impl.rs:129-134).
- `run_turn_with_retry`/`run_turn_attempt` — documented (agent.rs:45-59, 90-91).
- Frontend: `QuestionOption`, `PendingQuestion`, `UserAnswer`, `answerQuestion`, `reduceUserQuestion` — all have JSDoc (agentState.ts:34-50, tauri.ts:59-77, agentEventReducer.ts:361-364).
**Compliant.**

**CC2 (verified OK) — No commits to main.**
The diff is uncommitted working-tree changes; no `git commit`/`merge` to main occurred in this change set. The plan's final step (step 13) instructs committing to the feature branch. **Compliant** (pending the commit step).

**CC3 (cannot verify) — `cargo test` run before marking complete.**
This is the review step (step 13); the test run is the agent's responsibility, not observable in the diff. The new tests are present: `mid_turn_steer_soft_stops_and_carries_pending_steer`, `ask_user_two_questions_round_trip`, `user_question_into_serializable_extracts_sender`, `user_question_roundtrips_json`, `user_answer_serde`, `bold_header_extracted_from_step`, `bold_header_extracted_from_parsed_checklist`, `bold_header_round_trips_through_serialize`, `empty_bold_header_is_none`, plus `PendingQuestions` tests + frontend `promptHistory.test.ts` + `ipc-contract.test.ts` user_question fixture + `useAgentStore.test.ts` user_question reducer test. **Test coverage is strong.**

**CC4 (verified OK) — Windows paths / PowerShell syntax.**
No shell commands in the diff. The project constitution's Windows-path rule applies to the agent's shell invocations, not to source. **N/A in diff.**

**CC5 (verified OK) — Line-ending preservation.**
The `file_edit`/`file_write`/`file_append` tools normalize line endings automatically. The diff shows consistent `\n`-style content; no mixed endings introduced. **Compliant.**

### Test gaps

**T1 (low) — The steer soft-stop is tested at the `run_turn` level (turn driver) but NOT end-to-end through `AgentTask::run` (the `run_turn_with_retry` follow-up loop).**
`src/agent/tests.rs:2040-2110` tests `run_turn` directly: it asserts `outcome.pending_steer == Some("do this instead")` and `outcome.text == "partial"`. But the `run_turn_with_retry` loop (agent.rs:65-87) that *consumes* `pending_steer` and runs the follow-up turn is not tested with a mid-turn steer. The existing `between_turn_steer_is_converted_to_user_message_and_runs_turn` test (agent.rs:714-809) covers the *between-turn* Suggestion path, not the *mid-turn* soft-stop → follow-up path. **Suggestion:** add an `AgentTask::run`-level test: send a `Prompt`, send a `Suggestion` mid-stream (via a pausing provider), assert (a) the first turn soft-stops with partial text, (b) a `SuggestionInjected` fires, (c) the steer appears as a user message, (d) a second `Finished` fires (the follow-up turn ran). This would close the loop on the "no infinite loop, no dropped steer" guarantee at the integration level. Medium value.

**T2 (low) — The `ask_user` interrupt path (`Interrupt`/`Cancel` while awaiting the answer) is not tested.**
`src/agent/dispatch.rs:322-340`. The `ask_user` helper handles `Interrupt`/`Cancel` (returns an error result) and `None` (channel closed), but no test exercises these branches. The approval twin (`await_approval_interrupted`, `await_approval_channel_closed`) is tested. **Suggestion:** add a test that sends an `Interrupt` while `ask_user` is awaiting and asserts the error result. Low priority (the logic mirrors the tested approval path).

**T3 (verified OK) — The two-questions example is tested.**
`src/agent/tests.rs:2128-2307` (`ask_user_two_questions_round_trip`) tests Q1 (choice → label) + Q2 (freeform → text) sequentially, asserting one-question-at-a-time (the second `ask_user` only runs after the first resolves). **Covers the integration-test requirement.**

**T4 (verified OK) — Contract fixture locks the wire shape.**
`src-tauri/src/ipc/contract_fixtures.rs:96-117` + `frontend/src/lib/ipc-fixtures/event-user-question.json` + `ipc-contract.test.ts:208-220`. The `SerializableAgentEvent::UserQuestion` shape (question_id + question + options[{label, description?}]) is locked on both sides. **Compliant.**

---

## Conclusion

The change set is **correct, secure, and well-tested**. The four features are implemented consistently with the existing patterns (approval gate, between-turn steer, plan-file parsing). The `SerializedEvent` refactor is clean and all callers updated. The steer soft-stop correctly preserves partial output and carries the steer for a follow-up turn with no infinite-loop or dropped-steer risk.

**Actionable items (all low severity, none blocking):**
- B1: remove or use `isNavigating()` (dead code).
- B2: rename `trimmed` / strengthen the empty guard in `pushPrompt`.
- B3/B4: harden `extract_bold_header` + align the TS body-strip regex (false-positive on mid-sentence bold; `*` in header).
- T1: add an `AgentTask::run`-level test for the mid-turn steer → follow-up turn path.
- T2: add a test for the `ask_user` interrupt branch.

No findings require a fix before commit. The above are quality hardening suggestions.
