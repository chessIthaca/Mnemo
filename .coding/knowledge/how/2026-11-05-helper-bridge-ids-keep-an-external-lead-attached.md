+++
title = "helper bridge ids keep an external lead attached until the last leading helper closes (X-Mnemo-Bridge)"
created = "2026-11-05"
+++

Wire contract for the external-lead bridge ids (Mnemo2 local MCP server): every mnemo2-mcp helper process generates one random id (`Uuid::new_v4().simple()`, 32 lowercase hex chars) at start and sends it as header `X-Mnemo-Bridge` on every forwarded `/mcp` request, and as `"bridge": <id>` in the `/lead/detach` body its `Bridge::finish` posts on stdin close. A valid id to the server: trimmed, non-empty, at most 64 chars of [0-9a-f] (`valid_bridge_id` in mcp_server.rs); anything else (or missing) is an anonymous helper with the pre-bridge behaviour — an anonymous detach releases the external lead at once, exactly as it always did, so older helpers keep working.

Server rule (mnemo2 team + mcp_server): per team, `ExternalLead.bridges` (BTreeSet) remembers every helper id that made a tools/call while the lead was attached. A named `/lead/detach` forgets only that id and releases the lead when no remembered helper remains; the reply is `{"released": bool}` (the helper ignores it). Every release path — named or anonymous detach, the 10-minute idle sweep (no call and no waiter), `detach_external_lead` — clears the whole set, and nothing is noted while no lead is attached. The idle sweep stays the fallback for a helper that crashes without posting detach, regardless of remembered helpers. Consequence: a one-off probe helper that made one tool call can no longer drop the lead the main Claude Code helper is still holding.
