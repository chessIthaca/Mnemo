+++
title = "silent pause on image prompts — vision fallback describes images before run_turn, no event emitted"
created = "2026-08-27"
+++

Backlog adc689d8 "pause with image prompts + show query/response": root cause is src/runtime/agent.rs:436-456 — the text-only-provider vision fallback describes each pasted image via the vision model BEFORE run_turn starts, and AgentEvent::Started only fires inside run_turn (src/agent/turn.rs:237), so the UI is silent during the vision round-trip. Fix design (plan starting now): new AgentEvent::VisionDescribe {index(1-based), total, query} before each vision call + AgentEvent::VisionDescribed {index, total, success, description} after — mirroring the CompactStarted/Compacted announcement pattern (channels.rs ~306/317/436/631, console.rs ~353, events.rs log-label match ~53). Frontend: vision_describe/vision_described event kinds, a "vision" transcript entry + collapsible card in Message.tsx (query + response visible when expanded, like MemoryEntryCard). Multimodal providers are unaffected (images go inline, no vision round-trip). Pairing is guaranteed: the describe loop has no interrupt path before drive_turns.
