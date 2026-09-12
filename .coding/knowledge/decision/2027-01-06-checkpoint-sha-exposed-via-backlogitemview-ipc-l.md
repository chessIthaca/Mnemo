+++
title = "checkpoint_sha exposed via BacklogItemView (IPC layer), not a field on the persisted BacklogItem"
created = "2027-01-06"
+++

DECISION (backlog 212c14af, plan 2929d340, wt/agenticcoding): removed the tree's first #[allow(dead_code)] (on extract_checkpoint_sha, src-tauri/src/ipc/run_all.rs) by choosing fix option (b) — restoring a production caller — over option (a) deletion. Rationale: arch-perf review's preferred option; turns the manual resume/rollback anchor into a machine-readable feature instead of deleting working, tested parsing logic.

Design: IPC-layer view struct `BacklogItemView` in src-tauri/src/ipc/backlog_cmds.rs — `#[serde(flatten)] item: BacklogItem` + `checkpoint_sha: Option<String>` (always serialized → stable wire shape; flat JSON so the TS interface just gains an optional field). The persisted `BacklogItem` model in src/backlog.rs stays pure — no derived field on the store struct, no core-crate changes (avoids the 6 struct-literal sites and the stale-derived-data footgun of persisting a parsed copy). `resolve_item_images` became `resolve_item_view` (pub(crate)); ALL FOUR IPC read paths route through it: backlog_list, backlog_add, emit_backlog_changed, and startup_snapshot (the fourth added per review LOW 1 — the startup snapshot had been left on the raw BacklogItem shape, missing checkpoint_sha AND pre-existing image resolution). extract_checkpoint_sha is now pub(crate) with a real caller.

Deliberately NO UI chip: the note (which contains the sha) already renders on the backlog card; the field serves programmatic consumers. Tests: unit test BacklogItemView::new (sha+reason → Some, note preserved; non-sha/no note → None) + source-contract pins that the read paths route through resolve_item_view (backlog_cmds.rs + startup.rs) + the dto-backlog-changed-payload fixture (Rust + TS sides) locks the wire shape with a checkpointed item.
