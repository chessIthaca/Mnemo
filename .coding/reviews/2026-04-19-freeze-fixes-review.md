# Review — Freeze-Fix Plan (F1/F1b/F2/F3/F6) + CodeGraph WIP

**Scope:** ALL uncommitted changes (`git diff HEAD` + untracked `src/codegraph/`, `src/tool/agent/codegraph.rs`, `.coding/plans/*.md`, `.coding/reviews/2026-04-19-freeze-diagnosis.md`). Two bodies of work: the completed freeze-fix plan and the in-flight CodeGraph feature.

**Verdict:** The freeze-fix plan is correct at its core. F1's lock-drop/`spawn_blocking` pattern is right, F3's caps cover every *array-length* mutation site, F1b's edge detection handles all four transitions correctly, F2's metadata check is sound, and F6 was correctly verified as already-at-HEAD (`src-tauri/src/ipc/events.rs:337`). I found **no High-severity bugs**, but **two Medium correctness gaps** — one in the CodeGraph WIP (indexing flag never cleared) and one F3 gap (unbounded *per-entry* growth via tool-call merging) — plus several Low/informational items.

---

## Medium

### M1. CodeGraph: `indexing` flag is set `true` and never cleared — stuck "indexing" forever
**File:** `src-tauri/src/main.rs:940-957`

```rust
let g = graph.clone();
tauri::async_runtime::spawn(async move {
    g.set_indexing(true);
    let result = tokio::task::spawn_blocking(move || g.index(None)).await;  // g moved here
    match result { Ok(Ok(stats)) => eprintln!(...), ... }                    // g is gone
});
```

