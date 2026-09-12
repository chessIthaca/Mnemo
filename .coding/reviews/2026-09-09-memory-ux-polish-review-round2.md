## Verdict: PASS

Round-2 verification review of commit 6eac1a5 on `wt/agenticcoding` (plan d39573df, backlog 3ed2d537 — "Memory UX polish: tab staleness + lane-written knowledge surfacing + auto-recall store version"). Confirms all 4 LOW findings from `.coding/reviews/2026-09-09-memory-ux-polish-review.md` are correctly fixed in the current tree, plus the two incidental touch-ups. No new findings.

**Summary:** Each fix matches its round-1 fix direction and, where the rationale matters (LOW-3), the race-closure argument holds end-to-end. The old "lane wrote" wording survives only in `.coding/` history (the original review suggestion, the plan file, the backlog item text) — zero source occurrences. The `update_memory`/`delete_memory` tail restructures are behavior-preserving on their `Ok` contracts and additionally resolve round-1's no-op-bump observation. Test matrix (cargo 2151 lib + 16 src-tauri, vitest 1070, tsc clean, `deny(warnings)` clean) was run green by the parent after the fixes; this reviewer is read-only (no shell) and takes that as given — consistent with the diff shape (no `#[allow(...)]`, restructures compile-checked).

---

### LOW-1 — Doc attachment — FIXED

`src/tool/memory/mod.rs:58-65`. `KNOWLEDGE_WRITES` now carries its own doc ("The process-global count of successful knowledge-file writes (all agents — the store is factory-shared). The run-all completion note diffs this across the run window to surface knowledge written during the run (memory review 2026-09-08, suggestion 3).") and `/// The \`memory_write\` tool.` sits directly above `pub struct MemoryWriteTool` (:64-65). The static's doc no longer contains the tool line; each item owns its doc. (The static's doc wording also folds in the LOW-2 neutrality update — "surface knowledge written during the run", not "lane-written" — correct.)

### LOW-2 — Attribution-neutral wording — FIXED

- `end_run` (`src-tauri/src/ipc/run_all.rs:260-264`) formats `"wrote {n} knowledge record(s) during this run — uncommitted in the main tree"`, with the comment at :243-249 explaining the choice (counter is process-global, store factory-shared; sequential runs write on the main agent, parallel on main + lanes).
- Doc comments all match: `RunAllProgress.note` (backlog_cmds.rs:110-114), `IpcState.run_completion_note` (state.rs:199-203), `RunAllState.knowledge_writes_at_start` (state.rs:259-263), the `KNOWLEDGE_WRITES` static doc, and `frontend/src/lib/types.ts:87-90` — all carry the new wording or neutral phrasing; none says "lane".
- `BacklogView.tsx` tooltip unchanged and already neutral ("Knowledge records written during the run — uncommitted in the main project tree (see the Memory tab)").
- Repo-wide search: "lane wrote" appears only in `.coding/reviews/2026-09-08-full-review-memory.md` (the original suggestion — correctly untouched), `.coding/plans/d39573df.md` and `.coding/backlog.jsonl` (historical item text). No source occurrence.

### LOW-3 — Version-stamp race — FIXED (both halves; rationale verified)

**(a) Bump-after-visibility** (`src/memory/mod.rs`):
- `write` (:1259-1288): the `fetch_add` moved to the END — after the `spawn_blocking` insert completes (double-`?`) and `log_access`, at :1285-1286, with the comment stating the invariant ("Bumped AFTER the row is committed/visible: combined with the agent loop's pre-recall version read, any write invisible to a recall snapshot necessarily bumps above the stamped version (review LOW-3)"). On error the `??` propagates before the bump.
- `update_memory` (:1563-1629): binds `updated` after the JoinHandle + inner-Result double-`?` (:1591-1620); bumps only `if updated` (:1621-1627, "only when a row actually changed, and AFTER it is committed/visible"); returns `Ok(updated)`.
- `supersede_memory` (:1631-1687): bump after `tx.commit()` completes (:1684-1685); on failure (unknown id / already superseded) the `??` propagates first — no bump. Unconditional-on-success is correct: success always changes rows (insert + mark old).
- `delete_memory` (:1689-1708): binds `deleted`; bumps only `if deleted` (:1700-1706); returns `Ok(deleted)`.

**(b) Pre-recall stamp** (`src/agent/turn.rs`): `let version_before = store.version();` at :1870, BEFORE the recall, with the rationale comment (:1863-1869); the cache-hit check (:1872-1874) still reads the LIVE `store.version()`; BOTH fresh-recall arms stamp `version_before` (:1883, :1895). All 7 `last_recall_store_version` sites accounted for (doc :125, field :135, init :156, summarize reset :1781, cache check :1873, stamps :1883/:1895) — no other stamp site exists.

**Rationale holds:** with the bump strictly after visibility and the stamp read before the recall, a write whose row is invisible to the recall's snapshot necessarily has its bump postdate the version read (bump > visibility > snapshot > stamp-read), so the stamped value is below the post-bump counter → the next same-query cache check compares stamp ≠ live version → forced fresh recall that sees the row. No stale-pinning window remains. The only residual is conservative over-invalidation (a write that lands mid-recall AND is already visible in the snapshot still forces one extra fresh recall, since the stamp predates its bump) — harmless, the same class round 1 accepted. The regression test (`same_turn_memory_write_invalidates_auto_recall_cache`, src/agent/tests.rs) stays valid under the new semantics: it is sequential, so the mid-turn write is fully visible + bumped before the second recall, and the version mismatch forces the fresh recall it asserts.

### LOW-4 — README docs sync — FIXED

`README.md:81`: the parallel run-all bullet now ends with "When a run's agents wrote knowledge records (the process-global knowledge-write counter diffed across the run window), a completion note by the run controls — "wrote N knowledge record(s) during this run — uncommitted in the main tree" — persists until the next run starts." Matches the round-1 fix direction (one sentence, correct wording, persistence stated).

### Incidental touch-ups — VERIFIED

1. `end_run`'s doc comment (run_all.rs:239-241) now reads "Shared by the run's exit points in `run_all_dispatch_next`, `on_main_turn_resolved`, and `halt_run_all` (D3 dedup)" — the stale "5" is gone, replaced with path-neutral phrasing naming the three functions.
2. The LOW-3 tail restructures preserve the original contracts: `update_memory` returns `Ok(false)` exactly when the id is unknown (the no-change path's EXISTS probe returns `exists` — false for unknown, true for existing — matching the old COALESCE-UPDATE affected-rows behavior; the change path returns `updated > 0`), and the unknown-id/no-effect case no longer bumps the version (resolves round-1's over-invalidation observation). `delete_memory` returns `Ok(deleted > 0)` with the bump gated on a row actually leaving.

## Observations (non-findings)

- The README sentence lives in the parallel run-all bullet (:81) only, while the note appears on sequential runs too (`end_run` is shared). The sentence is worded generically ("a run's agents") and nothing claims parallel-only, and the run-all UI details (progress-line format) already cluster in that bullet — placement nit at most; the round-1 fix direction asked for one sentence in "the run-all bullet" and the verification brief describes exactly this placement.
- `update_memory`'s no-change-on-existing-row case still bumps the version (`updated == true`) — over-invalidation of one recall, harmless, unchanged in kind from round 1's accepted residual.
- The commit folds the four fixes into the single feature commit 6eac1a5 (24 files, +420/−47); the fix sites are exactly the ones round 1 flagged, with no unrelated drift in the touched regions.
