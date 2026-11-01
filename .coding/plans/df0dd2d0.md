# Plan: Reopened Mnemo2 lead answers new prompts

## Goal
A lead session restored from `.coding/mnemo2/lead.json` processes a new prompt exactly like a fresh one (LLM request, streamed events, persisted transcript), with a regression test that fails today and passes after the fix.

## Kind
bug_fixing

## Context
WHY: Mnemo2 now keeps running across Claude Desktop restarts and reopens projects more often, so a lead that goes silent after reopen leaves anyone using Mnemo2's own lead stuck until they discard the conversation. Knowledge record: `.coding/knowledge/bug/2027-01-11-mnemo2-reopened-lead-session-ignores-new-prompts.md` (it reports a text-only ScriptedLlm experiment: open, prompt, wait_for_persisted, shutdown, reopen, prompt; the second drain_turn timed out and the session's command queue was never consumed). It is pre-existing and independent of usage/cost work.

WHERE (start here, not a full list):
- verified: `grep -nE` at plan time -> src-tauri/src/mnemo2/runtime.rs:44 `const LEAD_STATE_FILE: &str = "lead.json"`, :290 `async fn open_inner(`, :297 `pub(crate) async fn open_inner_with_policy(`, :634 `pub async fn send_prompt(`, :890 `async fn start_engine(`, :981 `async fn spawn_live(`, :1334 `async fn pump_events(`; src/headless/engine.rs:363 `initial_messages: opts.resume.map(|transcript| {`; src-tauri/src/mnemo2/team.rs:2453 `pub(crate) async fn spawn_worker_session(`.
- verified: `grep -nE` at plan time -> test helpers in runtime.rs :1638 `struct ScriptedLlm`, :1820 `fn text_turn`, :1899 `fn write_config`, :1923 `fn options(`, :2243 `async fn drain_turn`, :2270 `async fn wait_for_persisted`, :2323 `async fn restores_the_saved_transcript_and_model_on_reopen`.
- verified: reading runtime.rs:1002-1015 -> the lead session is spawned with `SpawnOptions { resume: Some(resume), policy: self.policy.clone(), host_tools (team lead tools), permissions: None, guardrails: None, worktree: None }`; reading runtime.rs:677-680 -> an idle `send_prompt` sends `SessionCommand::Prompt { text, images }`.
- verified: reading src/headless/engine.rs:359-365 -> `SpawnOptions.resume` becomes `initial_messages` after `crate::agent::prompt::strip_request_scaffolding`.
- verified: reading team.rs:2462-2475 -> worker sessions (whose resume works) spawn with `policy: None`, `permissions: Some(PermissionPolicy::OverridePermissions)`, `worktree: Some(..)`, worker host tools and `guardrails: Some(guardrails_for(kind))`.
- verified: reading runtime.rs:911-916 and :954-966 -> the lead's engine uses `PermissionPolicy::ApproveViaApi` and registers the `mnemo.workflow` policy (`WorkflowPolicyFactory::with_team_lead`).
- assumption: those lead-versus-worker differences, and whatever the restored transcript ends with (a trailing message, a pending interaction or tool call, restored busy or hold state), are the first suspects; none is confirmed as the cause.
- verified: reading runtime.rs:2323-2356 -> `restores_the_saved_transcript_and_model_on_reopen` only asserts the restored transcript and model and never prompts after reopen, which is why CI misses this.

REGRESSION TEST: add `a_reopened_lead_answers_a_new_prompt` to the test module of `src-tauri/src/mnemo2/runtime.rs`. It opens a lead with a ScriptedLlm (one text turn), prompts, drains the turn, waits for lead.json, shuts down, reopens on the same state dir with a NEW ScriptedLlm holding one text turn (e.g. "after reopen"), sends a prompt, and asserts: the turn drains, that ScriptedLlm was called once, the streamed TextDelta text is "after reopen", the transcript holds the restored messages plus the new prompt and reply, and lead.json is saved again with the longer transcript. Without the fix it is expected to fail by timing out in drain_turn with the second ScriptedLlm never called; confirm that before fixing.

CONSTRAINTS / DECISIONS:
- Fix the root cause. No retries, timeouts, re-sends or "kick" prompts that paper over a stuck session, and do not drop the restored history to make it work.
- Keep the `lead.json` format readable: files written by the current release must still load and resume.
- Worker resume must keep working unchanged (team resume tests stay green).
- If the root cause is in the shared headless engine or agent kernel (`src/headless/` or `src/agent/`), fix it there, add a focused test at that level too, and say so in the report, because Mnemo1 shares that code.

VERIFY (run from the worktree root; Windows host):
- `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::runtime`
- `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::`
- If anything under `src/` (root crate) changed: `cargo test -p mnemo --lib headless` plus the tests of the module you touched.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests` must add no warnings in the files you touched.

## Acceptance scenarios
- `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::runtime::tests::a_reopened_lead_answers_a_new_prompt` passes; on the code before the fix it fails by timing out in drain_turn with the second ScriptedLlm never called.
- After reopen, a new prompt produces an LLM request, streamed TextDelta events with the scripted reply, an idle (not busy) snapshot afterwards, and a lead.json whose transcript holds the restored messages plus the new prompt and reply.
- A lead.json written before the fix still loads on reopen and the restored lead answers a prompt.
- Worker resume is unchanged: the whole `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::` suite passes, including the team resume tests.

## Steps
- [x] 1. **Regression test and root-cause fix** — add `a_reopened_lead_answers_a_new_prompt` to the tests in `src-tauri/src/mnemo2/runtime.rs`, confirm it fails on the current code, find why a resumed lead session never consumes `SessionCommand::Prompt` (`src-tauri/src/mnemo2/runtime.rs`, and if the cause lies there `src/headless/engine.rs`, `src/headless/types.rs` or the agent kernel under `src/agent/`), fix it at the root, and make the test pass with the existing suites green.

## Bug
After a project is reopened (app restart or project switch back), Mnemo2's in-app lead restores its saved transcript but ignores every new prompt: send_prompt returns Ok, yet no LLM request is made and no session events are emitted, so the lead looks frozen. Worker sessions resumed the same way accept messages normally.

## Regression test
a_reopened_lead_answers_a_new_prompt

## Progress
- 2026-10-31 step 1 done: step 1 merged: leadReopenFix31 (leadReopenFix31) on kimi-k3.1-pro, merge commit 6cf1e8f5; review: Root cause found and fixed in the headless engine: a resumed lead pins its saved model, and Engine::spawn's forced-model path replaced the builder-injected provider with a config-resolved client, so a reopened lead with an injected (scripted) provider never called it. The fix keeps the injected provider for model-pinned sessions. Production Mnemo2 opens the lead with no injected provider (LeadRuntime::open passes None), so the knowledge record's symptom was a harness artifact; the fix corrects the harness/embedder seam and adds regression coverage at both levels. Checks pass. No findings.
