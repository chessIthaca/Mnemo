+++
title = "Steered images carried through the steer pipeline (text+images end-to-end)"
created = "2027-01-07"
+++

Symptom: Adding an image to a steering message (interjecting while the agent runs) drops the image — only the text reaches the model. The steer pipeline is text-only end-to-end: InputBar's steer arm (frontend/src/components/layout/InputBar.tsx handleSend ~:301-316) drops attachedImages; sendSuggestion/AgentCommand::Suggestion(String) carry only text; StopReason::Steer(Vec<String>) + fold accumulate bare strings (also dropping images from mid-stream Prompts); and the injection sites (src/runtime/agent.rs run_turn_with_retry :186-208, drive_turns Compact arm :309-330, between-turns Suggestion arm :690-726) push Message::user_text(steer) — no image blocks, no vision fallback. Expected: the steer lands as a user message WITH image blocks exactly like a normal prompt's images (multimodal path / vision-model fallback) and stays in conversation context for subsequent turns. · regression test: midturn_image_input_injects_user_message_with_image_block

Full record for plan dabfe373 (see .coding/plans/dabfe373.md for the plan file).

regression test: midturn_image_input_injects_user_message_with_image_block · path .coding/plans/dabfe373.md · branch wt/agenticcoding @ b1f9c72 (unmerged — exists only on this branch)
