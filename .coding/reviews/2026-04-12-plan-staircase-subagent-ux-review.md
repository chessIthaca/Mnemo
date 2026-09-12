# Review — Plan staircase, subagent close-x, model label, backlog edit, approval auto-switch, cleanup-on-spawn

**Date:** 2026-04-12
**Reviewer:** read-only subagent
**Scope:** all uncommitted changes (`git diff HEAD`) — 24 files.

## Summary

The diff implements six UI/UX improvements. The core logic is correct, the
concurrency reasoning (lock ordering, FIFO cleanup, trigger-agent exclusion)
is sound, contract fixtures are consistent, line endings are clean CRLF
throughout, and all new public Rust functions carry doc comments.

**One unrelated change** is bundled in the diff: a major version bump of
`vite` (5 → 8) and `vitest` (2 → 4) in `frontend/package.json` (+ the
regenerated `package-lock.json`, ~1787 lines of churn). This is outside the
scope of the six-feature plan and should be justified or reverted before
committing — see Constitution finding C1.

---

## Correctness

### C1 (info) — `cleanup_inactive_subagents` trigger-agent argument is correct
`spawn.rs:101` passes the not-yet-registered `agent_id` as `trigger_agent`.
The filter (`events.rs:486`: `h.id != trigger_agent && h.parent_id.is_some()
&& !h.is_running()`) excludes it by id. Since `mgr.next_id()` allocates a
fresh monotonic id guaranteed ahead of all existing ids (`backlog.rs:86`:
`next_id = file.next_id.max(max_id + 1)`), the new id cannot collide with an
existing inactive subagent. Passing it is harmless and defensive. ✅

### C2 (info) — No race between cleanup and new-agent registration
`cleanup_inactive_subagents` only enqueues `Cancel` commands (non-blocking
`try_send`); it does not wait for `Exited`. `spawn_agent_shared` then
registers the new agent under the manager lock. The cancelled subagents'
`Finished`→`Exited` events are processed later by the single-threaded
forwarder, which removes them from the manager + `agent_loops` map. The new
agent is registered immediately after cleanup returns, so there is no
interference. ✅

### C3 (info) — `list_agents` nested lock (manager → agent_loops) is deadlock-free
`agent.rs:260-261` now acquires `manager` then `agent_loops` simultaneously.
Verified no path acquires them in reverse order: every other `agent_loops`
lock site (`agent.rs:373,392,449,511`, `events.rs:325`, `run_all.rs:92`,
`settings.rs:430`, `spawn.rs:130`) holds `agent_loops` alone, and the
forwarder's `Exited` arm (`events.rs:322-325`) locks `manager`, drops it,
then locks `agent_loops` (sequential, not nested). Lock ordering is
consistently `manager → agent_loops`. No cycle. ✅

### C4 (info) — `agentModels` store logic is correct
`useAgentStore.ts:445-455`: spreads into a new object, overwrites only when
`info.model` is truthy (`if (info.model)`), so an absent model (agent exited,
field omitted by `skip_serializing_if`) preserves the prior value, and a
present model always wins. Exit-removal (`agentEventReducer.ts:574-575`)
deletes from `agentModels` alongside `agents`/`agentNames`/`agentParents`/
`workflowStates`. The new test (`useAgentStore.test.ts:335-360`) covers both
the absent-model-preserves-prior and present-model-overwrites cases. ✅

### C5 (info) — Backlog `edit` preserves status/note/created_at
`backlog.rs:174-183`: mutates only `text` + `images`, leaving `status`,
`note`, `created_at`, and `id` untouched. Persists via the existing atomic
write. Unknown id returns `false` (no-op, no persist). The test
(`backlog.rs:374-400`) verifies text+images change, status+note survive,
reopening reads the edit, and unknown id is a no-op. ✅

### C6 (info) — Close-x uses `cancel` (correct vs `interrupt`)
`MainPanel.tsx:154` calls `cancel(t.id)`. `cancel` sends `AgentCommand::Cancel`
→ agent emits `Finished` then `Exited` → forwarder removes the agent → tab
disappears. `interrupt` would only stop the current generation, leaving the
agent alive and the tab present. For "close tab", `cancel` is the right
choice. `stopPropagation` + `preventDefault` correctly prevent the Radix
`TabsTrigger` `onValueChange` from firing (which would switch tabs). ✅

