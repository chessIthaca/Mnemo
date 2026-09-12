## Verdict: FINDINGS (3 high, 4 low)

**Scope**: all uncommitted changes on wt/agenticcoding for plan ffd7a86f (parallel run-all) — src/project/worktrees.rs (new), src-tauri (run_all.rs, events.rs, spawn.rs, state.rs, backlog_cmds.rs, console.rs, main.rs), src (backlog.rs, agent/factory.rs, runtime/agent/loop_impl.rs), frontend (BacklogView.tsx + tests, useAgentStore, types.ts, tauri.ts, fixtures), README.md, agent.md, .gitignore, and the plan file.

**Summary**: The git-worktree layer, per-agent event routing, lock ordering, landing serialization, and N=1 sequential-path preservation are sound — but the run-lifecycle integration has three high-severity defects: (1) the run ends prematurely while parallel lanes are still in flight, orphaning items/branches/agents in the *common* case; (2) the main-lane selection can double-dispatch an item a spawned agent is still working; (3) the worktree-aware "landed" predicate reads the plan file from the wrong path so it is *always false* for spawned items — breaking the closed-earlier stall recovery and making the exit-drain delete landed work. None of the three is covered by a behavioral test.


---

## HIGH findings

### H1. Premature `end_run` orphans in-flight parallel lanes (5 sites)

`end_run` clears the run state unconditionally; several call sites don't check whether spawned lanes (or the main lane) are still in flight:

- **run_all.rs:2510-2518** — `run_all_dispatch_next`'s no-item path: `next_pending_eligible()` → `None` → `end_run`. The comment "Nothing eligible left — the run is complete" is false in parallel mode: the remaining items can be `InFlight` in the spawned lanes or the main lane (InFlight items are not Pending, so they're invisible to the selector).
- **run_all.rs:3785-3788** — `on_main_turn_resolved`'s stopped path ends the run unconditionally. The spawned sibling's equivalent (`on_spawned_turn_resolved`, run_all.rs:3244-3258) correctly checks `any_in_flight` (current_item + spawned) first — the main path must do the same.
- **run_all.rs:4041-4048** — `halt_run_all`'s no-item arm.
- **run_all.rs:4260** — `drain_run_all_on_main_exit`: the main agent's exit ends the run while independent (parentless) spawned agents are still working.
- **run_all.rs:2477-2482** — `compact_then_dispatch_next`'s stop path.

**Repro (the acceptance scenario itself)**: 2 pending items, parallel on (concurrency 3). `backlog_run_all` → main gets item 1, `fill_spawned_window` spawns item 2. Item 2's agent finishes first → `on_spawned_turn_resolved` → Done → lands the branch → `run_all_dispatch_next` → no Pending items (item 1 is InFlight) → `end_run` → run state dropped. Item 1's later resolution finds `run_all_active = false` → single-dispatch path → `single_in_flight` is `None` → item 1 stays InFlight forever, its work never lands via the run. Symmetrically, if item 1 resolves first, item 2's `owns_spawned_run` → `false` (run_all is None) → its resolution is a no-op: item stuck InFlight, branch never landed, agent never retired (it stays in the manager until app shutdown). This hits the *common* case — any parallel run where a lane terminally resolves while no Pending items remain (2 items/concurrency 3: the first resolution orphans the second; 5 items/concurrency 3: the last two lanes get orphaned).

**Aftermath**: stale `wt/runall-*` branches + `.worktrees/runall-*` worktrees; the next parallel run's `provision_item_worktree` refuses the existing branch → `dispatch_spawned_item` errors → `fill_spawned_window` returns on the first error (see L4) → the window never refills. Recovery is only the next run-all start's `adopt_orphaned_in_flight` requeue — and its landed-work detection is also broken for these items (H3).

The plan's step 5 says "end_run when nothing is in flight **and** nothing eligible remains" — the "nothing in flight" half is missing at all five sites. Fix direction: gate every `end_run` on `current_item.is_none() && spawned.is_empty()` (the `any_in_flight` read at run_all.rs:3244-3258 already computes exactly this — hoist it into a helper).

### H2. Main-lane selection can double-dispatch a spawned agent's item

