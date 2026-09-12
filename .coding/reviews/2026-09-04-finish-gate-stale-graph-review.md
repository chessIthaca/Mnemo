## Verdict: PASS

Reviewed the full uncommitted diff on `fix/finish-gate-stale-graph` (plan b40d040f): `src/tool/workflow/plan.rs` plus `.coding/` bookkeeping (`backlog.json`, `plans/stack.json`, untracked plan file).

### Summary

The finish bug_fixing regression-test gate now does snapshot resolve → on miss one `spawn_blocking` incremental `CodeGraph::index(None)` → re-resolve, fail-closed on reindex/`JoinError`/still-missing. Schema text documents auto-refresh. Regression test `finish_refreshes_stale_graph_and_finds_new_regression_test` constructs a never-indexed in-memory graph, writes the symbol after open, and requires finish → Complete. Existing missing-symbol and happy-path tests remain compatible (error still contains `not found in the code graph`).

### Correctness / bugs / security

- **Retry logic:** `found_in_snapshot` short-circuits; miss path uses `graph.clone()` + `spawn_blocking` + `.ok().and_then(|r| r.ok()).is_some()` so `Err` and panic/`JoinError` fail closed; still-missing uses the updated message with the preserved substring.
- **Locks across await:** Workflow lock is snapshot-and-drop at `execute` ~922–937 before gates. The `view()` snapshot is dropped before `spawn_blocking`. Re-lock only at the final state transition (~1095+). Comment at 1018–1020 is accurate.
- **Notes:** `"code graph was stale — refreshed before the symbol check"` is pushed only when resolve succeeds *after* refresh (`refreshed == found_after`), and is joined into the success output/data — correct placement.
- **Cost:** Single incremental pass on miss; content-hash based (`CodeGraph::index` docs / implementation). No loop/retry storm.
- **Security:** No new trust boundary, path handling, or injection surface. Bookkeeping-only / in-process graph.

### Concurrency

- `CodeGraph::store()` is a short-held `std::sync::Mutex` (poison-recovering); `view()` holds it only for snapshot load; `index()` takes it per file / upsert, not across the finish await.
- Watcher (`watcher.rs` ~197–206) waits on `is_indexing()` before its own pass; finish does not wait, so a finish refresh can overlap a watcher pass. Store mutex serializes DB work; `indexing` is status-only (AtomicBool + drop guard). No deadlock with the workflow `Mutex`. Residual double-index is wasted work, not a finish-gate correctness bug — not raised as a finding for this minimal fix.

### Constitution

- No `#[allow]`, no Windows-only APIs/paths/shell; `tokio::task::spawn_blocking` is portable.
- Public `FinishTool` docs/schema style preserved; schema description updated for the new behavior.
- Warning-free build claimed by parent full matrix; diff introduces no obvious unused items.

### Documentation sync

- Finish tool schema description correctly notes automatic stale-graph refresh before block.
- No README / PLAN.md user-facing surface requires this internal gate behavior.
- System-prompt finish text is app-level (out of repo) — N/A here.
- Field rustdoc on `FinishTool.codegraph` still says “resolves … against it” without naming refresh; accurate enough given schema + inline gate comment citing backlog #91.

### Bug-plan requirements

1. **Regression path:** `finish_refreshes_stale_graph_and_finds_new_regression_test` exercises execute → graph gate → miss → `spawn_blocking` reindex → retry → Complete. Success is impossible without the refresh path (graph never indexed before the late `tests.rs` write).
2. **Root cause documented:** Plan `.coding/plans/b40d040f-c507-48ec-acd1-075cf762b0f6.md` (Bug/Context), gate comment (backlog #91), and test module comment.
3. **BUG: memory:** Auto-capture via `src/memory/finish_capture.rs` (`capture_finish` writes `BUG: {title}` for `PlanKind::BugFixing`; unit test `bug_fixing_plan_also_writes_bug_digest`). No pre-existing BUG row required; mechanism exists. Happy-path finish test still checks PLAN:/BUG: capture counts.

### Other uncommitted files

- `.coding/plans/stack.json` — active plan id `b40d040f-…`, `reviewed: false`: expected mid-closeout.
- `.coding/backlog.json` — #91/#87 flipped to `cant_resolve` with “plan loop never ran…” notes; structural JSON is valid (`next_id: 92`). Not corrupted by the code change; status noise is session bookkeeping, not a product defect in this diff.
- Untracked plan markdown is the plan under review — fine.

### Findings

No findings.