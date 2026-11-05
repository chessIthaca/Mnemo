## Decisions

- 2026-11-05: added step 3 (Integration check) because steps 1 and 2 ran in parallel

## Interfaces

- 2026-11-05: Wire contract between the helper (step 2) and the server (step 1): forwarded `/mcp` requests carry header `X-Mnemo-Bridge: <id>` (32 lowercase hex chars, `Uuid::new_v4().simple()`, one per helper process, fixed for its lifetime); `/lead/detach` body is `{"root": <root>, "bridge": <id>}`. The server treats a missing header/field as an anonymous helper (today's behaviour). The `/lead/detach` response stays a JSON object; step 1 may add `"released": bool` but the helper ignores the response.
- 2026-11-05: File ownership: step 1 owns src-tauri/src/mnemo2/mcp_server.rs and src-tauri/src/mnemo2/team.rs; step 2 owns src-tauri/src/bin/mnemo2_mcp/serve.rs and src-tauri/src/bin/mnemo2_mcp/serve_tests.rs. Neither touches the other's files.
