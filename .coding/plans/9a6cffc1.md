# Plan: Workers that keep re-reporting "done" get their turn ended

## Goal
A worker that has reported to the lead and then keeps calling message_parent without doing anything else no longer floods the lead with copies: the repeats are not forwarded, the worker is told to end its turn, and if it still persists its turn is ended for it, which produces the normal "finished its turn" notice.

## Kind
implementation

## Context
WHY: on 2026-11-15 a local qwen3-coder:30b worker finished its task, then sent three near-identical "Task complete" message_parent reports in a row instead of ending its turn. Each became a lead notice, and the lead had to team_stop the worker before it could review (the review gate needs an idle worker). The existing strike counter only catches verbatim repeats and failures, and the texts differed slightly.
WHERE: src-tauri/src/mnemo2/team/pump.rs. worker_host_tool_result (around line 57) answers message_parent by pushing a TeamMessage and returning "message sent to the lead…continue your task or end your turn."; pump_worker's event loop (around line 345) calls it for HostToolCall events and already keeps per-worker loop state (call_ids, pending_calls, the strike counter). Lead notices for reports come from runtime.rs (around line 1396, `{name} reports: {text}`), driven by the HostToolCall event itself.
RULES:
- "Reports in a row" counts message_parent calls with no other tool call in between, within one worker turn. Any other tool call (read, edit, shell, git, plan_read…) resets it to zero; so does a new turn.
- 1st report: unchanged behaviour (forwarded, lead notice, same result text).
- 2nd report in a row: not recorded as a TeamMessage and must not produce a lead notice; the tool result tells the worker its report already reached the lead and to end its turn now with no further tool calls.
- 3rd report in a row: same refusal, and the host ends the worker's turn (SessionCommand::Interrupt is the available command; verify that an interrupted turn reaches the same Finished path that sends "finished its turn" and leaves the worker idle for review; if it does not, find the right mechanism and say so in your report).
- Audit workers (kind "audit") follow the same rule; their first report still sets report_delivered.
- Memory tools (memory_write etc.) between reports count as "other tool calls"? NO: a worker that only writes memories between reports is still repeating itself. Treat memory_* and message_parent as non-resetting; everything else resets.
CONTRACT: the suppression must happen before the runtime turns the HostToolCall into a lead notice, or the runtime must skip notices for suppressed calls; choose the smaller change and keep a single source of truth for "suppressed".
TESTS (in the existing pump/team test module, using its fakes): `second_report_in_a_row_is_not_forwarded`, `other_tool_call_resets_the_report_streak`, `third_report_in_a_row_ends_the_turn` (asserts an Interrupt was sent and the worker ends up idle/finished), `memory_writes_do_not_reset_the_report_streak`, `first_report_is_unchanged`.
VERIFY: `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::`, `cargo clippy -p mnemo-app --all-targets --features mnemo2`, `git diff --check`.

## Acceptance scenarios
- A worker that sends a report and then two more message_parent calls in a row produces exactly one lead 'reports:' notice, and then a 'finished its turn' notice.
- A worker that reports, then reads a file, then reports again produces two report notices.

## Steps
- [x] 1. **Report streak in the worker pump** Files: src-tauri/src/mnemo2/team/pump.rs, src-tauri/src/mnemo2/runtime.rs only if the notice suppression must live there, and the team test module that already tests pump behaviour (src-tauri/src/mnemo2/team/tests.rs or the pump's own tests). Implement the RULES and CONTRACT in the plan context with the five named tests. Keep the first report's result text byte-identical. The refusal text must name what to do next (end the turn with no tool calls). Done when the VERIFY commands pass and the acceptance scenarios hold in the tests.

## Decisions
- 2026-11-15: no spec impact: Worker host-tool behaviour in pump.rs; no OpenSpec capability governs it.

## Progress
- 2026-11-15 step 1 done: step 1 merged: reportStreak01 (reportStreak01) on glm-5.3, merge commit d7a122b0; review: Report streak in the pump: first report unchanged, repeats refused and never forwarded, third repeat interrupts the turn, which finishes normally; plan_read and other tools reset, memory_* do not. Refusal text reviewed as a prompt: clear and actionable.
