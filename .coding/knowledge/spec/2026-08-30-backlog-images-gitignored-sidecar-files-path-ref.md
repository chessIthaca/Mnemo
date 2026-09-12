+++
title = "backlog images → gitignored sidecar files + path refs (shipped eba994c)"
created = "2026-08-30"
status = "superseded"
+++

SPEC: backlog images → gitignored sidecar files + path refs in JSONL — SHIPPED on wt/agenticcoding at eba994c (2026-12-04, plan 833e476a, awaiting merge_to_main).

Behavior: backlog image attachments no longer live as inline base64 data URLs in the git-tracked .coding/backlog.jsonl (which made diffs huge). Instead:
1. BacklogItem.images (src/backlog.rs) stores RELATIVE PATHS (e.g. backlog-images/<id>/<i>.<ext>). persist() serializes paths, so the JSONL stays small.
2. Image binary data lives in gitignored sidecar files under .coding/backlog-images/<id>/<i>.<ext> (.gitignore entry added).
3. BacklogStore methods: write_image_files (data URL → parse → decode base64 → write binary file → return path; non-data-URL strings pass through unchanged for backward-compat), resolve_images (path → read file → data URL via bytes_to_data_url; missing files skipped gracefully; legacy data: strings pass through), delete_item_images (best-effort remove_dir_all on remove/edit/clear_finished), migrate_inline_images (on open, extracts legacy inline base64 to files + re-persists with paths — idempotent).
4. Path-traversal guards (defense-in-depth for the union-merge trust boundary): is_safe_item_id rejects ids with /, \, .. (guards write_image_files + delete_item_images); resolve_images rejects any ParentDir component + checks starts_with(image_root) before reading (a tampered JSONL with backlog-images/../../etc/passwd is skipped, not exfiltrated).
5. IPC layer resolves paths → data URLs at 5 boundaries (src-tauri/src/ipc/backlog_cmds.rs: backlog_add, backlog_list, emit_backlog_changed, dispatch_item; src-tauri/src/ipc/run_all.rs: run_all_dispatch_next). Frontend + agent dispatch unchanged (still receive data URLs).

Key files: src/backlog.rs, src-tauri/src/ipc/backlog_cmds.rs, src-tauri/src/ipc/run_all.rs, .gitignore, README.md.
