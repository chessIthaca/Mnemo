## Verdict: FINDINGS (0 high, 1 low)

**Summary:** The ordering fix is correct and safe in every path I could trace — parentless agents, multi-turn children, the double clear, lock discipline, and the cleanup/run-all side effects — and the three regression tests genuinely pin the happens-before deterministically. One low, process-level finding: the BUG: memory record required by the bug-plan checklist is not findable in the memory store, although plan step 2 is checked as done.

**Scope reviewed** — all uncommitted changes on `wt/agenticcoding` (`git diff HEAD` + untracked plan file):
- `src-tauri/src/ipc/events.rs` (+159): the fix line, doc comments (module + function), and the 3-test regression module.
- `src/runtime/mod.rs` (+8): `DescendantTracker` trait doc (update-timing invariant).
- `.coding/backlog.jsonl`: run-all bookkeeping for item 2c406d72 (steer-halt note + plan_id/plan_title, status stays `pending` — consistent with "returned to queue"; the normal plan-completion flow stamps it) + one unrelated user-requested addition (51bab4da). Status text only, no code.
- `.coding/plans/c8c49338.md` (untracked): root cause traced, steps 1-3 checked.

Verified against the live code: the forwarder loop (notification match events.rs:520-544 runs before the running-state match :557-682), `notify_parent_on_completion` (:788-871), `AgentManager::set_running`/`has_running_descendants` (runtime/mod.rs:117-121, 150-154), `AgentHandle` (channels.rs:820-882), the console counterpart (console.rs:694-710), the dispatch gate (dispatch.rs:180+), and the gate's consult path `IpcSpawner::has_running_descendants` (spawn.rs:522-525).

### 1. Ordering fix — correct in all paths

- **Parentless agents (main/UI turns):** the new clear at events.rs:800 runs before the `parent_id` check (:804-819), so a parentless agent gets its flag cleared at the top and the function returns `None` — the forwarder's running-state match then clears again in the *same loop iteration* (:605). No observable change: the flag was always cleared within this iteration; the only widening is a microsecond-scale idle-read window (two brief lock acquisitions) that exists **only on an agent's first finish** (`notified_children` dedup at :524/:534 inserts once per id) and sits inside the already-documented accepted residual ("the forwarder flips `running` only when it processes the event" — 2026-08-20 run-all review). Dismissed, no action.
- **Multi-turn children:** the next `Started` re-marks running (:558-563). Later `Finished` events skip `notify_parent_on_completion` entirely (dedup) — but they also send no `Suggestion`, so no notification race exists for them; the running-state match clears in the same iteration. Sound.
- **Double clear (idempotency/race):** `set_running` is a bare `AtomicBool::store` (channels.rs:878-881) — idempotent. The only `set_running(true)` writer in the IPC path is the forwarder's own `Started` arm (:562) — the same task, and events are processed one at a time, so nothing can interleave between the two clears. `console.rs` (the only other production caller, :687) runs in the separate console mode with its own `AgentManager` — never concurrent with the Tauri forwarder. Race-free.
- **Consistency bonus:** the console path (`console.rs on_turn_ended`, :694-710) *already* cleared the flag before notifying the parent — the fix unifies both frontends on the same invariant.

### 2. Lock discipline — no deadlock

The function now takes the manager lock three times (:800, :804, :862) plus the `agent_loops` lock twice (:828, :845) — all **sequential, never nested, never held across an await** (`set_running` is a sync atomic store; the loops reads clone a String). The new acquisition at :800 is a standalone statement (guard dropped at statement end). Both call sites (forwarder notification match, :526/:536) hold no locks when calling — no re-entrancy (tokio Mutex is non-reentrant, but there is no nesting). Cost: one extra brief lock per agent lifetime (first finish only, via the dedup) — negligible against the structural-event path.

### 3. Side effects of clearing earlier

- **`cleanup_inactive_subagents`** (events.rs:917-934, filter `!h.is_running()` at :925): the earlier clear means a just-finished child now *reliably* counts as inactive when the parent — woken by the `Suggestion` — spawns its next agent. That is the documented intent ("completed (inactive) subagents are cleaned up when a new agent is spawned"); previously the race could let a finished child escape cleanup and linger as a phantom "running" tab. Correct direction; a genuinely running child is still never touched.
- **Run-all resolution:** `turn_resolve.on_finished(agent_id, descendants_running)` computes `descendants_running` at :607, *after* the clear at :605 — in both old and new code, within the same iteration. The main's own flag never counts toward `has_running_descendants(main)` (runtime/mod.rs:150-154 walks the `parent_id` chain of *other* agents). The child's `Finished` else-branch (`try_flush_deferred_main_resolution`, :639-644) runs after the clear either way. No observable change beyond closing the race; the run-all dispatch busy guard now correctly proceeds past a verifiably-finished child.

