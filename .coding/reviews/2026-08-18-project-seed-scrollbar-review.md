# Review: eager project-store seeding + phantom-scrollbar fix (branch feat/new-project-seed)

Scope: all uncommitted changes per `git status --short` / `git diff HEAD` —
`src/project/mod.rs`, `src-tauri/src/ipc/projects.rs`, `frontend/src/lib/textareaAutosize.ts` (new),
`frontend/src/lib/textareaAutosize.test.ts` (new), `frontend/vitest.config.ts`,
`frontend/src/components/layout/InputBar.tsx`, `frontend/src/components/views/BacklogView.tsx`.
(`.coding/plans/stack.json` + the untracked plan file are bookkeeping — ignored.)

**Verdict: the diff is clean.** No correctness, bug, security, or constitution-compliance
findings. Every review-focus claim was verified against the supporting code, not assumed.
Two non-blocking informational notes at the end.

## Verification against the review focus

### 1. `seed_stores` correctness — VERIFIED (src/project/mod.rs:145-185)

- **Error mapping keeps context**: all three `map_err` closures embed the failing path
  (`self.memory_db.display()` / `self.codegraph_db.display()` / `self.root.display()`), the
  operation, and the source error, via the same `Error::Project` variant used throughout the
  module.
- **Idempotency**: `MemoryStore::open` (src/memory/mod.rs:287-303) only opens two connections
  and applies the schema; the schema (src/memory/schema.rs:32+) is exclusively
  `CREATE TABLE/INDEX IF NOT EXISTS` + pragmas — a re-open writes nothing.
  `CodeGraph::index` (src/codegraph/mod.rs:171-233) is content-hash incremental
  (`file_hash` comparison skips unchanged files; the prune sweep is a no-op when nothing
  changed), so a second seed pass is a cheap scan. Covered by `seed_stores_is_idempotent`.
- **HashEmbedder is genuinely inert**: `apply_schema` performs no INSERTs; the embedder is
  purely in-process state (`embedder: RwLock<Arc<dyn Embedder>>`, src/memory/mod.rs:263/295)
  and nothing about it is persisted on open. With zero rows,
  `stored_model_fingerprints()` returns an empty set, so startup's `reembed_if_needed`
  (src/memory/mod.rs:44-78) computes `needs_reembed = true` but `reembed_all` iterates zero
  rows and writes nothing. The startup embedder-swap / re-embed logic (src-tauri/src/main.rs:875-908)
  is unaffected. The claim in the doc comment is accurate.
- `Arc::new(HashEmbedder::new())` coerces to the `Arc<dyn Embedder>` parameter — matches
  `MemoryStore::open`'s signature.

### 2. `create_project` lock discipline — VERIFIED (src-tauri/src/ipc/projects.rs:87-126)

- The config read is in a scoped block (lines 89-92); the guard drops at block end, strictly
  before `spawn_blocking(...).await` (line 99). No lock is held across the await.
- Seed failure cannot fail creation: all three match arms are handled — `Ok(Ok(()))` no-op,
  `Ok(Err(e))` and `Err(e)` (JoinError, incl. panic) both `eprintln!` and fall through to
  name-defaulting, registration, and `Ok(path)`. There is no early return on any seed path.
- The registry-add block (lines 118-126) takes a fresh `mut` lock correctly (same shape as
  `remove_project`). No deadlock or double-lock path.
- Bonus: `spawn_blocking` tasks are un-abortable, so even if the frontend dropped the IPC
  response future mid-seed, the seed still completes — desirable, since `switch_project`
  restarts into the seeded project.

### 3. Blocking work on the async runtime — VERIFIED

- `seed_stores` is fully synchronous (rusqlite, `std::fs::read`, `std::sync::Mutex`); it is
  `move`d wholesale into `tokio::task::spawn_blocking` (projects.rs:98-99) and the async fn
  only awaits the JoinHandle. `CodeGraph::index` is confirmed `pub fn` (not async; progress
  callback is `&dyn Fn`). This mirrors the startup pattern at main.rs:1002-1006.