`g` is moved into the `spawn_blocking` closure, so the async block **cannot** call `g.set_indexing(false)` after completion — and it doesn't. After the first startup index, `is_indexing()` returns `true` for the rest of the process lifetime. The flag's only purpose is the status surface (`codegraph_status` / Graph tab, the current WIP step), which will permanently show "indexing…" once wired. Nothing reads it yet (verified: only `factory.codegraph_handle()` and codegraph's own tests reference it), so this is latent — but it must be fixed before the Graph-tab step lands.

Secondary mismatch in the same block: the comment (and `mod.rs` docs) promise "index panics → graph unavailable, tools omitted", but a panic mid-`index` leaves `codegraph = Some(..)` and the `graph_*` tools registered over a store that may be poisoned — they then error **at call time** instead of being omitted.

**Fix:** clone the Arc for the flag reset (e.g. `let g2 = graph.clone();` used in the match arm to call `g2.set_indexing(false)` in all three outcomes), or move the flag set/clear inside `CodeGraph::index` itself (cleaner; matches `indexing_flag_flips_around_pass`'s intent). On the `Err`/panic paths, either drop the handle or document the degrade-to-error-at-call-time behavior.

### M2. F3 gap: the `calls` array inside one merged tool entry grows without bound
**File:** `frontend/src/hooks/agentEventReducer.ts:263-281` (merge), `:349-363` (result keeps the entry "last")

`capTranscript` bounds the **number** of transcript entries, not the **size of one entry**. The merge path in `reduceToolCallStart` appends a call to the last entry whenever it is a tool card with the same name — and `reduceToolResult` updates that entry *in place* (it stays the last entry, keeping the chain alive). So a run of N sequential/parallel calls to the same tool with no intervening text or different tool (very common: `search`→`search`→…, batched `file_read`s, Run-All loops) accumulates **N calls — each with full `args` + full `result` output (file contents can be 100 KB+)** in a single transcript entry.

This is the same progressive-slowdown/memory shape F3 set out to kill, relocated per-entry: `reduceToolResult` clones the whole `calls` array (`[...entry.calls]`, line 355) on every result, so a 1,000-call merged card pays O(1000) per result plus unbounded retained memory. `MAX_TRANSCRIPT_ENTRIES` never fires because the *entry count* stays at 1.

**Fix:** cap the merge (e.g. stop merging past ~50 calls per card — push a new card instead), and/or cap the retained `result.output` size on merged cards. A regression test should drive `tool_call_start` × N for the same tool + `tool_result`s and assert the entry splits.

---

## Low

### L1. F3 gap: `planDiffs` is an unbounded array holding full diff bodies
**Files:** `frontend/src/hooks/useAgentStore.ts:254` (field), `agentEventReducer.ts:110-112` (`upsertPlanDiff`), `:812-814` (effect applied)

`planDiffs` keeps one entry per **distinct edited path** for the current top plan, each holding `unifiedDiff` / `content` strings, and is never capped — it only resets when `topPlanId` changes (`:828-835`). A large plan (or a long session on one plan — the norm in this app) grows it without bound. Strictly outside the transcript/activityLog scope of the freeze diagnosis, but it is a missed unbounded growth path in the same store with the same symptom class. A `.slice(-N)` by path-recency (like `toolOutputLog`'s `-50`) would match the established pattern.

### L2. F1b: unmount guard was dropped (setState / in-flight IPC after unmount)
**File:** `frontend/src/App.tsx:311-317`

The old poll had a `cancelled` flag so a late `getGitBranch()` wouldn't `setGitBranch` after unmount; `refreshGitBranch` has none. In React 18 a post-unmount `set` is a silent no-op, so this is benign — but the in-flight promise also isn't cancelled, worst case one wasted git subprocess on unmount. Informational; restore a `cancelled` guard only if you want parity with the old code.

### L3. F1b has no regression test (constitution: every fixed defect needs one)
**File:** `frontend/src/App.tsx:329-338`

F1, F2, F3 each added a regression test; F1b (the 5 s → 60 s poll + turn-end refresh) added none. The interesting logic — `runningOf` + the `prev && !cur` edge — is a pure function of two store snapshots and is trivially extractable (e.g. `didMainTurnEnd(prev, next)` in `agentState.ts`) and testable for all four transitions (false→false, false→true, true→true, true→false), including the main-agent-id-changes and agent-removed (`exited`) cases. Recommend extracting + testing.

### L4. CodeGraph: `Mutex::lock().expect(...)` panics propagate after a poisoned store
**File:** `src/codegraph/mod.rs:118-120, 126, 170-172, 191-192, 239-241`

Every store access unwraps the lock with `expect`. A panic inside `index()` while holding the lock (or inside `upsert_file`'s transaction) poisons the `Mutex`, after which **every** `stats()`/`view()` call panics in the caller's thread. The agent tools survive (`run_query`'s `spawn_blocking` converts the panic to `ToolResult::error`), but the future IPC status command would panic in the Tauri command handler. Consider catching poison (`into_inner()` — the store is a plain cache, corruption is recoverable by rebuild) or replacing `expect` with an error path.

### L5. F1's regression test shells out to real `git` and is environment-sensitive
**File:** `src-tauri/src/ipc/files.rs:113-144`

The test is well-constructed (local `user.email`/`user.name`/`commit.gpgsign=false`, deterministic `checkout -b`), but it inherits the ambient environment: `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` env vars or a missing `git` on PATH make it fail for reasons unrelated to the code. Low risk on this repo's dev machines; a `Command::env_remove("GIT_DIR")…` sweep would make it hermetic.

### L6. `codegraph.db` gets no permission restriction (consistency)
**File:** `src/codegraph/store.rs:83-87` vs `src/provider/trace.rs:773-775`, keys.toml/M6 handling

`memory.db`, `keys.toml`, and `traces.jsonl` are all restricted to the current user; `codegraph.db` (and its `-wal`/`-shm`) is not. It contains only repo-derived structure (names/lines/edges) — no secrets — so this is consistency, not vulnerability. Also note it lives inside the sandbox root and is agent-writable; corrupting it is gracefully recovered (cache, rebuilt on restart), but adding it to the M5 protected-write-target list would match how `memory.db` is treated.

---

## Informational / verified-good (no action required)

**F1 (freeze vector) — pattern verified correct.** `let root = state.project.root.lock().await.root.clone();` drops the guard at the end of the statement (before `spawn_blocking`), so the project lock is provably not held across the subprocess, and the blocking `git` call is off the async runtime. `CREATE_NO_WINDOW` is preserved verbatim inside `read_git_branch` (`files.rs:333-338`). No shell, fixed args, `current_dir(root)` — no injection surface. `map_err` chain to `IpcError::from(String)` matches `browse_markdown_file`'s established pattern.

**F1b edge detection — all four transitions correct.** zustand v4's `subscribe` listener receives `(state, prevState)`; `runningOf(prev) && !runningOf(s)` fires only on true→false. false→false and false→true correctly no-op; true→true no-ops. The main-agent-removed case (`exited` deletes the agent → `?? false`) fires a refresh, which is harmless/correct. `refreshGitBranch` is a stable `useCallback`, so the effect doesn't re-subscribe per render, and the `unsub` return cleans up (StrictMode-safe). `setInterval` doesn't fire on registration, so there's no duplicate of the mount-time fetch at `App.tsx:188-193`.

**F2 — cap logic verified sound.** `metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_TRACE_FILE_BYTES` is the right check against actual file state: a metadata error (missing file) → 0 → merge path → `read_to_string` default → same fresh-write result; a torn/invalid-UTF-8 file also collapses to fresh via `unwrap_or_default()` (pre-existing). Redaction still runs *before* the cap decision, so the fresh-start path cannot persist unredacted rows. Note the residual behavior (accepted, documented): below the cap each writer wakeup still re-reads + rewrites up to ~8 MiB, and a cap-triggered rewrite drops every row not in the current batch — with `MAX_RECORDS=32` × 2 MiB bodies a live ring alone can exceed the cap, so the file will oscillate grow→fresh-start. Effectively `traces.jsonl` is now "recent history past 8 MiB"; the doc comment at `trace.rs:698-700` should ideally say that explicitly.

**F3 — array-length growth paths: all verified capped.** Exhaustive sweep of every `transcript`/`activityLog` write: `flushStreamingText` (`agentState.ts:351`), `pushTranscriptEntry` (`:368`), `reduceToolCallStart` memory push (`agentEventReducer.ts:255`) and tool push (`:282`), `reduceError` (`:704,708`), `reduceChildFinished` (`:744`), `reduceReasoningDelta` (`:172`, guard no-op), `appendStreamingReasoning` batch path (`useAgentStore.ts:862`), and all three InputBar sites (`:147, :290, :472` including the untrusted `/load` payload — good catch). The in-place mutators (`reduceToolResult:364`, `reduceToolCallArgDelta`, `applyToolCallArgDeltas`) never change array length, so they need no cap. `toolOutputLog` keeps its pre-existing `slice(-50)`. `capTranscript` returns the same reference under the cap (no gratuitous re-render identity churn). Both new vitest regressions exercise the caps through the real store dispatch path and assert content + order.

**F6 — claim verified.** `prev_workflow_state.remove(&agent_id)` exists at `src-tauri/src/ipc/events.rs:337`; no change was needed and none was made. Accurate.

**CodeGraph — otherwise solid.** Walker fails closed outside root and reuses `should_search`/`IGNORED_DIRS` (`walk.rs:79-84`), doesn't follow symlinks, caps at 1 MB. `relpath` normalizes `\`→`/` (`mod.rs:248-252`) — Windows-safe DB keys. `Store` is `Send` behind a `Mutex` (rusqlite `Connection` is `Send`), so `CodeGraph` is `Send + Sync` and the `spawn_blocking` in `run_query` is legal; the view snapshot pattern keeps the lock held only for two SELECTs. `upsert_file` is a single transaction with both-direction edge cleanup; `rebuild_edges` correctly re-derives cross-file edges only when `reindexed > 0 || pruned > 0`. `.gitignore` covers `codegraph.db-*` (WAL/SHM). All `graph_*` tools are `AutoRun` read-only with graceful unknown-name results. Doc comments present on every public item across `codegraph/*`, `factory.rs`, `config/general.rs`, `project/mod.rs`, `files.rs`, `trace.rs`, `agentState.ts` — constitution-compliant. Tests are thorough (extraction, resolution, incremental/prune semantics, walker, schema idempotency, tool payloads).

**Two things to be aware of, no bug today:**
1. `Cargo.lock` adds `indexmap` under `serde_json` — feature unification (via tree-sitter) now compiles serde_json with `preserve_order`, so JSON object key order becomes insertion-ordered **process-wide** (previously sorted). Cosmetic effect on `traces.jsonl` rows and any serialized JSON output ordering.
2. `codegraph` defaults **on** (`config/general.rs:227-231`) and the startup pass reads + hashes every source file even when nothing changed (`mod.rs:161-167` — mtime is recorded but never used as a pre-filter). For this repo that's fine; for large repos every startup pays a full-tree read in the background. Consider an mtime short-circuit before hashing, and confirm default-on is the intended rollout posture while the Graph tab is still unbuilt.

**Line endings:** `.gitignore`/`Cargo.toml`/`Cargo.lock`/`.coding/*` show LF→CRLF warnings consistent with existing repo state; no mixed-ending introductions in the tracked diffs. Untracked `src/codegraph/*.rs` couldn't be checked by git — worth a quick `git add` sanity pass when committing.

---

## Required fixes before commit
1. **M1** — clear the codegraph indexing flag (and reconcile the panic-path comment).
2. **M2** — bound merged-call growth per tool card + regression test.
3. **L3** — extract + test the F1b turn-end edge (constitution requirement).
4. L1/L2/L4/L5/L6 — fix or explicitly waive with justification.
