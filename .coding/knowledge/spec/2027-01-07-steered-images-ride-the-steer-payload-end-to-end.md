+++
title = "Steered images ride the steer payload end-to-end (text + images)"
created = "2027-01-07"
+++

SPEC: A steering message (mid-work guidance sent while the agent runs) carries its pasted image attachments end-to-end and delivers them to the model exactly like a normal prompt's images (user report 2027-01-07: "steering with an image drops the image" — the pipeline used to be text-only).

Pipeline (widened from bare Strings to payloads): InputBar steer branch passes `images` → sendSuggestion(agentId, text, images) → send_suggestion IPC → AgentCommand::Suggestion(SteerPayload { text, images }) (src/runtime/channels.rs) → StopReason::fold accumulates payloads into Steer/InterruptWithSteers/CompactWithSteers(Vec<SteerPayload>) (a mid-stream Prompt folds WITH its images too — same bug class, fixed by the same arm) → injection sites build user messages via AgentLoop::build_user_content (loop_impl.rs) — the ONE image path shared with the normal prompt arm: multimodal → MessageContent::Parts (text + image_url blocks); text-only + vision client → VisionDescribe/describe/VisionDescribed per image with descriptions folded into the text; else multipart (provider strips). Injection sites: run_turn_with_retry's Steer arm, drive_turns' Compact arm, the between-turns Suggestion arm (all user messages), and the three summarization/model-switch re-injection paths (text-only steer → system message, unchanged; image-bearing steer → user message with image blocks).

Events/UI: SuggestionInjected { text, images } carries the images → reduceSuggestionInjected pushes a `steer` transcript entry with images (rendered as thumbnails in Message.tsx, amber styling); the pending-steer bubble shows an ImageIcon + count; click-to-edit loads the steer's images back into the attachment tray. capTranscriptImages covers steer entries (same 32 MiB rolling budget, imagesEvicted chip). Paste-time downscale (mem-perf LOW 4, spec 2027-01-07-pasted-images-are-downscaled-on-attach-transcrip) applies automatically — steered images go through the same attach path (fileToAttachedDataUrl).

Cancel stays TEXT-match based (documented limitation, spec 2026-08-30-cancel-delete-a-scheduled-steer-via-the-x-on-eac): two steers with identical text cancel together even if their images differ.

Tests: midturn_image_input_injects_user_message_with_image_block + steer_with_image_injects_user_message_with_image_block + non_multimodal_with_vision_describes_steered_image (src/runtime/agent.rs), fold_carries_images_with_steers_in_order + fold_prompt_mid_stream_carries_images + fold_cancel_by_text_still_drops_image_bearing_steer (src/agent/loop_impl.rs), suggestion_injected_roundtrips_json (channels.rs), useAgentStore.test.ts suggestion_injected images test.