- No other blocking call was introduced on the async path in this diff (`Project::init`'s
  small fs ops pre-existed unchanged).

### 4. `overflowY` boundary logic — VERIFIED (frontend/src/lib/textareaAutosize.ts:27-33)

- `heightPx = min(scrollHeight, maxPx)`, `overflowY = scrollHeight > maxPx ? "auto" : "hidden"`:
  equality → hidden. At `scrollHeight == maxPx` the border-box shortfall (2 × 1px borders)
  clips only 2px of bottom padding — 8px (`py-2`) on InputBar + BacklogInput, 6px (`py-1.5`)
  on the backlog edit textarea — so no text is clipped in any of the three usages.
- No stuck-overflow path: InputBar/BacklogInput effects run on mount (`[text]` initial run);
  the edit effect's `[editing, editText]` fires in the same commit the textarea renders
  (conditional render BacklogView.tsx:504-523), so `editRef.current` is set. Both style
  properties are written together unconditionally on every run, including shrinks
  (`height = "auto"` first) and the disabled case (InputBar `disabled={isSubagent}` —
  scrollHeight is measurable on disabled elements). Closing the editor unmounts the
  textarea, so no stale style survives.
- Sweep for missed sites: the only other `scrollHeight` uses in the frontend
  (Conversation.tsx:35, InflightBar.tsx:33) are scroll-position checks on divs, not the
  autosize pattern — nothing was missed.
- Effect deps not re-running on font-size/family changes is pre-existing behavior, unchanged
  by this diff — not a regression.

### 5. Rust / repo conventions — VERIFIED

- Doc comments on all new pub items (`seed_stores`, updated `create_project` doc; the TS
  helper has JSDoc). No `#[allow(...)]` anywhere in the diff; no unused imports added.
- `IndexStats` (src/codegraph/mod.rs:50-66) carries no `#[must_use]`, so the discarded
  `graph.index(None).map_err(...)?;` value is warning-free — consistent with the
  `#![deny(warnings)]` build.
- Tests exercise the real code: the graph test writes a parseable `lib.rs` and asserts the
  symbol is resolvable by name through a fresh `CodeGraph::open` + `view().resolve`
  (exact-name matching, src/codegraph/query.rs:189-204); the disabled-gate test asserts
  `codegraph.db` is NOT created; the idempotency test double-runs. All use `tempdir`.
- Test registration: `frontend/vitest.config.ts:32` correctly adds
  `src/lib/textareaAutosize.test.ts` to the include list (pure node-env test — fine).

### 6. Other correctness / security — none found

Paths are derived from `Project::from_root` on a validated existing directory; seed writes
only inside the project's `.coding/`; no injection or traversal surface. Best-effort
`eprintln!` failures match the codebase's established warning style (cf. main.rs:867, 923).

## Non-blocking informational notes (not findings; no action required)

1. **Picker latency on huge trees** (src-tauri/src/ipc/projects.rs:99): `create_project`
   awaits the full first index, so converting a very large existing directory blocks the IPC
   response (and the subsequent switch) for the index duration. This is the plan's explicit
   synchronous-seeding choice and failure-safe; just be aware the picker has no progress
   feedback during it. A future enhancement could move seeding off the response path
   (fire-and-forget like startup's main.rs:1002) without changing correctness.
2. **Re-creating the currently-open project's directory**: seed would open the same
   memory.db/codegraph.db the running app holds. WAL + `busy_timeout=5000`
   (src/memory/schema.rs:22-29) and per-file transactions make contention transient, and any
   `SQLITE_BUSY` surfaces as a logged best-effort seed failure — no corruption path.

## Required before finish (main agent)

Run the suites (read-only reviewer could not): `cargo test` (workspace) and
`cd frontend; npx vitest run` — expect the three new `seed_stores` tests and the three
`autosizeForScrollHeight` tests green, zero warnings under `#![deny(warnings)]`.

Report path: .coding/reviews/2026-08-18-project-seed-scrollbar-review.md
