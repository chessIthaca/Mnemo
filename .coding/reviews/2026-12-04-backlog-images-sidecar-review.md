## Verdict: FINDINGS (1 high, 2 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 833e476a ("Backlog images → gitignored sidecar files + path refs in JSONL"). The core storage change is sound — `persist()` serializes path refs (not base64), migration is idempotent, cleanup is correct on all three mutation paths, and the 4 IPC boundaries in `backlog_cmds.rs` resolve correctly. **One IPC boundary was missed**: the Run-All dispatch path in `run_all.rs` sends raw stored paths (not resolved data URLs) to the agent and frontend, breaking image-attached items in unattended Run-All mode.

---

### HIGH 1 — Run-All dispatch path does not resolve image paths → data URLs

**Files:** `src-tauri/src/ipc/run_all.rs:634, 641` (function `run_all_dispatch_next`)

The single-dispatch path (`dispatch_item` in `backlog_cmds.rs`) was correctly updated to resolve paths → data URLs before both `manager.send(AgentCommand::Prompt { images })` (line 304) and `emit_prompt_dispatched(...)` (line 312). But Run-All is a **separate** dispatch path with its own `manager.send` + `emit_prompt_dispatched`, and it was NOT updated:

```rust
// run_all.rs:633-641
manager
    .send(main_id, AgentCommand::Prompt { text: run_all_prompt(&item.text), images: item.images.clone() })  // ← PATHS, not data URLs
    .map_err(|e| format!("failed to dispatch run-all item: {e:?}"))?;
drop(manager);
emit_prompt_dispatched(app, main_id, &item.text, &item.images);  // ← PATHS, not data URLs
```

`item.images` now holds relative paths (`backlog-images/<id>/0.png`), not data URLs. Two consequences for every Run-All item that has image attachments:

1. **Agent dispatch broken.** `AgentCommand::Prompt { images }` is consumed by the agent task to build `MessageContent::Parts` (vision content) — it expects `data:<mime>;base64,...` URLs. A bare filesystem path is not a valid image URL, so the vision content is malformed (the model receives a non-data-URL string where an image part is expected).
2. **Frontend goal bubble broken.** `emit_prompt_dispatched` carries the images to the frontend, which renders `<img src={dataUrl}>` (`Message.tsx:311`, `BacklogView.tsx:654`). A relative path like `backlog-images/...` is not a resolvable `src` in the webview, so the dispatched-prompt goal shows broken images.

This directly violates the plan goal ("the frontend and agent dispatch are unchanged") for the Run-All path. The single-dispatch (▶ button) and auto-feed paths route through `dispatch_item` and are correct; only Run-All is affected. There is no integration test covering Run-All dispatch with images (the code-quality review flagged Run-All as untested), so this escaped the green `cargo test`.

**Fix** (mirror `dispatch_item`): resolve the images before the send + emit. `item` is already an owned clone from `next_pending()`, so resolve outside the manager lock (before line 552), exactly as `dispatch_item` does at `backlog_cmds.rs:290`:

```rust
let images = state.backlog.store.lock().await.resolve_images(&item);
// ... manager.send(main_id, AgentCommand::Prompt { text: run_all_prompt(&item.text), images: images.clone() }) ...
// ... emit_prompt_dispatched(app, main_id, &item.text, &images) ...
```

This makes Run-All consistent with the single-dispatch path. Consider extracting a shared helper so the two dispatch paths can't drift again.

---

### LOW 1 — `BacklogItem.images` field doc comment is now stale

**File:** `src/backlog.rs:35`

```rust
/// Image attachments (data URLs), may be empty.
pub images: Vec<String>,
```

The stored representation is now **relative paths** to gitignored sidecar files (resolved to data URLs only at the IPC boundary in `backlog_cmds.rs`). The same struct is also serialized by the agent-facing `backlog_list` tool, where it now carries paths (smaller — an improvement, but the doc says "data URLs"). Update the doc to reflect the path-based storage + IPC resolution, e.g.:

