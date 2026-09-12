## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoding` for plan c9c92da5 ("Backlog soft delete with 30-day startup purge", backlog ccad2743). The soft-delete lifecycle is **correct, secure, and well-documented at the module level**. One low-severity documentation-sync finding (stale README claim about image cleanup).

**Files reviewed:** `src/backlog.rs`, `src/memory/indexer.rs`, `src/project/git_ops.rs`, `frontend/src/lib/types.ts` (via `git diff HEAD` + full reads of the changed regions).

---

### Correctness — soft-delete lifecycle (PASS)

**Read paths all exclude deleted items.** Every accessor that feeds the UI / dispatch / indexer filters `is_live` (`deleted_at.is_none()`):
- `items()` (535) → `filter(is_live).cloned()` — returns owned `Vec<BacklogItem>` (signature change from `&[BacklogItem]` is correct: the internal vec still holds deleted items, so a filtered *copy* is required, not a view). All cross-crate callers (`backlog_cmds.rs:78/133`, `run_all.rs`, `startup.rs:109`) consume it via `.iter()`/`.to_vec()` — no behavioral break.
- `next_pending()` (485) and `pending_item()` (497) → `Pending && is_live`. A soft-deleted pending item is **never** dispatched. ✓
- Memory indexer `scan_backlog` → `status == "pending" && deleted_at.is_none()`; `BacklogEntry` carries `#[serde(default)] deleted_at`. A soft-deleted pending item is not indexed as a live PLAN record. Test `soft_deleted_backlog_item_is_not_indexed` is non-vacuous (fixture `BACKLOG` has one live pending item `b-0001` + a done `b-0002`; the test appends a deleted pending `b-0003` and asserts `indexed == 1`). ✓

