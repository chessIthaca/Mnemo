# Plan: The team ledger records who sent each worker message

## Goal
Every message that reaches a worker is stored in the team ledger with its sender (Claude Code lead, built-in lead, the user in mission control, or Mnemo2 itself) and time. The worker receives it with a sender header, and mission control shows who sent what. Workers are told that a lead reply only ever arrives as such a message, so they stop acting on replies they imagined.

## Kind
implementation

## Context
WHY: worker skillsMemFlow33 (glm-5.3) made a "review fix" commit and kept asking for "two commands" nobody sent. The user asked: "lets make sure the ledger remembers who sends things." Investigation of its saved transcript in .coding/mnemo2/team.json: after a `message_parent` call returned "message sent to the lead", the model's next assistant turn said "The lead confirmed … and asked for one targeted reword". No user or steer message exists between them, so the model hallucinated a lead reply and acted on it. Today nothing in the ledger records who sent a worker message, and the live transcript is only saved when a turn ends, so this took transcript forensics to prove.

WHERE (verified by grep/reading):
- verified: src-tauri/src/mnemo2/team.rs:2525 `TeamRuntime::message(worker_id, text)` -> `message_inner` sends `SessionCommand::Prompt` (idle) or `SessionCommand::Steer` (busy). It has no sender.
- verified: callers of `message`/`message_inner`: team.rs:1607 (the `team_message` lead host tool, used by BOTH the built-in lead and the MCP external lead through the same handler), src-tauri/src/mnemo2/ipc.rs:826 (`team_message` tauri command = the user replying in mission control), team.rs:3214 (`message_inner(worker_id, brief)`, the route-fix / Mnemo2-generated brief), and runtime.rs tests (3332, 3385, 4133, 4743, 4755) plus team.rs:6143.
- verified: team.rs:346-355 `TeamMessage { worker_id, text, at_ms }` records worker-to-lead messages only. team.rs:4182-4204 handles the worker's `message_parent` and returns `ToolResult::success("message sent to the lead")` (asserted in runtime.rs:5884).
- verified: team.rs:143 `WorkerRecord.transcript` is persisted. team.rs:4622 copies the snapshot transcript at turn end; team.rs:4841 `without_transcripts` strips transcripts from some outputs.
- verified: worker guidance strings around team.rs:5053 ("use message_parent with the precise issue and wait for the lead").
- assumption: the MCP server (src-tauri/src/mnemo2/mcp_server.rs `call_tool`) reaches the team host tools through a path where the caller can be passed as the external lead. The worker confirms the exact path.

DECISIONS:
1. Add a sender enum, e.g. `MessageSender { ExternalLead, Lead, User, Mnemo }`, serialized in snake_case. `TeamRuntime::message` takes it explicitly. Every caller passes the true sender: MCP = ExternalLead, built-in lead host tool = Lead, ipc team_message = User, route-fix/brief = Mnemo.
2. Persist every inbound worker message immediately (not at turn end) on `WorkerRecord` as an inbox log: sender, kind (prompt/steer/brief/fix), text, at_ms. Keep old ledgers loading (serde default). Cap it sensibly (e.g. last 200 entries per worker). team_status returns it.
3. Prefix the text delivered to the worker with a one-line header naming the sender, e.g. "[From the lead (Claude Code)]", "[From the built-in lead]", "[From the user in mission control]", "[From Mnemo2]". The header lands in the worker's transcript too.
4. `message_parent` returns: "message sent to the lead. A reply, if any, arrives later as a new message that starts with a [From …] header; do not assume or invent a reply — continue your task or end your turn." Worker guidance states the same rule. Update runtime.rs:5884's assertion.
5. Mission control (worker detail) shows the inbox entries with sender and time. That is step 2, after step 1 merges.
6. Keep the existing worker-to-lead `TeamMessage` log; add the sender only where it is ambiguous.

VERIFY:
- cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::
- cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests (no new warnings in touched files)
- npm --prefix mnemo2 run check and npm --prefix mnemo2 test (step 2)
- Fresh worker worktrees need `npm ci --no-audit --no-fund` at the worktree root before the mnemo2 crate builds.

## Acceptance scenarios
- After Claude Code calls team_message, team_status for that worker shows an inbox entry with sender external_lead and the text, and .coding/mnemo2/team.json contains it before the worker's turn ends.
- A message typed in mission control is recorded with sender user; the built-in lead's team_message is recorded with sender lead; a route-fix brief is recorded with sender mnemo.
- The worker's transcript shows each received message starting with its [From …] header.
- message_parent's result tells the worker that replies arrive only as new [From …] messages and not to assume one.
- Mission control's worker detail lists the received messages with sender and time; `cargo test --features mnemo2 --lib mnemo2::` and the mnemo2 frontend tests pass.

## Steps
- [ ] 1. **Record and deliver the sender** — `src-tauri/src/mnemo2/team.rs` (MessageSender, inbox log on WorkerRecord persisted on every message, sender header on delivered text, `message_parent` result and worker guidance wording, the route-fix/brief caller passes Mnemo), `src-tauri/src/mnemo2/mcp_server.rs` (external lead calls pass ExternalLead), `src-tauri/src/mnemo2/ipc.rs` (mission control passes User), `src-tauri/src/mnemo2/runtime.rs` (built-in lead path passes Lead; update test callers and the message_parent assertion). Tests: each sender is recorded and persisted immediately, the header reaches the worker's session, old ledgers without the field load, the cap holds, message_parent's new result text.
- [ ] 2. **Show senders in mission control** — `mnemo2/src/lib/` team types plus the worker detail component (find it with graph_search, e.g. WorkerDetail.svelte): list the worker's inbox entries with a sender label and time, newest last, collapsed by default like the brief. Add or extend the frontend tests next to the touched components.

## Interfaces
- 2026-11-01: `src-tauri/src/mnemo2/team.rs` is also being changed by plan 763a911f step 1 (external-lead memory tracking, close-gate wiring, closed-notice reminder) and possibly by plan 53e56d1d (shutdown). This plan's step 1 touches only the worker-message path (`message`/`message_inner` and callers, `WorkerRecord` inbox field, `message_parent` handling, worker guidance strings). Do not refactor neighbouring code, so merges stay mechanical.
- 2026-11-01: Step 1 -> step 2 data shape: `WorkerRecord.inbox: Vec<InboxEntry>` serialized as `inbox: [{ sender: "external_lead"|"lead"|"user"|"mnemo", kind: "prompt"|"steer"|"brief"|"fix", text: string, at_ms: number }]` in team snapshots and team_status.