### 4. Regression tests — genuine and deterministic

- All three tests call `notify_parent_on_completion` directly — the exact changed path. **A/B claim verified by construction:** each test asserts `!has_running_descendants(parent)` at/after `Suggestion` receipt; without events.rs:800 the only clear lives in the forwarder's running-state match, which these direct-call tests never execute — the child stays marked running and all three assertions fail. Sound.
- **`tracker_agrees_at_notification_receipt` is not flaky:** the `set_running(false)` store precedes the `try_send` in program order on the notify task; the test's `recv()` observing the message establishes happens-before, so the store is visible at the check. The parent channel (capacity 8) is empty, so the best-effort `try_send` cannot drop. Deterministic.
- Fixtures check out against the real signatures: `AgentHandle::new`/`with_parent` (channels.rs:846-863), `AgentLoopMap` = `Arc<tokio::sync::Mutex<HashMap<AgentId, Arc<AgentLoop>>>>` (state.rs:91-92 — the empty map type-checks), imports all resolve via `super::*` + the explicit `use mnemo::runtime::AgentHandle`. Exactly one `mod tests` in the file (:1442 — no duplicate). `notify_parent_on_completion` has no other callers besides the two forwarder arms and the tests (graph-verified).
- Minor, no action: the tests exercise the function rather than the full forwarder loop (which would need a Tauri `AppHandle`) — the repo's established pattern for events.rs-adjacent logic.

### 5. Bug-plan checks

- Regression test exercises the changed path: **yes** (direct calls).
- Root cause documented: **yes** — function doc ("Tracker invariant", events.rs:764-779), events.rs module doc, `DescendantTracker` trait doc (runtime/mod.rs:269-275), and the plan file's Context section (precise line-level trace).
- BUG: memory record: **not found** — see finding LOW-1.

### 6. Constitution

- **Doc comments:** the public `DescendantTracker` trait doc is updated; module/function/test-module docs added. ✓
- **Multi-platform neutrality:** no `cfg(windows)`, no paths, no shell syntax in the diff. ✓
- **Documentation sync:** README.md and PLAN.md searched (descendant/tracker/subagent/running flag) — they document the reviewer/subagent model, review flow, and model routing; nothing describes tracker update timing or notification ordering. The "no user-facing docs affected" judgment is **verified correct**; the right docs (module/function/trait) were updated. ✓
- **Tests/warnings:** this reviewer is read-only and cannot re-run `cargo test`; the parent reports root crate 2025 passed / src-tauri 231 passed, both exit=0 and warning-free under `#![deny(warnings)]`. The diff introduces no warning sources (all new items used; no unused imports; single test module).

### Findings

**LOW-1 — BUG: memory record not findable (bug-plan closing check).** Plan step 2 is checked ("memory_write a BUG: record … ≤600 chars"), but two `memory_search` queries (record_type `bug` with "descendant tracker lags session completion complete_step refused…", and "2c406d72 clear running flag before finish notification notify_parent_on_completion events.rs") return no BUG: record for this fix — only the pre-dispatch PLAN digest (3d96f42b) and unrelated BUG records; `.coding/knowledge/bug/` likewise has no file for 2c406d72/c8c49338. **Fix:** `memory_write` the BUG: record before `finish` (symptom: `complete_step` refused for a full turn after every child verifiably ended, live-observed 2026-12-30, plan 72329f2c; root cause: the notification match sent the completion `Suggestion` before the running-state match cleared the flag, so the woken parent hit the descendant gate on a still-marked child; fix: events.rs:800 clears first; regression tests: the three test names). The finish-time auto-capture may cover it, but the checked step and the store currently disagree — make them agree.

### Considered and dismissed (no action)

1. Parentless early-clear window widening (see §1 — first-finish-only, microseconds, inside the documented accepted residual; the dispatch guard's post-checkpoint re-check covers it).
2. Full-forwarder test coverage (see §4 — `AppHandle` not constructible in unit tests; function-level is the established pattern).
3. Multi-turn second-finish timing (see §1 — no notification, no race; same-iteration clear).
