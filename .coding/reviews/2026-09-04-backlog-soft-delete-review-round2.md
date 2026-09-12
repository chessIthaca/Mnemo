## Verdict: PASS

Round-2 re-review of plan c9c92da5 ("Backlog soft delete with 30-day startup purge", backlog ccad2743) on `wt/agenticcoding`, verifying that commit f5be6cf resolves round-1 finding L1 (README.md:76 stale orphaned-image-cleanup claim) and that nothing else regressed. Round-1 report: `.coding/reviews/2026-09-04-backlog-soft-delete-review.md` (FINDINGS, 0 high / 1 low).

---

### 1. L1 resolved — README clause now matches shipped behavior (PASS)

README.md line 76 now reads: "…referenced by relative path and resolved back to data URLs at the IPC boundary; deleting an item (remove / clear-finished) is a soft delete — the item is marked `deleted_at` and stays in the JSONL (invisible to the UI, dispatch, and the memory indexer) with its image sidecar files kept, until a startup purge hard-removes items soft-deleted more than 30 days ago (keeping the line makes deletion survive the git union merge, which would otherwise resurrect a hard-removed line from another worktree's copy), while `edit` still replaces an item's image files in place; …"

Every claim verified against `src/backlog.rs` as committed in f5be6cf:

- **"remove / clear-finished is a soft delete … stays in the JSONL"** — `remove()` (282–294) stamps `deleted_at = Some(now_secs())` + persists, keeps the line; `clear_finished()` (510–527) stamps only *live* finished items + persists. Neither touches image files. ✓
- **"invisible to the UI, dispatch, and the memory indexer"** — `items()` (doc 529–534) returns live items only; `next_pending()` (485–490) and `pending_item()` (497–502) filter `is_live`; indexer `scan_backlog` filters `status == "pending" && deleted_at.is_none()` (`src/memory/indexer.rs:615`). ✓
- **"image sidecar files kept"** — `delete_item_images` is called only from `purge_expired` (234) and `edit` (471), never from `remove`/`clear_finished`. ✓
- **"startup purge hard-removes items soft-deleted more than 30 days ago"** — `purge_expired()` (220–243): `matches!(i.deleted_at, Some(at) if now.saturating_sub(at) >= PURGE_AFTER_SECS)` with `PURGE_AFTER_SECS = 30*24*60*60` (40); deletes sidecars, retains out expired ids, persists + logs only when something was purged; runs on BOTH `open()` paths (165 jsonl load, 198 legacy `.json` migration). ✓
- **"keeping the line makes deletion survive the git union merge…"** — matches the module doc's "Deletion lifecycle" section (18–28) and the `remove` doc (273–279). ✓
- **"`edit` still replaces an item's image files in place"** — `edit()` (463–481): `delete_item_images(id)` then `write_image_files(id, &images)` — old sidecars removed immediately, new set written. ✓

One sub-second wording nuance, explicitly **not a finding**: the purge predicate is `>=` 30 days (an item deleted *exactly* 30 days ago is purged) while the README/module doc say "more than 30 days ago" (strictly `>`). The boundary difference is at most one clock second, the module doc uses the same phrasing, and round-1's own suggested fix wording used it too — no reader is misled.

### 2. No other stale docs (PASS)

- `"cleaned up"` across `*.md`: **zero hits** in README.md, PLAN.md, or docs/ — the stale claim is fully gone. Remaining hits are point-in-time records (`.coding/plans/*`, `.coding/reviews/*` — including the round-1 report quoting the old text) and unrelated features (subagent cleanup, compaction, browser temp profiles).
- `"orphaned"` across `*.md`: no backlog-image hits in living docs; README.md:64 and docs/why-mnemo-deck.md:510 refer to *compaction* orphaned tool results (accurate, unrelated).
- `"clear-finished"` across PLAN.md/README.md/docs/: exactly one hit — the fixed clause itself, where "clear-finished" now correctly appears in the soft-delete context.
- `endpoints.toml` examples: untouched by the commit (not in f5be6cf's stat) and carry no backlog-deletion claims — unaffected, as round 1 concluded.
- `backlog.rs` module doc (18–28) is accurate and consistent with the README.

### 3. Rest of the change unchanged from round 1 (PASS)

- `git show f5be6cf --stat`: code files are exactly the four round 1 reviewed — `src/backlog.rs` (319 ±), `src/memory/indexer.rs` (31 ±), `src/project/git_ops.rs` (1 +), `frontend/src/lib/types.ts` (6 +) — plus `README.md` (2 ± = the single-line L1 fix) and bookkeeping (`.coding/backlog.jsonl`, plan/review/knowledge records).
- `git diff HEAD` is empty — clean working tree; the entire change is the one commit.
- Committed code matches round-1's cited line numbers exactly: `purge_expired` 220, `remove` 282, `edit` is_live-check 468 / `delete_item_images` 471, `next_pending` 485, `pending_item` 497, `clear_finished` 510, `items()` ~535, purge on both `open()` paths 165/198 — byte-for-byte the reviewed change.
- Indexer + frontend bits re-confirmed present: `indexer.rs:565` `deleted_at: Option<u64>` (serde default), `:615` live-pending filter, `:1422–1425` the non-vacuous exclusion test; `types.ts:33` `deleted_at?: number | null`.

### 4. Tests

Full suite re-run green after the fix per the dispatching session: 1898 passed, 0 failed (reviewer is read-only; consistent with round 1's green run — the fix touched one README line only, no code paths).

---

**Conclusion:** L1 is fully resolved; the README clause is an accurate description of the soft-delete lifecycle (soft delete keeps line + sidecars → 30-day startup purge hard-removes both → edit replaces images in place), no other stale documentation exists, and the code under review is unchanged from round 1. No findings.
