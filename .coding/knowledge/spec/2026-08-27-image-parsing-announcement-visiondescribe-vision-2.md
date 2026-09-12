+++
title = "image-parsing announcement (VisionDescribe/VisionDescribed events + transcript card) — MERGED into main (9eaa25d3)"
supersedes = "2026-08-27-image-parsing-announcement-visiondescribe-vision"
created = "2026-08-27"
+++

SPEC: Image-parsing announcement for the text-only-provider vision fallback — MERGED into main at 9eaa25d3 (2026-08-27, merge_to_main skill), branch wt/agenticcoder deleted, not pushed. New event pair AgentEvent::VisionDescribe {index(1-based), total, query} / VisionDescribed {index, total, success, description} (src/runtime/channels.rs, kinds "vision_describe"/"vision_described"), emitted around each describe_image_data_url call in the Prompt-arm vision fallback (src/runtime/agent.rs) — the fallback runs BEFORE run_turn, which is why Started alone left the UI silent. Rendered as a collapsible "image parsing" VisionEntryCard in Message.tsx (query + response when expanded; failure = red ✗). Reducers: vision_describe replaces a still-running same-index entry (provider retry); vision_described matches by index with a last-running fallback; reduceError sweeps dangling running vision cards to "(interrupted)" on FINAL errors only (review LOW 1). Contract fixtures event-vision-describe/described.json. Multimodal providers take no vision round-trip → no card. Only exercised when main model is text-only AND a vision model is configured (Settings → Vision).
