## Verdict: PASS

The fix is correct, complete, well-tested, and constitution-compliant. The read-modify-write approach soundly eliminates the recurring backlog.jsonl clobber, all 8 mutating methods are covered, the `open()`→`migrate`→`persist()` path is correctly left reload-free, and the regression test reproduces the defect (fails without the fix, passes with it). No high or low findings requiring changes; a few non-blocking observations are noted at the end.

---

### 1. Correctness — read-modify-write is sound

The core invariant the fix relies on — *"every mutation persists immediately, so in-memory == last disk write"* — holds for all 8 methods:

- **Always-persist methods** (`add`, `reorder`): reload → mutate → persist. After persist, in-memory == disk. ✓
- **Conditional-persist methods** (`remove`, `transition`, `set_status`, `requeue`, `edit`, `clear_finished`): when the mutation is a no-op (id not found / illegal transition / nothing finished), the method returns early *without* persisting — but `reload_from_disk()` already ran, so in-memory == disk anyway. When the mutation applies, it persists. Either way the invariant holds. ✓

There is no path where in-memory diverges from disk after a mutation: either the mutation wrote (in-memory == disk) or it was a no-op on freshly-reloaded state (in-memory == disk). So the next mutation's reload only ever picks up *post-write external* (git) changes — never stale local state. This is exactly the property needed to prevent the clobber.

**Concurrency / TOCTOU**: `BacklogStore` is held behind `Arc<tokio::sync::Mutex<BacklogStore>>` in production (`src/agent/factory.rs:86`, `src/tool/mod.rs:1213`, `src/tool/workflow/backlog.rs:70`). Each mutation acquires the lock, runs reload→modify→persist, releases. So there is no in-process concurrent access during a single mutation; the only external writer is git (steering-time ops), which is precisely the scenario the fix addresses. The non-atomic read→write window is the *same* window `persist()` always had, and the reload strictly *narrows* the staleness window (it now picks up changes made before the mutation rather than ignoring them). No new race is introduced; the fix improves the existing one.

### 2. Edge cases — graceful, consistent with `open()`

`reload_from_disk()` (lines 435-448) handles all three failure modes by **keeping the in-memory state**, matching the documented "in-memory is the source of truth, never lost to an IO hiccup" philosophy:

- **Missing file (NotFound)**: `let Ok(content) = read_to_string else { return }` → keeps in-memory. Consistent with `open()`'s "missing → empty store" intent (a missing file is treated as a transient/IO condition, not a signal to clear). If git *deleted* the file, there is nothing on disk to clobber, so re-creating it with in-memory items is the safe, non-destructive choice. ✓
- **Corrupt/unparseable**: `parse_jsonl` `Err` → logs a warning, keeps in-memory. Mirrors `open()`'s corrupt-file handling. ✓
- **Empty file (0 bytes / whitespace)**: `parse_jsonl("")` returns `Ok(vec![])`, so reload sets in-memory to empty. This is correct — an empty file is a *valid* empty backlog (an external change saying "backlog is now empty"), distinct from a *missing* file (an IO condition). This asymmetry is deliberate and consistent. ✓

**The `open()` → `migrate_inline_images()` → `persist()` path is correctly preserved.** `reload_from_disk` is deliberately NOT added to `persist()` itself. Verified: the three `persist()` calls inside `open()` (lines 127, 154, 158) all run on freshly-loaded/migrated in-memory state with no intervening reload, so the image migration (inline data URL → sidecar path) is written out rather than being undone by a reload re-reading the still-unmigrated disk. The design rationale in the doc comment is accurate. ✓

### 3. Call-site completeness — all 8 mutating methods covered

Every method that calls `persist()` and mutates `self.items` now calls `reload_from_disk()` first:

| Method | reload line | persist line |
|---|---|---|
| `add` | 187 | 199 |
| `remove` | 205 | 211 |
| `reorder` | 222 | 238 |
| `transition` | 260 | 284 |
| `set_status` | 295 | 299 |
| `requeue` | 317 | 329 |
| `edit` | 341 | 353 |
| `clear_finished` | 383 | 406 |