```rust
/// Image attachments. Stored as relative paths to gitignored sidecar files
/// (`backlog-images/<id>/<i>.<ext>`); resolved back to base64 data URLs at
/// the IPC boundary for the frontend + agent dispatch. May be empty.
```

---

### LOW 2 — No path validation on sidecar read/write (defense-in-depth)

**Files:** `src/backlog.rs` — `write_image_files` (line 428), `resolve_images` (line 470), `delete_item_images` (line 497)

The focus point asks to verify "no path traversal (the id is a UUID...)". That holds for the `add` path (fresh `uuid::Uuid::new_v4()`). But `edit` and `migrate_inline_images` use an `item_id` sourced from existing items, and `resolve_images` joins an arbitrary stored path string (`parent.join(p)`) with **no validation** that the result stays under `image_dir()`. In normal operation these are UUIDs / generated paths, but a tampered or maliciously-merged `.coding/backlog.jsonl` (the union-merge driver concatenates lines from both sides) could carry an `item_id` or image path containing `..` — `resolve_images` would then read an arbitrary file and encode it as a data URL sent to the model/frontend (mild exfiltration surface), and `write_image_files`/`delete_item_images` would write/remove outside `.coding/backlog-images/`.

This is a local-trust-boundary concern (the user owns the JSONL), so LOW — but a cheap defense-in-depth fix exists: reject `item_id`s containing path separators / `..` in `write_image_files` + `delete_item_images`, and in `resolve_images` verify the joined path canonicalizes under `image_dir()` before reading (skip otherwise, mirroring the existing graceful-skip for missing files).

---

### Verified correct (focus points 1–4, 7–10)

- **JSONL carries paths, not base64 (FP1):** `persist()` serializes the in-memory `BacklogItem` (`serde_json::to_string(i)` over `self.items`), and `add`/`edit`/`migrate` store path strings in `item.images`. The migration test asserts the re-persisted JSONL `!contains("base64")` and `contains("backlog-images/")`. ✓
- **Backward-compat (FP2):** `parse_data_url` returns `None` for any non-`data:...;base64,...` string, so test fixtures / opaque strings pass through unchanged in both `write_image_files` and `resolve_images` (the latter also passes through legacy `data:` strings). The `add_with_non_data_url_passes_through_unchanged` test pins this. ✓
- **Cleanup (FP3):** `remove` → `delete_item_images` (line 204); `edit` → `delete_item_images` then `write_image_files` (lines 337-338); `clear_finished` → collects finished ids before retain, deletes each (lines 391-393). All best-effort (NotFound is a silent no-op). Tests `remove_deletes_image_files` + `clear_finished_deletes_image_files` + `edit_replaces_image_files` cover the three paths. ✓
- **Migration idempotency (FP4):** `migrate_inline_images` filters on `starts_with("data:")`; after migration items hold paths, so re-open finds nothing to migrate → returns `false` → no re-persist. `write_image_files` passes already-migrated paths through (they don't parse as data URLs), so no double-write. The legacy `.json` path persists twice (once with inline, once after migration) — wasteful but the final on-disk state is correct and self-healing on crash. ✓
- **Gitignore (FP7):** `.coding/backlog-images/` matches `image_dir()` = `<jsonl-parent>/backlog-images` = `.coding/backlog-images`. ✓
- **Multi-platform (FP8):** pure `std::fs` + `base64` 0.22 (a dep in both `Cargo.toml`); forward-slash paths join correctly on Windows; no Windows-only APIs. ✓
- **Docs (FP9):** README backlog bullet updated; doc comments present on `write_image_files`, `resolve_images`, `delete_item_images`, `migrate_inline_images`, `resolve_item_images`, and the free functions. (The `BacklogItem.images` field doc is the one stale spot — LOW 1.) ✓
- **Build (FP10):** `bytes_to_data_url(mime: &str, bytes: &[u8]) -> String` (`image_tools/mod.rs:198`, `pub(crate)`) matches the call site; `base64::Engine` trait imported correctly. Build green per the main agent. ✓
