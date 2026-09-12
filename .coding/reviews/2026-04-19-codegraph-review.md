# Review — CodeGraph IPC + Graph tab (uncommitted working tree)

Scope: ALL uncommitted changes vs HEAD (15 modified + 5 untracked files), against plan
76d06893 steps 7–8 (IPC commands `codegraph_status/refresh/graph`, query-layer slice API,
right-panel Graph tab, contract fixtures).

**Verified clean (no findings):**

- **`slice()` core logic** (`src/codegraph/query.rs:377-447`): both-directions BFS is correct
  (visited-set guard terminates cycles; root included; `depth.unwrap_or(1).max(1)` sanitizes 0),
  name/kind filters apply in the documented AND order, cap sorts by degree-desc/id-asc
  (deterministic), nodes are id-sorted, and the store's `UNIQUE(from_id,to_id,kind)`
  (`schema.rs:61`) plus `INSERT OR IGNORE` guarantee no duplicate edges can perturb the
  selection. The four new unit tests cover cap/filter/neighborhood/edge-scoping well.
- **`spawn_blocking` lifetimes** (`src-tauri/src/ipc/codegraph_cmds.rs:161-194`): owned
  `Option<String>`/`Option<usize>` params + the `Arc<CodeGraph>` are moved into the closure;
  the borrowed `GraphSliceFilter` is constructed inside — correct. The `??` chain
  (JoinError → String → IpcError) type-checks against `Result<Option<GraphSlice>, String>`,
  and `unwrap_or_default()` implements the "unknown root → empty graph, not error" contract.
  `graph.clone()` into `runner` before the spawn avoids use-after-move.
- **IPC security**: read-only surface; no path or SQL strings accepted from the frontend
  (filters are applied in-memory over the snapshot); `codegraph_refresh` re-indexes only the
  configured project root. No injection surface, no untrusted-payload risk.
- **Windows**: nothing new touches paths on the frontend; relpath `\`→`/` normalization is
  backend-side (pre-existing). Line-ending warnings on modified files match the repo's
  existing autocrlf state.
- **Contract fixtures**: `dto-codegraph-status.json` and `dto-codegraph-graph.json` match the
  Rust wire shapes exactly (field-by-field: `Symbol` snake_case `kind:"function"` via
  `#[serde(rename_all="snake_case")]`, `EdgeRow` `calls/imports/contains`, `last_indexed_at`
  as number). Both halves of the H4 drift detector wired (`contract_fixtures.rs:255-282`,
  `ipc-contract.test.ts:366-391`).
- **Constitution**: doc comments on every new public item (structs, fields, commands, consts,
  exported TS functions); regression tests added for all new pure logic; no `#[allow]`;
  `fromSymbol`→`from_symbol` relies on Tauri 2's camelCase conversion — the repo's established
  convention (`browser_screenshot_latest { pageId }`, etc.). Registry sync tests are generic
  (length + membership) so they hold with the new "graph" tab unchanged. `.coding/plans/*`
  edits are bookkeeping only; nothing touches protected-write targets.

---

## Findings

### 1. MEDIUM (bug) — Refresh never visibly indicates the pass, and the graph is not reloaded when indexing completes
`frontend/src/components/views/GraphView.tsx:287-298` (onRefresh), `:169-192` (poll effect);
backend race at `src-tauri/src/ipc/codegraph_cmds.rs:130-138`.

Three compounding issues in the Refresh flow:

- **Racy flag**: `codegraph_refresh` spawns the pass and *then* reads stats. The spawned task
  sets `indexing=true` only when `index()` starts — scheduler-dependent, usually *after* the
  command's stats read — so the returned status usually says `indexing: false`.
- **No poller starts**: the poll effect (`useEffect` keyed on `status?.indexing`) only sets up
  the 1.5s interval when the *last-known* status says `indexing`. Since onRefresh stores a
  `false` status, no interval ever starts → the "Indexing…" indicator never appears for
  user-triggered refreshes (it works only for the startup pass, where the mount-time poll
  catches the flag already set).