**Mutators treat a deleted id as unknown (return false).** `transition` (344), `annotate` (385), `set_note` (403), `requeue` (441), `edit` (468) all gate on `is_live(i)`. ✓
- `set_status` (417) is the **deliberate exception**: it does NOT check `is_live`, but it is documented as "raw state accessor — production callers must use `transition`; this exists for tests and internal state construction", and it sets only `status`+`note` — it never clears `deleted_at`, so a deleted item stays deleted (no resurrection). Acceptable per its documented contract. ✓
- `edit`'s second `find` (473) doesn't re-check `is_live`, but it is guarded by the `is_live` check at 468 with no intervening reload/mutation (single-threaded under the store's `Mutex`), so a deleted id cannot slip through. ✓
- `reorder` (301) does not filter deleted items, but this is harmless — it only resequences the internal list; deleted items remain invisible via `items()`. Not a finding.

**`remove()` (282) / `clear_finished()` (510) soft-delete correctly.** Both `reload_from_disk()` first (honoring the reload-before-mutation invariant from the prior clobber fix), stamp `deleted_at = now_secs()`, persist, and keep the JSONL line + image sidecars. `remove` returns `false` for unknown/already-deleted ids. `clear_finished` only stamps *live* finished items (avoids a needless re-stamp/persist of already-deleted ones). ✓

**`purge_expired()` (220) — safe and correct.**
- Filter: `matches!(i.deleted_at, Some(at) if now.saturating_sub(at) >= PURGE_AFTER_SECS)`. A live item (`deleted_at = None`) never matches the `Some(at)` pattern → **live items can never be purged**. ✓
- `saturating_sub` prevents underflow if `deleted_at > now` (clock skew / future timestamp) → such an item is retained, not purged. ✓
- Deletes image sidecars for purged ids, then `retain(!expired.contains(&i.id))`, persists + logs only when something was actually purged. ✓
- Boundary: exactly 30 days old (`now - at == PURGE_AFTER_SECS`) is purged (inclusive `>=`); the test uses 31 days (purged) vs 1 day (kept) — covers the boundary correctly. ✓

**`purge_expired` runs on BOTH `open()` paths** — line 165 (jsonl load) and line 198 (legacy `.json` migration). A legacy store has no `deleted_at` (migration sets `None`), so the legacy-path purge is a no-op today but is correct/forward-safe. ✓

**`parse_jsonl` dedup (849) — deletion-sticky in BOTH line orders** (the central correctness claim, since a git union merge can carry the same id twice: soft-deleted on one side, live on the other):
- *live then deleted:* live pushed (deleted_at=None); deleted line found → `existing.deleted_at = Some(map_or(d, max))` = `Some(d)`. Result: deleted. ✓
- *deleted then live:* deleted pushed (deleted_at=Some); live line found but its `deleted_at` is `None` → the `if let Some(d)` branch is skipped, `existing.deleted_at` stays `Some`. Result: deleted. ✓
- Two deleted lines (independent deletions on both sides): `max` of the two timestamps is taken → retention is never shortened. ✓
- Two live duplicates (pre-existing latent bug from union merge): first wins, stays live — strictly better than the old behavior (which produced duplicate display entries). ✓
The `union_merge_soft_deleted_line_stays_deleted` test exercises both orders and asserts deletion wins. ✓

**Backward compatibility.** `#[serde(default, skip_serializing_if = "Option::is_none")]` on `deleted_at`: old JSONL lines lacking the field deserialize to `None` (live); new live items serialize *without* the field → byte-identical wire format to pre-change. The `open_purges_items_deleted_more_than_30_days_ago` test includes a legacy-shape live line (no `deleted_at`) and confirms it survives. ✓

**Frontend `types.ts`:** `deleted_at?: number | null` optional — backend omits it for live items, so no UI change is needed (deleted items are filtered server-side via `items()`). ✓

**`git_ops.rs`:** test-only `BacklogItem` struct literal gains `deleted_at: None` — compiles with the new required field. ✓

### Security (PASS)
No new untrusted-input surface. `deleted_at` is a `u64` set only from `now_secs()` (server clock); it is never accepted from the frontend/IPC. `purge_expired` only ever *removes* items already marked deleted — it cannot be coerced into deleting live data. Image sidecar deletion reuses the existing `delete_item_images` path (relative paths under `.coding/backlog-images/`). No path traversal, no injection.

### Constitution checks
- **Multi-platform neutrality (PASS):** Pure Rust `std` (`SystemTime`, `std::fs`) + `serde`. No Windows-only APIs, paths, or shell syntax. Image sidecar paths are relative. ✓
- **Warning-free / no `#[allow]` (PASS):** No `#[allow(...)]` added in any changed file. `is_live`, `PURGE_AFTER_SECS`, `now_secs` are all used. Build reported green under `#![deny(warnings)]` at both crate roots. ✓
- **Public doc comments (PASS):** All public functions (`open`, `add`, `remove`, `reorder`, `transition`, `annotate`, `set_note`, `set_status`, `requeue`, `edit`, `next_pending`, `pending_item`, `clear_finished`, `items`, `PURGE_AFTER_SECS`) have doc comments; the private `purge_expired`/`parse_jsonl`/`now_secs`/`is_live` are documented too. The module doc has a new "Deletion lifecycle" section explaining soft-delete + 30-day purge + the union-merge rationale. ✓
- **Tests (PASS):** 6 new/renamed tests are non-vacuous and exercise the changed paths: `remove_soft_deletes_line_stays_on_disk`, `remove_soft_deletes_and_keeps_images_until_purge`, `clear_finished_soft_deletes_and_keeps_images`, `mutators_treat_deleted_id_as_unknown`, `open_purges_items_deleted_more_than_30_days_ago` (boundary + image cleanup), `union_merge_soft_deleted_line_stays_deleted` (both orders), `soft_deleted_backlog_item_is_not_indexed`. Full core suite reported green (1898 passed, 0 failed).

---

### FINDING L1 (low) — README.md:76 stale re: orphaned-image cleanup triggers

**Location:** `README.md` line 76.

**Issue:** The backlog feature bullet states:
> "…image attachments are stored as gitignored sidecar files under `.coding/backlog-images/` … with orphaned files cleaned up on **remove/edit/clear-finished**…"

After this change, `remove` and `clear_finished` **soft-delete** — they keep the JSONL line *and* the image sidecar files until the 30-day startup purge. Only `edit` still cleans up old images immediately (`delete_item_images` at `backlog.rs:471`). So 2 of the 3 cited cleanup triggers are now inaccurate: orphaned files are no longer cleaned up "on remove" or "on clear-finished"; they are cleaned up on `edit` and on the **startup purge** (`purge_expired`).

**Why it matters:** The project constitution requires documentation sync for behavior changes ("A feature that ships with its docs not updated is an incomplete change"). A user reading the README would expect deleting a backlog item to free its image files at once; it now retains them for up to 30 days.

**Fix:** Update the README clause to reflect the soft-delete lifecycle, e.g.:
> "…referenced by relative path and resolved back to data URLs at the IPC boundary; deleting an item (`remove`/`clear-finished`) is a soft delete that keeps the line + image sidecars on disk (invisible to the UI/dispatch/indexer) until a startup purge hard-removes items soft-deleted more than 30 days ago, so deletion survives the git union merge; `edit` replaces an item's image files in place…"

(Exact wording at the author's discretion — the key correction is that `remove`/`clear-finished` no longer clean up images immediately; the 30-day startup purge does.)

**No other stale docs found:** `PLAN.md` has no backlog-deletion claims; the `backlog.rs` module doc is fully updated; `endpoints.toml` is unaffected.