### C7 (info) — Contract fixture is consistent
Rust `AgentInfo` has `model: Option<String>` with
`#[serde(skip_serializing_if = "Option::is_none")]` (`agent.rs:107-108`).
The fixture struct (`contract_fixtures.rs:91`) sets `model: Some("gpt-5")`,
which serializes as `"model": "gpt-5"`. The JSON fixture
(`dto-agent-info.json`) contains `"model": "gpt-5"`. The TS type
(`types.ts:144-147`) is `model?: string | null`. The contract test
(`ipc-contract.test.ts:256`) asserts `typeof dtoAgentInfo.model === "string"`.
All three sides agree. The fixture now ends with a trailing newline (the
`\ No newline at end of file` marker was removed), matching the other
fixtures. ✅

---

## Bugs

### B1 (low) — Approval auto-switch can thrash focus between two subagents
`useAgentEvents.ts:259-263`: on every `approval_request` for a subagent that
is not active, it calls `setActiveAgent(agent_id)`. If subagent A is active
and subagent B hits an approval, focus switches to B. If the user switches
back to A and B hits another approval (a second tool call in a later turn),
focus switches to B again. Each switch is triggered by a genuine approval
need, so this is arguably correct UX, but with two subagents alternating
approvals it can feel like focus thrashing. The comment at lines 256-258
acknowledges the same-turn case but not the cross-turn case. This is a UX
judgment call, not a correctness defect — the user does need to see each
approval. No fix required; noted for awareness.

### B2 (low) — Auto-switch no-ops if `agentParents` not yet populated
`useAgentEvents.ts:260` reads `st.agentParents[payload.agent_id]` to decide
if the agent is a subagent. `agentParents` is populated only by
`registerAgents` (from `listAgents`, called on mount + periodically). If a
subagent's `approval_request` arrives before `listAgents` has run (e.g. a
tool-spawned agent that immediately hits an approval on a fresh app load),
`agentParents[id]` is `undefined`, so `isSubagent` is false and no
auto-switch occurs. The fallback is the existing "Switch & review" banner
(derived from `tabs` via `s.agents`, which `getOrCreate` populates on the
event), so the approval is still surfaced — just not auto-focused. Minor
timing edge; acceptable given `listAgents` runs on mount.

---

## Security

No security findings. The backlog `edit` command writes to the same
`.coding/backlog.json` via the same atomic persist path as every other
backlog mutation — no new file-write surface. The `backlog_edit` Tauri
command takes `id`/`text`/`images` from the frontend (UI→Tauri, no approval
gate, consistent with the other backlog commands per the constitution). The
close-x `cancel` call targets an agent id already known to the frontend.
No untrusted input reaches a shell or file path.

---

## Constitution compliance

### C1 (medium) — Unrelated dependency bump bundled in the diff
`frontend/package.json` bumps `vite` 5.4.21 → 8.2.1 and `vitest` 2.1.9 →
4.1.10 (devDependencies), with a regenerated `package-lock.json` (~1787
lines of churn). This is a **major** version jump (vite 5→8 spans three
major releases) and is entirely unrelated to the six-feature plan. The
constitution says "Follow the existing code style" and the plan's scope was
the six UI/UX improvements. A dependency bump of this magnitude:
- May break the build or test runner (vite 8 / vitest 4 have breaking
  changes vs 5 / 2).
- Was not part of any plan step.
- Inflates the diff and commit with unrelated churn.

**Recommendation:** revert `frontend/package.json` and `package-lock.json`
to HEAD before committing, OR split into a separate commit with its own
justification + a verified `npm install` / `npm test` run. Do not silently
bundle it with the feature work.

### C2 (info) — All new public Rust functions have doc comments
- `BacklogStore::edit` (`backlog.rs:171-173`): doc comment ✅
- `backlog_edit` Tauri command (`backlog_cmds.rs:153-157`): doc comment ✅
- `cleanup_inactive_subagents` visibility changed `async fn` → `pub(crate)
  async fn` (`events.rs:478`); it already had a doc comment, now updated to
  document the new spawn-time caller ✅
- `AgentInfo::model` field (`agent.rs:103-106`): doc comment ✅

### C3 (info) — Line-ending style preserved
All 13 modified source files are consistently CRLF (Windows style) with
zero mixed `\r\n`/`\n` endings (verified by byte scan). `git diff --check`
reports no whitespace errors. The `dto-agent-info.json` fixture gained a
trailing newline, consistent with sibling fixtures. The `.coding/backlog.json`
LF→CRLF warning is pre-existing gitattributes behavior on `.coding/` files,
not introduced by this diff.

### C4 (info) — No commit to main
The diff is uncommitted in the working tree on a feature branch. No `git
merge`/`git push` to main was performed. ✅

---

## Verdict

The six features are correctly implemented and constitution-compliant. The
**only actionable finding is C1**: the unrelated `vite`/`vitest` major
version bump in `frontend/package.json` + `package-lock.json` should be
reverted or split out before committing. B1 and B2 are low-severity UX
timing notes that do not require changes.