**run_all.rs:2510** — the main path selects via `next_pending_eligible()` with **no exclusion of items already handed to spawned lanes**. A spawned item stays `Pending` until its agent's workflow enters Executing (`stamp_spawned_in_flight`, events.rs:476-488). If the main agent's item terminally resolves inside that window, `run_all_dispatch_next` hands the SAME item to the main agent while a spawned agent works it in a worktree.

Realistic trigger: a spawned agent's first turn ends in a waiting arm (provider error, not abandoned) — its item stays Pending for minutes while the run waits; meanwhile the main agent's item resolves (Done, or a quick plan abandonment → Failed) → dispatch_next → the spawned agent's item is selected for the main lane.

Consequences: two agents on one item — the exact cross-corruption class this feature exists to prevent (two checkpoints: main tree + worktree; both stamping/resolving). When the second resolution arrives, the item is no longer InFlight → the `may_flip` guard fails (run_all.rs:3146-3160) → the entry is removed with `remove_worktree = false` → worktree + branch leak.

The asymmetry is the tell: `fill_spawned_window` excludes the main's current_item (run_all.rs:2745-2755), but the main path doesn't exclude the spawned ids. Fix: select the main lane via `next_pending_eligible_excluding` with current_item + spawned ids (the helper exists, backlog.rs:522-541, and is already tested).

### H3. `orphan_work_landed_at` reads the plan file from the wrong path — the landed predicate is always false for spawned items

**run_all.rs:4169-4172**: `root.join(".coding").join("plans").join(format!("{plan_id}.md"))`. A spawned agent's plans dir is `<worktree>/.coding/plans/agents/<agent_id>/` (spawn.rs:235-240), so its plan file lives at `<worktree>/.coding/plans/agents/<agent_id>/<plan_id>.md` — the predicate's `plan_steps_all_done` reads a file that never exists at the path it builds → the landed predicate is **always false** for spawned items. Consequences:

- (a) The `closed_earlier` recovery (run_all.rs:3146-3150) never fires for spawned lanes → the e33a07fd slid-resolution stall class is unrecovered for spawned agents: the item stays InFlight with a waiting note and the run waits forever for a turn resolution that already happened.
- (b) `drain_spawned_on_exit`'s done-orphan guard (run_all.rs:3350-3360) never fires → a crashed spawned agent whose work DID land on its branch is requeued to Pending **and** its worktree + branch are removed (`remove_item_worktree` → `git branch -D`) — completed work destroyed, duplicate re-dispatch. The README bullet ("unless the work already landed, which auto-resolves done and lands the branch") describes behavior that cannot occur.

Same wrong-path assumption in `stamp_spawned_in_flight`'s title lookup (run_all.rs:3428: `worktree/.coding/plans` — cosmetic only: the in-flight chip falls back to the short plan id). Fix: thread the agent's plans dir (or derive `agents/<agent_id>/` from the agent id) into `orphan_work_landed_at` and the title lookup.


---

## LOW findings

### L1. No behavioral coverage of the dispatch→resolution→landing flow

The plan's step 4/5 test list (two items + concurrency 2 → main + spawned; a spawned agent's resolution flips only its item; Done lands the branch; the drain requeues a dead spawned agent's item) is implemented only as **source-contract string matches** (run_all.rs tests `parallel_dispatch_fills_the_spawned_window_on_every_dispatch_exit`, `spawned_dispatch_records_the_run_before_the_prompt_send`, `spawned_resolution_is_routed_per_agent_never_main`) — they pin that certain strings appear in the source, not that the loop works. The real-git tests (worktrees.rs) cover the git ops in isolation; the store test covers the exclusion; nothing exercises dispatch + resolution + end-run together. All three HIGH findings live in that untested seam — the acceptance criterion ("two items dispatched concurrently… both land") has no test that fails today. A state-level test of `run_all_dispatch_next`'s no-item path with a non-empty spawned set would have caught H1 directly.

### L2. `item_branch` is dead code with a triplicated derivation

worktrees.rs:40-43 (`item_branch`) has no callers anywhere; the branch-name derivation is inlined in `provision_item_worktree_impl` (worktrees.rs:70-71) and a third 8-char-prefix derivation builds the agent name in `dispatch_spawned_item` (run_all.rs:2854). Drift hazard — use it at both sites or delete it. (The 8-char UUID prefix is hex → always a valid ref name; a collision fails loudly at provision — acceptable.)