- **Stale visualization**: nothing reloads the graph payload when indexing completes
  (`onRefresh`'s `loadGraph()` runs immediately against the pre-pass store). After the pass
  finishes, the tab keeps showing the old graph until the user changes a filter.

Net effect: the Refresh button re-indexes but gives no feedback and doesn't update the view —
the feature's headline interaction. Suggested fix (small): in `codegraph_refresh`, set the
flag synchronously before spawning (or return `indexing: true` deterministically after
triggering), and in `GraphView` reload the graph on the `indexing` true→false transition
(e.g. a `[status?.indexing]`-keyed effect that calls `loadGraph()` on falling edge).

### 2. LOW (correctness/determinism) — slice edge sort lacks a `kind` tiebreak
`src/codegraph/query.rs:445`.

`edges.sort_by(|a, b| a.from_id.cmp(&b.from_id).then(a.to_id.cmp(&b.to_id)))` — a symbol pair
can legitimately carry two edge kinds (module→item `contains` plus a top-level-ref
`calls`/`imports` between the same ids), and the pre-sort order comes from iterating a
`HashSet<usize>` (`RandomState`, per-process randomized). Rust's stable sort preserves that
random relative order, so the emitted edge list can differ run-to-run — contradicting the
"Edges between the selected nodes only, deterministic order" doc (`query.rs:432`) and the
store's own `ORDER BY from_id, to_id, kind` (`store.rs:390`). Downstream it makes the React
`key={i}` line rendering (`GraphView.tsx:414`) potentially misapply styles across fetches.
Fix: append `.then(a.kind.as_str().cmp(b.kind.as_str()))` (mirrors the store's ORDER BY).

### 3. LOW (transient render glitch) — render reads the previous payload's `nodePos` against the new `model`
`frontend/src/components/views/GraphView.tsx:425-452`.

Between the `setNodes`/`setEdges` commit and the simulation effect running (post-paint), the
render maps over the *old* `nodePos.current` while indexing the *new* `model.radii` /
`model.degrees`. When the payload shrinks (e.g. applying a filter), `model.radii[i]` is
`undefined` for the tail → `<circle r={undefined}>` renders with no `r` (invisible, SVG
default 0) for a frame; in both directions the index→node alignment is semantically wrong
for one frame (wrong radii/labels). The links path is already guarded (`if (!from || !to)
return null`, `:411`) — nodes need the same treatment (e.g. `nodePos.current[i] && i <
model.radii.length`, or rebuild `nodePos` in a layout effect). Cosmetic, but visible on every
filter change.

### 4. LOW (doc accuracy / dead derive) — `GraphStats` Serialize doc claim is false
`src/codegraph/store.rs:29-31`.

New doc: "Serializable so the IPC status command returns it directly" — the command consumes
it field-by-field into `CodegraphStatus` (`codegraph_cmds.rs:65-74`); `GraphStats` itself is
never serialized. Same for `GraphSlice`'s `Serialize` (`query.rs:117`) — the IPC returns
`CodegraphGraph`, not `GraphSlice` (`EdgeRow`'s derive *is* used; these two are not).
Derives don't trip `deny(warnings)`, so this is inert, but the justification comments are
wrong — either drop the two derives or fix the docs to say what actually consumes them.

### 5. LOW (style) — accidental line-join in `view_load_roundtrips_store`
`src/codegraph/query.rs:635`.

The diff collapsed the test's leading comment onto the `fn` signature line
(`fn view_load_roundtrips_store() {        // Integration with the real store: …`).
Compiles, but it's an accidental formatting regression from this diff — restore the comment
on its own line.

### 6. NOTE (no action required) — overlapping index passes are possible
`src-tauri/src/ipc/codegraph_cmds.rs:130-138`.

Nothing prevents a second `codegraph_refresh` (or one during the startup pass) while a pass
is running. Store writes serialize under the `Mutex` with short critical sections, so this is
safe — just duplicated CPU work and a doubled log line. An `if graph.is_indexing() {
return current stats }` early-out would make it idempotent.

### 7. NOTE (no action required) — cap can drop the `from_symbol` root
`src/codegraph/query.rs:417-425`.

In a focused neighborhood exceeding `GRAPH_SLICE_CAP` where the root's degree is below the
cut, the cap can exclude the focus root itself (root is an ordinary candidate). Matches the
documented "always capped" semantics and is unlikely at depth 1 (a node with a >300-node
neighborhood is itself high-degree), but if it ever matters, always-retaining the root would
be the intuitive behavior.

---

**Summary:** 1 Medium (Refresh feedback/reload gap), 4 Low, 2 informational notes. No
security findings; no constitution violations. The backend slice engine, spawn_blocking
handling, and contract fixtures are correct as committed; the Medium and the two frontend
Lows are the items worth fixing before commit.