No mutating method is missed. The only other `persist()` calls are the 3 migration paths in `open()` (correctly reload-free). Read-only methods (`next_pending`, `pending_item`, `items`, `resolve_images`) and filesystem-only helpers (`write_image_files`, `delete_item_images`, `migrate_inline_images`) correctly do not reload — they don't write the JSONL. `migrate_inline_images` mutates `self.items` but is only called from `open()` before persist, so no reload is needed there. ✓

Borrow-checker note: in every method `self.reload_from_disk()` is the first statement and completes before any shared (`&self`) borrow (`self.items.iter()`, `self.delete_item_images()`, `self.write_image_files()`), so there are no borrow conflicts. ✓

### 4. Regression test — reproduces the defect, verifies the fix

`backlog::tests::mutation_does_not_clobber_external_disk_changes` (lines 1425-1499):

- Opens a store on a **missing** file → in-memory empty (no file created).
- Writes 2 items to the disk file directly (simulating `git restore`/merge) — in-memory is now stale (empty) while disk has 2.
- Calls `store.reorder(&[])` — chosen because `reorder` **always** persists (even on empty input/empty store), making it the most direct clobber path.
- Asserts the 2 disk items survive; then calls `store.add(...)` and asserts 3 items (2 preserved + 1 new).

**Fails without the fix**: `reorder(&[])` on the stale-empty in-memory store persists `[]` (well, `"\n"`), wiping the 2 disk items → `assert_eq!(reloaded.len(), 2)` gets 0 → fails. ✓
**Passes with the fix**: `reorder` reloads (2 items), reorders (no-op), persists (2 items); `add` reloads (2), appends (3), persists (3). ✓

The test correctly exercises the read-modify-write mechanism with two representative mutations (one always-persist, one append). The remaining 6 methods share the identical one-line reload, so testing the mechanism rather than each method is appropriate and not redundant.

### 5. Performance — acceptable

One `std::fs::read_to_string` + `parse_jsonl` per mutation. The file is small (`.coding/backlog.jsonl`, typically a few KB), mutations are infrequent user/run-all actions, and each runs under the Mutex (no contention amplification). The cost is negligible. ✓

### 6. Constitution checks

- **Documentation sync**: `reload_from_disk` has a thorough doc comment explaining the rationale, the safety invariant, and the graceful-error policy. The public API contract ("all mutating methods persist immediately") is unchanged — the reload is an internal implementation detail — so no README/PLAN/module-doc updates are required. The BUG knowledge file (`.coding/knowledge/bug/2026-08-31-...md`) is accurate and names the regression test. ✓
- **Multi-platform neutrality**: only `std::fs::{read_to_string, write, rename}` and `Path::display()` — all cross-platform std APIs. No Windows-only paths, APIs, or shell syntax. ✓
- **Warning-free build** (`#![deny(warnings)]`): `reload_from_disk` is called from 8 sites (not dead code); the `let Ok(content) = … else { return }` binding is used; the `match` arms both use their bindings; no unused imports or stray `mut`. The test uses only in-scope items (`use super::*` + full `serde_json::`/`std::fs::` paths). No warning sources identified. ✓

---

### Non-blocking observations (no action required)

1. **Test breadth**: the regression test covers `reorder` + `add`; the other 6 methods rely on code-review coverage of the shared reload line. This is fine given the uniform mechanism, but a future hardening pass could add a parametrized/table-driven test across all mutating methods if the clobber class ever recurs.
2. **Empty-vs-missing asymmetry** (documented above): an empty file clears in-memory while a missing file preserves it. This is the correct, deliberate distinction (valid-empty vs IO-condition) and matches `open()`, but it's worth being aware of if a future git workflow ever produces a transiently-empty file during a merge conflict.
3. **`eprintln!` for the reload-parse warning** is consistent with the rest of the file's logging style (all warnings use `eprintln!`); no change needed, but if the project later adopts a structured logger, this line should migrate with the others.

**Conclusion**: ship it. The fix correctly addresses the root cause (stale in-memory state clobbering external disk changes), is complete across all mutating methods, preserves the migration path, and is backed by a regression test that fails without the fix.
