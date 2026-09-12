## Verdict: FINDINGS (0 high, 4 low)

Plan 6c6f985d (backlog bba2c82d) — guard the plan_abandoned → Failed transitions like Done. The core change is correct and well-tested: the evidence chain (forwarder prev-top tracking → TurnResolveLatch → resolution guards) is sound, `plan_linkage_allows_failed` is mirror-exact with the Done guard, all six Failed sites are guarded (no seventh exists), all ten call sites pass the new arg in the right order with the right agent id, and the blocked arms' recovery flows are complete (no stuck runs, note preserved byte-for-byte). Four low findings: the spawned blocked arm leaves the worktree+branch behind (the R3-L2 lane-redispatch blocker class); a factually wrong mechanism claim in the blocked-arm comment; an ineffective ordering pin in the evidence-chain test; the plan's checked BUG-memory step has no findable record.


## What was verified (all checks from the task brief)

### 1. The evidence chain — CORRECT

- **Emission contract** (src/agent/turn.rs:965-1033): `emit_workflow_state` fires a `WorkflowStateChanged` on EVERY successful workflow tool call (create_plan, update_plan, complete_step, abandon_plan, finish, skill_start, skill_end, abandon_skill), carrying the post-operation state + `top_plan_id` read under the workflow lock. Failed calls are gated out (no stack change, no event). Therefore every stack push/pop emits an event with the then-current top — the forwarder's `prev_top_plan_id` tracking cannot go stale between events.
- **The invariant**: at a root abandonment, `abandon_plan` popped the only stack frame (the state went to Planning ⇒ the stack emptied ⇒ the popped frame was the root and was on top). The tracked prev-top is the top from the last event, which (by the emission contract) equals the stack top just before the pop — i.e. the abandoned root plan's id. The event's own `top_plan_id` is the post-pop top (None), so passing the PRE-event tracked top (read at events.rs:455, before the map insert at 457) is exactly right.
- **Sequences checked**: create→abandon (prev_top = the plan ✓); create→finish→create→abandon (finish keeps the finished plan on the stack and emits with it; create clears the finished stack and emits the new top; the abandonment captures the new plan ✓); sub-plan push/pop then root abandon (each push/pop emits with the new top, so the last event before the root abandon names the root ✓); sub-plan abandons stay Executing (stack non-empty) and never fire `is_root_plan_abandonment` ✓; the dispatch-time Complete→Planning entry never emits, and a Planning→Planning abandon is not a transition (pinned by the pre-existing test at run_all.rs:1416-1419) ✓.
- **Latch hygiene**: `plan_abandoned` (HashSet) and `abandoned_plan_ids` (HashMap) are written together by `note_plan_abandoned` and cleared together by `on_started`/`on_exited` — the flag discriminates; `abandoned_plan_id()` returning None covers both absent and present-None (blind). The borrow of `Option<&str>` across the `.await` is fine (the forwarder loop is sequential; nothing mutates the latch between the read and the resolution).
- **Blind case**: nearly unreachable in practice (the forwarder lives for the whole session, so every agent's first WorkflowStateChanged is observed), and it is the safe pre-guard fallback. Mirrors the Done guard's blind case.

### 2. Guard logic — MIRROR-EXACT

`plan_linkage_allows_failed` (run_all.rs:410-425) is structurally identical to `plan_linkage_allows_done`: status != InFlight → false (Pending stays Pending, terminal statuses never re-flip); (Some, Some) → equality; (None, Some) → false; `_` → true (blind). All SIX Failed-transition sites are guarded — confirmed by literal search, the only `"plan abandoned (abandon_plan)"` transition sites are run_all.rs:3901, 3963 (spawned success/failure), 4536, 4624 (run-all main success/failure), 4832, 4850 (single-dispatch rows); no seventh site exists. Guarding the spawned-lane pair beyond the task's four was right — same damage class.

### 3. Blocked arms' flows — COMPLETE, NO STUCK RUNS

- **Spawned** (run_all.rs:3881-3904 + tail): blocked → `terminal_resolution=false` (no done-counter bump), but `remove_spawned_run` and `retire_spawned_agent` run unconditionally at the tail and `run_all_dispatch_next` continues. The lane is freed; the run does not stall. (One gap on the worktree — finding 1.)
- **Run-all main** (4536/4624 arms + 4672ff): blocked → no transition, no annotate, done-counter not bumped, the `current_item` pointer cleared unconditionally, `item_resolved=false` → no auto-compact, dispatch-next still runs. The re-queued item stays Pending and remains eligible.
- **Single-dispatch (None, None) row** (4834ff): the downstream `if let Some(note) = &note` skips annotate entirely (note preserved byte-for-byte), the pointer is restored via check-and-set, `resolved_terminally=false` suppresses auto-feed. Correct.

### 4. The ten call sites — CORRECT

All ten (events.rs:644, 664, 709, 721, 736, 747 direct; 1165, 1187 main-flush; 1243, 1264 lane-flush) pass `turn_resolve.abandoned_plan_id(<id>)` immediately after `turn_resolve.plan_abandoned(<id>)`, matching the signatures, with the correct agent id per site (agent_id / main_id / lane_id — each matching its `plan_abandoned` sibling).

### 5. Constitution

- **Docs**: internal logic only — no config/UI/README/PLAN.md surface. Doc comments on every new function, parameter, and arm are exemplary (root-cause references, backlog ids, rationale).
- **Multi-platform**: no platform-specific code; `eprintln!` is cross-platform. ✓
- **Security**: plan ids flow from workflow events to a guard comparison; no new attack surface; the eprintln leaks only a UUID. ✓
- **Warning-free**: all new params used, no unused imports; read-level check clean (I cannot run cargo as a read-only reviewer — the verdict on compile/tests relies on the parent's reported green suites for both crates plus this read-level verification).

## Findings

### LOW-1: The spawned blocked arm leaves the worktree + branch behind — the R3-L2 lane-redispatch blocker class

**Location**: src-tauri/src/ipc/run_all.rs:3897-3904 (spawned `_ if plan_abandoned` arm) — the blocked branch leaves `remove_worktree = false`.

**Evidence**: The tail removes the SpawnedRun and retires the agent unconditionally, but `remove_item_worktree` only runs `if remove_worktree`. The blocked arm's typical item is exactly the re-queued Pending item — eligible for lane re-dispatch — and `provision_item_worktree` (src/project/worktrees.rs:57-59, 80-92) refuses an existing branch (`git worktree add -b`). Every subsequent lane fill attempt (run_all.rs:3447-3460) then fails provisioning, appends `"parallel dispatch failed: branch already exists"` to the item's note, and skips it. The app's own documentation of this exact state (remove_item_worktree, the R3-L2 fix): "a stale `wt/runall-*` branch blocks every later spawned-lane dispatch of the item ('branch already exists' → the fill's skip arm → permanently undispatchable via lanes)".

**Impact**: bounded — the item is NOT lost (stays Pending, note preserved; the plan's acceptance holds), the run does not stall (the fill skips and continues), the main lane can still dispatch the item (no worktree provisioning on the main path), and the InFlight-mismatched sub-case self-heals via the next-run adoption sweep. But the re-queued item is lane-undispatchable until the branch is manually removed, and every fill cycle pollutes its recovery note with another failure annotation.

**Mitigating context**: this mirrors the Done-blocked arm's pre-existing behavior (run_all.rs:3866-3873 also leaves `remove_worktree = false`), so it is consistent with the prior art — but the drain path (`drain_spawned_on_exit`) removes the worktree in the same semantic situation ("a re-dispatch starts fresh from main; the crashed run's incomplete work is not kept"), and R3-L2 treats stale branches as a bug class to prevent.

**Suggested fix**: set `remove_worktree = true` in the blocked branch — the spawned run is over either way and the worktree has no future owner (the abandoned plan's work is worthless for the item). Ideally fix the Done-blocked arm the same way (or queue a follow-up item for that pre-existing case).

### LOW-2: The blocked-arm comment's mechanism claim is factually wrong — `annotate` appends, it does not wipe

**Location**: src-tauri/src/ipc/run_all.rs:3968-3971 (spawned blocked arm) and ~4834-4841 (single-dispatch blocked row): "annotating would wipe it, the exact damage this guard prevents".

**Evidence**: `annotate` APPENDS — `"{existing} | {addition}"` (src/backlog.rs:661-672, doc at 650-659: "The addition is appended to any existing note"). Only `transition` replaces the note (that is the wipe in the original damage). The no-annotate OUTCOME is still the right call (the re-queue note is preserved byte-for-byte, no diagnostic pollution — arguably better than the Done-blocked arm, which does annotate), but the comment misleads a future maintainer into thinking `annotate` is destructive and into wondering why the Done-blocked arm annotates safely.

**Suggested fix**: reword — the wipe damage is `transition`'s; the no-annotate choice avoids polluting recovery evidence (and note the deliberate asymmetry with the Done-blocked row).

### LOW-3: The evidence-chain test's ordering pin is ineffective — it does not pin the read-before-update ordering it claims

**Location**: src-tauri/src/ipc/run_all.rs:2094-2106 (`abandonment_evidence_captures_the_abandoned_plan_id`).

**Evidence**: the assertion `forwarder[..note_site].contains("prev_top_plan_id")` (note_site = the `note_plan_abandoned` call at events.rs:471) is satisfied by the map's DECLARATION alone (events.rs:~357 precedes 471). Moving `let prev_top = prev_top_plan_id.get(...)` to AFTER `prev_top_plan_id.insert(...)` — the exact bug the assertion message describes (the pre-event top becoming the post-pop top, degrading the guard to blind) — would still pass the test.

**Suggested fix**: pin the ordering concretely — e.g. locate `let prev_top = prev_top_plan_id.get` and `prev_top_plan_id.insert` in the forwarder source and assert the read's index < the insert's index (both < note_site).

### LOW-4: The plan's checked BUG-memory step has no findable record

**Location**: .coding/plans/6c6f985d.md step 2 (checked: "memory_write a BUG: record … (bba2c82d)").

**Evidence**: three targeted memory searches (semantic query, `record_type:"bug"` + `BUG:` prefix, exact-title query) surface only the PLAN record for bba2c82d — no BUG: record exists, and it is not in the auto-recalled set. Either the write did not happen or it is expected to be auto-captured at finish. The closing sequence for bug plans requires the BUG memory (symptom → root cause → fix + regression test name) to be written — the parent should write/verify it before `finish`.

## Notes (verified, not findings)

- **Single-abandonment-per-turn limitation**: two root abandons in one turn (abandon P, create Q, abandon Q) leave the latch holding only Q, so an item whose plan was P would be blocked. This mirrors the Done guard's identical limitation (the live top read at resolution) and is exotic; consistent with the prior art's precision.
- **TOCTOU between the `may_flip` read and the `transition`** (two separate store lock acquisitions in the spawned/run-all arms): mirrors `may_flip_done`'s established shape; the guarded damage class (a stale pointer with no concurrent writer) is unaffected.
- **Test coverage**: `plan_linkage_allows_failed_matrix` covers the full acceptance (Pending stays Pending regardless of linkage; InFlight+matching flips; mismatched/None/blind cases; terminal statuses never re-flip); `failed_transitions_are_guarded_on_status_and_plan_linkage` pins the guard counts (≥2 spawned, ≥3 main), the `plan_abandoned && may_flip_failed` row conditions (≥2), and the no-transition diagnostic in both bodies; `abandonment_evidence_captures_the_abandoned_plan_id` pins the tracking map's existence (its ordering pin is LOW-3); `abandoned_plan_id_round_trips_and_clears_per_turn` covers the latch lifecycle (round-trip, blind None, per-turn and exit clears, agent independence).
- **`.coding/backlog.jsonl`**: the in_flight stamping is the app's own write, as stated — expected, not hand-edited.
