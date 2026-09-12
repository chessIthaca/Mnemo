# Plan: Steered images carried through the steer pipeline (text+images end-to-end)

## Goal
A steering message with image attachments delivers the images to the model exactly like a normal prompt: Suggestion carries (text, images) end-to-end, the fold/queue accumulates payloads rather than bare Strings, and every injection site builds user messages with image blocks (multimodal multipart / vision-model fallback), with the images kept in conversation context for subsequent turns. CancelSuggestion's text-match keeps working. Regression tests: steer with image → injected user message carries the image block; cancel-by-text still works; fold order preserved; vision fallback fires for a non-multimodal model on a steered image.

## Kind
bug_fixing

## Context
Verified root-cause pointers (2026-01-07): frontend sendSuggestion (tauri.ts:51) → send_suggestion IPC (src-tauri/src/ipc/agent.rs:241) → AgentCommand::Suggestion(String) (channels.rs:59-93) → buffered mid-turn → StopReason::fold (loop_impl.rs:407-522, called from consume_stream select! turn.rs:2302/2312, between-tool-call drain turn.rs:1089/1169, approval re-injection turn.rs:2018-2029 via buffered_steers Vec<String> at turn.rs:351) → StopReason::Steer/InterruptWithSteers/CompactWithSteers(Vec<String>) → injection sites push Message::user_text. Normal path to mirror: Prompt arm (agent.rs:537-682) builds MessageContent::Parts (text + ImageUrl) for multimodal, vision fallback (VisionDescribe/describe_image_data_url/VisionDescribed, descriptions folded into text) for text-only + vision client, else multipart for provider strip. AgentEvent::SuggestionInjected { text } (channels.rs:187) + SerializableAgentEvent (:459, roundtrip test :1095) → frontend reduceSuggestionInjected (agentEventReducer.ts:1060) pushes { kind: "steer", text } transcript entry (types.ts:238 event type, TranscriptEntry steer variant ~:362, Message.tsx steer case ~:336). Frontend store: SteerEntry { id, text, status, timestamp } (agentState.ts:138), addSteer (useAgentStore.ts:956). CancelSuggestion is text-match based (documented limitation, SPEC 2026-08-30-cancel-delete-a-scheduled-steer) and must keep working. Existing tests to keep green: fold_accumulates_steers_in_order + fold tests (loop_impl.rs:1658+), drop_cancelled_steers tests, steer injection tests (agent.rs:2938/:3025), summarize_with_interrupt buffering (context.rs:1498/:1510), send_suggestion source-contract tests (ipc/agent.rs:921/:956), suggestion_injected_roundtrips_json (channels.rs:1095).

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
Adding an image to a steering message (interjecting while the agent runs) drops the image — only the text reaches the model. The steer pipeline is text-only end-to-end: InputBar's steer arm (frontend/src/components/layout/InputBar.tsx handleSend ~:301-316) drops attachedImages; sendSuggestion/AgentCommand::Suggestion(String) carry only text; StopReason::Steer(Vec<String>) + fold accumulate bare strings (also dropping images from mid-stream Prompts); and the injection sites (src/runtime/agent.rs run_turn_with_retry :186-208, drive_turns Compact arm :309-330, between-turns Suggestion arm :690-726) push Message::user_text(steer) — no image blocks, no vision fallback. Expected: the steer lands as a user message WITH image blocks exactly like a normal prompt's images (multimodal path / vision-model fallback) and stays in conversation context for subsequent turns.

## Regression test
midturn_image_input_injects_user_message_with_image_block
