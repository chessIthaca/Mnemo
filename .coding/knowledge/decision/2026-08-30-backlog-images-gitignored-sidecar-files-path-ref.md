+++
title = "backlog images → gitignored sidecar files + path refs in JSONL"
created = "2026-08-30"
+++

DECISION: backlog images → gitignored sidecar files + path refs in JSONL (backlog a39830cf, design 2026-12-04).

Problem: BacklogItem.images: Vec<String> holds base64 data URLs serialized inline into the git-tracked .coding/backlog.jsonl → huge diffs that pollute `git diff` / git_read context.

Design (chosen): gitignored image files + path references in JSONL + IPC-layer resolution to data URLs.
1. Image files at .coding/backlog-images/<item-id>/<index>.<ext> (binary, decoded from base64). Gitignored (add .coding/backlog-images/ to .gitignore).
2. BacklogItem.images stores RELATIVE PATHS (e.g. backlog-images/<id>/<0>.png) in memory + on disk — NOT data URLs. So persist() writes small path strings; the JSONL stays small.
3. BacklogStore (src/backlog.rs): add(text, images=data_urls) writes each data URL to a file (parse data URL → mime→ext → decode base64 → bytes) + stores the path. edit() deletes old image dir, writes new. remove()/clear_finished() delete image dirs. New method resolve_images(&self, item) -> Vec<String> reads each file → data URL (bytes_to_data_url with mime from ext). open() migrates legacy inline data URLs (strings starting with "data:") to files + paths.
4. IPC layer (src-tauri/src/ipc/backlog_cmds.rs): resolve paths → data URLs at 4 boundaries: backlog_add return, backlog_list, emit_backlog_changed, dispatch_item (AgentCommand::Prompt { images } + emit_prompt_dispatched both need data URLs).
5. Frontend: UNCHANGED (receives data URLs, renders <img src={dataUrl}>). CSP already allows data: URLs.
6. Cleanup: delete image files on remove/edit/clear_finished. Graceful degradation: missing image files (cross-worktree, manually deleted) → skip (fewer thumbnails).

Why not convertFileSrc/asset protocol: CSP has no `asset:` in img-src; enabling it is a security-surface change. Resolving to data URLs in the IPC layer avoids frontend changes + CSP changes. IPC payload size is unchanged (frontend needs the data to render); only the git-tracked JSONL shrinks.

Key files: src/backlog.rs, src-tauri/src/ipc/backlog_cmds.rs, .gitignore. Reuses base64 crate + bytes_to_data_url (image_tools).
