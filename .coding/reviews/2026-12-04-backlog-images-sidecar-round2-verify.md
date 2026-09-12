## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of commit `a1c99a0` on `wt/agenticcoding` (plan 833e476a "Backlog images → gitignored sidecar files + path refs in JSONL"). **HIGH 1 and LOW 1 are fully resolved.** **LOW 2 is partially resolved:** the `is_safe_item_id` guard fully protects `write_image_files` + `delete_item_images`, but `resolve_images`'s `starts_with` check does not canonicalize — a `backlog-images/../../<file>` path bypasses it, contradicting the guard's own doc comment. No other new issues.

---

### HIGH 1 — Run-All dispatch resolves image paths → data URLs — RESOLVED ✓

`src-tauri/src/ipc/run_all.rs:632` now resolves before both the send and the emit:

```rust
let images = state.backlog.store.lock().await.resolve_images(&item);   // :632
manager.send(main_id, AgentCommand::Prompt { text: run_all_prompt(&item.text), images: images.clone() })  // :639
emit_prompt_dispatched(app, main_id, &item.text, &images);             // :646
```

This is the 5th IPC boundary. The other 4 (`backlog_add`, `backlog_list`, `emit_backlog_changed`, `dispatch_item` in `backlog_cmds.rs`) resolve via `resolve_item_images` / `resolve_images` and were already correct. A repo-wide search for `AgentCommand::Prompt {` and `emit_prompt_dispatched(` confirms **no 6th boundary** carries backlog images — every other site uses `vec![]` or frontend-supplied data URLs (console, spawn, main prompt). ✓

---

### LOW 1 — `BacklogItem.images` field doc comment — RESOLVED ✓

`src/backlog.rs:35` now reads:

```rust
/// Image attachments. Stored as relative paths to gitignored sidecar
/// files (`backlog-images/<id>/<i>.<ext>`); resolved back to base64 data
/// URLs at the IPC boundary for the frontend + agent dispatch. May be
/// empty. (Pre-upgrade JSONL carried inline base64 here — migrated on
/// open.)
```

Accurately reflects path-based storage + IPC resolution. ✓

---

### LOW 2 — Path validation on sidecar read/write — PARTIALLY RESOLVED (1 low)

**`is_safe_item_id` (write/delete paths) — correct.** The helper rejects empty, `/`, `\`, `..`, `.`; `write_image_files` returns images inline (no file write) and `delete_item_images` is a silent no-op when `is_safe_item_id(item_id)` is false. The written filename is always `<index>.<known-ext>` (index from `enumerate`, ext from the fixed `mime_to_ext` set), so the write path cannot traverse. ✓

**`resolve_images` (read path) — guard is bypassable.** `src/backlog.rs` `resolve_images`:

```rust
let file = parent.join(p);
if !file.starts_with(&image_root) { return None; }   // ← does not canonicalize
let bytes = std::fs::read(&file).ok()?;
```

`Path::starts_with` compares path **components** without resolving `..`. A stored path like `backlog-images/../../etc/passwd` joins to `<parent>/backlog-images/../../etc/passwd`, whose first components (`<parent>`, `backlog-images`) match `image_root` → `starts_with` returns `true` → `std::fs::read` escapes the image dir and the bytes are encoded as a data URL. The guard's own doc comment claims it "verify the resolved path stays under the image dir (a tampered JSONL with `..` could otherwise read an arbitrary file + exfiltrate it as a data URL). Skip if it escapes" — but it does not skip in this case. This is exactly the exfiltration surface LOW 2 aimed to close.

Practical impact is low (local trust boundary — whoever can write `.coding/backlog.jsonl` already has full local read access), so LOW, but the comment is misleading and the protection incomplete. **Fix:** reject `..` components explicitly, e.g. `if file.components().any(|c| c == std::path::Component::ParentDir) { return None; }`, or canonicalize both `file` and `image_root` before comparing (canonicalize requires the file to exist, which it does here).

**No regression test for any LOW 2 guard.** The 7 new image tests cover add/resolve/edit/remove/clear/missing/migration, but none exercise `is_safe_item_id` or the `starts_with` skip. Add: (a) `write_image_files` with `item_id = "../x"` returns images inline + writes no file; (b) `delete_item_images("../x")` is a no-op; (c) `resolve_images` on an item whose `images` entry contains a `..` path returns `None` for it. Per the project constitution, security-relevant guards should be pinned by a test that fails without them.

---

### Note (non-blocking) — Run-All resolves under the manager lock

Round-1 suggested resolving "outside the manager lock (before line 552)". The fix resolves at `run_all.rs:632`, **inside** the manager lock hold (acquired `:552`, dropped `:641`), so the central dispatch lock is held during `resolve_images`'s blocking `std::fs::read`. The sibling `dispatch_item` resolves at `backlog_cmds.rs:290` *before* locking the manager (`:291`) — the two paths' lock patterns diverge despite the comment claiming they "mirror" each other. This is **deadlock-free** (no code path holds the store lock across a manager `await` — `dispatch_item` releases store before manager, and no other store site acquires manager) and the impact is negligible (rare, small images), so not a finding — but moving the resolve above `:552` would match `dispatch_item` and drop the file I/O out of the manager hold.

---

### No other new issues

- IPC `resolve_item_images` helper clones + resolves correctly at all 4 `backlog_cmds.rs` boundaries; `backlog_add`/`backlog_list`/`emit_backlog_changed` all resolve before returning to the frontend.
- `cargo test --lib` (1693 passed, 0 warnings under `#![deny(warnings)]`) + `cargo build` green per the main agent; code inspection is consistent.
- Multi-platform neutral (pure `std::fs` + `base64`; forward-slash joins). Docs (README backlog bullet, decision record, field doc) updated.
