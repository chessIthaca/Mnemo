+++
title = "Steered images dropped — text-only steer pipeline (fold/injection discard images)"
created = "2027-01-07"
+++

BUG (user report 2027-01-07): steering with an image drops it — only text reaches the model. Root cause: the steer pipeline is text-only end-to-end — InputBar handleSend's steer arm drops attachedImages; sendSuggestion/AgentCommand::Suggestion(String) carry only text; StopReason::Steer/InterruptWithSteers/CompactWithSteers(Vec<String>) + fold accumulate bare strings (also dropping mid-stream Prompt images); injection sites (run_turn_with_retry :186-208, drive_turns Compact arm :309-330, between-turns Suggestion arm :690-726) push Message::user_text — no image blocks, no vision fallback. Fix: SteerPayload{text,images} carried end-to-end; fold accumulates payloads; injection sites build user messages via the Prompt arm's image handling (multimodal Parts / vision fallback). Regression test: midturn_image_input_injects_user_message_with_image_block (src/runtime/agent.rs tests, fails pre-fix: injected steer = Text only).
