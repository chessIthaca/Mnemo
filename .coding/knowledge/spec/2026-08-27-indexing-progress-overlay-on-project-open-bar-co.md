+++
title = "indexing progress overlay on project open (bar + counter)"
created = "2026-08-27"
status = "superseded"
+++

SPEC: Indexing progress overlay on project open — MERGED into main at d0438848 (2026-08-27, plan fad07509, backlog 4f87731f done). Tauri stream `codegraph://index-progress` (IndexProgressEvent in src-tauri/src/ipc/codegraph_cmds.rs: started/progress{done,total}/done/failed, "type"-tagged kebab-case, pinned by index_progress_event_wire_shape). Emitters: startup pass (main.rs build_brain_inner — 1s gate via startup_should_forward + PROGRESS_EVERY=50 throttle, terminal event only if ≥1 tick forwarded, so already-indexed projects stay silent) and create_project seed pass (immediate Started, no gate). seed_stores takes Option<&dyn Fn(usize,usize)>. Frontend: IndexingOverlay.tsx (self-subscribing, pure applyIndexProgress reducer, bar + mono "N/M files indexed", useBrowserOverlay when visible), mounted in App.tsx normal tree + ProjectPicker STARTUP branch only (exactly one live instance — a second in switch-mode stacked the failed card, review LOW 1). Frontend test files MUST be added to frontend/vitest.config.ts include (it replaces default discovery — silent test loss otherwise, review HIGH 1).