### L3. Reviewer reports for spawned items land in the main tree, not on the item's branch

`WriteReviewReportTool`/`FinishTool` are constructed with the factory's main-derived `reviews_dir()` (factory.rs:925, 966), so a spawned agent's reviewer writes to the MAIN tree's `.coding/reviews/`. The in-process round-trip works (the spawned agent's finish gate reads the same dir), but (a) the landed merge carries the code without its review record, and (b) the main agent's next `commit_success` (`git add -A` on a dirty main tree, git_ops.rs:179-194) sweeps the spawned items' reports into the MAIN item's commit — cross-item bookkeeping contamination. Deliberate per design decision D4 ("reviews stay shared"), but the landing consequence is undocumented (README/agent.md).

### L4. `fill_spawned_window` stops on the first dispatch error

run_all.rs:2786-2789: any `dispatch_spawned_item` error returns from the fill — one stale branch (e.g. left behind by H1's orphaned lanes) blocks the whole window refill: items behind the failing one never spawn, and the run silently degrades to the main lane with only an eprintln. Skip-and-continue (annotate the item, keep filling) would be more robust.

---

## Verified good (review-focus items that check out)

- **Lock ordering**: consistent everywhere — run_all → current_item → spawned (fill_spawned_window run_all.rs:2745-2755; on_spawned_turn_resolved:3244-3258). `spawned` is always nested under the run_all guard, never the inverse; the std Mutexes are scoped, never held across awaits.
- **LANDING_LOCK**: correct serialization (static tokio Mutex held across the git ops; no other lock taken under it except the brief project-root clone). The landing worktree is reused with `reset --hard main` re-sync; conflicts capture `--diff-filter=U` before `merge --abort`; `--no-ff` keeps main merge-commits-only.
- **Record-before-send**: the SpawnedRun entry is pushed under the run_all lock before the prompt is sent (dispatch_spawned_item run_all.rs:2866-2886) — the agent cannot emit resolution events before the entry exists.
- **Double-resolution**: sequential per-agent event processing (Finished/Error precede Exited in the stream) + the InFlight re-verify under the store lock make the turn-resolution vs Exited-drain interleave safe in the normal paths; the retire→Exited ordering leaves the drain a no-op.
- **Window accounting**: `spawned_count + 1 >= concurrency` reserves the main lane (under-fills by one while the main lane is deferred — documented intent); the exclude list (current_item + spawned ids) is correct for the spawned selection.
- **Busy-guard semantics**: fill runs on all three dispatch exits (success, busy-guard defer, checkpoint-race defer) — spawned lanes fill while the main is busy, as intended.
- **Sequential-path preservation (N=1)**: `fill_spawned_window` returns at the `concurrency <= 1` guard; `owns_spawned_run` / `stamp_spawned_in_flight` / `drain_spawned_on_exit` all no-op on an empty spawned set; payload additions are additive (`parallel`, `concurrency`, `spawned: []`). No behavior change found on the N=1 path.
- **Security**: `SpawnedRunView` exposes only agent_id/item_id/branch — no worktree paths to the frontend. Git args are fixed strings + branch names from 8-char UUID prefixes passed as Command args (no shell) — no injection surface. The worktree path never crosses into prompts or notes beyond the branch name.
- **Multi-platform neutrality**: worktrees.rs is std::path + git subprocess + tempfile; no Windows-only APIs; the `\\?\` canonicalize prefix is handled in the tests; `git_run` reuses the CREATE_NO_WINDOW guard. Cross-platform clean.
- **Constitution**: doc comments on all new public functions; no `#[allow]` anywhere in the diff; README + agent.md updated and match the *intended* design (modulo H1/H3 breaking the described behavior); `.gitignore` covers `.worktrees/`; the worktree's `backlog.jsonl` is a provision-time snapshot the shared store never writes (it writes the main root's file), so landing merges carry no stale status data — the union driver is the safety net.

**Test matrix**: root cargo test 2129/0/4, src-tauri 264/0/0, frontend 1060/1060, both builds pass — consistent with the findings above: the bugs live in the untested integration seam, not in any covered unit.
