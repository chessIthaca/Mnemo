## Verdict: PASS

Round-2 verification of commit 0c0307d (plan 6c6f985d, backlog bba2c82d, branch wt/agenticcoding): all four round-1 findings are correctly resolved, the fixes introduce no regression, and the final pass over the whole commit — correctness, bugs, security, and the project constitution checks — is clean. Round 1's whole-change verification (the evidence chain, the mirror-exact guard, the six guarded arms, the ten call sites, the tests) stands: the fixes are surgical additions to already-verified code.


## Fix verification (all four round-1 findings)

### LOW-1 — worktree removal in all three blocked arms: RESOLVED

- All three sites set the flag: the Done-blocked arm (run_all.rs:3888), the spawned success-path plan_abandoned blocked arm (:3937), the spawned failure-path blocked arm (:4000).
- **Tail interaction verified** (run_all.rs:4030-4079): `remove_spawned_run` (:4039) and `retire_spawned_agent` (:4048) run UNCONDITIONALLY; the worktree removal (:4040-4047) sits between them as `let _ =` best-effort — a `Result` swallow with no `?` propagation and no panic path (`remove_item_worktree` is `spawn_blocking` + `git_raw`, both `Result`-returning; a JoinError maps to `Err`), so it cannot skip the SpawnedRun removal or the agent retirement. The done-counter bump stays gated on `terminal_resolution` (blocked arms leave it false — correct: the item was not resolved); `emit_backlog_changed`, the stopped check, and `run_all_dispatch_next` all still run. This is the same tail shape the may_flip arms already used (`remove_worktree = true` on landing Ok) — the blocked arms share the established risk profile, no new risk class.
- `remove_item_worktree` (src/project/worktrees.rs:104-131) removes BOTH the worktree and the branch (`git branch -D`), with the two ops run independently (a remove failure cannot skip the branch delete — the R3-L2 design), after a `worktree prune`. Exactly the remediation the R3-L2 lane-redispatch blocker class requires.
- **No path removes a worktree it shouldn't**: the "run WAITS" arms (any-other-turn-end :3948-3964, terminal Error :4006-4028) return early with `remove_worktree = false` — the still-live run keeps its worktree; the Done success path keeps the worktree on a landing CONFLICT (branch kept for manual resolution); the `closed_earlier` recovery (the item's own landed work) diverts to the may_flip branch before any blocked arm, so landed item work is never discarded. Every arm that sets the flag is one where the spawned run is definitively over (the tail tears down the run + retires the agent regardless) — the worktree has no future owner, and a future lane re-dispatch provisions fresh from `main` (the drain path's established semantics, `drain_spawned_on_exit` :4190-4201). The item-vanished early path (:3775-3780) already removed the worktree unconditionally — consistent.

### LOW-2 — reworded comments: RESOLVED, accurate

Verified against the actual semantics in src/backlog.rs: `transition` REPLACES the note (`item.note = note`, :643-644); `annotate` APPENDS (`"{existing} | {addition}"`, :666-669, doc at :649-660). Every reworded site now states exactly this — the wipe damage is `transition`'s; `annotate` appends, but the recovery evidence stays unpolluted either way (the deliberate asymmetry with the Done-blocked row, which annotates a transient note): the spawned success arm (:3926-3931), the spawned failure arm (:3995-3998), the run-all main success/failure arms, the single-dispatch `may_flip_failed` comment, and both single-dispatch blocked rows (:4869-4873, :4887-4888). No site retains the wrong "annotating would wipe it" claim.

### LOW-3 — ordering pin: RESOLVED, effective

The test (run_all.rs:2094-2120) now locates `let prev_top = prev_top_plan_id.get` and `prev_top_plan_id.insert` and asserts read_site < insert_site (both < note_site). Verified in events.rs: the read (:455) precedes the insert (:457), which precedes the `note_plan_abandoned` call (:471). Both search strings are UNIQUE in the file (the declaration at :357 matches neither; the Exited-arm cleanup is `.remove`; no comment carries either literal), so `find`'s first-occurrence semantics resolve to exactly the intended sites. Moving the read after the insert — the exact degradation round 1 described (the pre-event top becoming the post-pop top, the guard degraded to blind) — now fails the first assertion with the explanatory message; moving either past the latch call fails the second. The pin is concrete and effective.

### LOW-4 — BUG memory: RESOLVED, complete

.coding/knowledge/bug/2027-01-07-plan-abandoned-arms-flipped-items-the-plan-never.md is in the commit with the full chain: symptom (the steer-pivot damage class, Pending → Failed a legal row), root cause (six unguarded arms + the linkage evidence unavailable live post-pop), fix (the evidence chain, the guard, the six arms, remove_worktree), all four regression-test names, and the round-1 pointer. The semantic BUG record exists and is findable (memory id 7112091b, confirmed by search and auto-recall). The plan's step-2 requirement is discharged.


## Regression check on the fixes

- The `remove_worktree = true` additions change behavior only in the three spawned blocked arms; the main-path and single-dispatch blocked arms touch no worktree (worktree provisioning is spawned-lane only). Verified the main-path downstream (run_all.rs:4705-4729): the done-counter is gated on `terminal_resolution` (no bump when blocked), the `current_item` pointer is cleared UNCONDITIONALLY (:4726 — outside the `terminal_resolution` check), `item_resolved = terminal_resolution` → no auto-compact, dispatch-next still runs — the re-queued item stays Pending and eligible. The single-dispatch blocked rows return `(None, None)`: no transition, the `if let Some(note)` skips annotate entirely (note preserved byte-for-byte), the pointer restored via check-and-set (:4944-4953), `resolved_terminally = false` suppresses auto-feed. No stuck runs, no double resolutions.
- The comment rewordings and the strengthened test are inert to runtime behavior.

## Final pass over commit 0c0307d

- **Correctness**: round 1 verified the evidence chain (emission contract → prev-top invariant → latch hygiene), the mirror-exact `plan_linkage_allows_failed`, all six guarded sites (no seventh escape hatch), the ten call sites, and the blocked arms' recovery flows; the fixes are surgical (three flag sets, comment rewordings, one strengthened test, the BUG memory file) and touch none of that verified logic.
- **Security**: no new attack surface — plan ids flow from workflow events to a guard comparison; `remove_item_worktree` passes paths/branches as git arg arrays (no shell interpolation); the eprintln diagnostics log item ids (UUIDs) only.
- **Documentation sync**: internal dispatch/resolution logic — no README/PLAN.md/config/UI surface; the required BUG memory is written and complete; doc comments on every new public function (`plan_linkage_allows_failed`, `note_plan_abandoned`, `abandoned_plan_id`) with root-cause references and backlog ids.
- **Multi-platform neutrality**: no platform-specific code; the worktree ops go through the established cross-platform module (`spawn_blocking` + arg-array git); `eprintln!` is cross-platform.
- **Warning-free build**: both Rust suites green under `deny(warnings)` (root 2138/0/4, src-tauri 281/0/0) — a green suite under `deny(warnings)` proves zero warnings; the read-level check agrees (all new params used, no unused imports).
- The side-car artifacts in the commit (backlog.jsonl in_flight stamping, the plan file, the round-1 report, the BUG memory file) are expected app/agent writes.

## Notes (verified, not findings)

- The Done-blocked arm now removes the worktree AND annotates its diagnostic — the annotate is pre-existing behavior (an InFlight item's note is transient); only the removal was added. Consistent.
- In the Done-blocked case the spawned agent completed a plan that is not the item's — that work is discarded with the branch. This matches the drain path's established semantics ("a re-dispatch starts fresh from `main`") and is the exact tradeoff round 1 recommended; keeping the branch is the R3-L2 blocker.
- The single-dispatch blocked row restores the `single_in_flight` pointer over a Pending item — the same shape as the pre-existing plan_open row; the stale pointer is guarded by the very linkage guards this commit adds (both require InFlight).
- The plan file's step-4 checkbox is still open at commit time — expected: the closing sequence (this review → commit → finish) completes it; the regression-test field is already recorded.