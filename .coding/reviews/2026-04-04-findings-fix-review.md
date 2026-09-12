# Review: Findings-fix round (M2, M3, M4, N1, N3) + never-defer rule

**Date:** 2026-04-04
**Branch:** `feat/reviewer-report-instructions`
**Reviewer:** read-only reviewer subagent (spawn_agent)
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`).

This round's diff fixes the 4 Minor + 3 Nit findings from the prior review
(`.coding/reviews/reviewer-instructions-review.md`) and hardens agent.md step 3
with a "never defer review findings" rule. Verification already done by the main
agent: `cargo test` passes (392+9+5, 0 failures); `tsc --noEmit` clean.

---

## Major

None.

---

## Minor

### m1 — `dispatch()` fires `listAgents()` once per event for an unknown agent (no in-flight guard)
**File:** `frontend/src/hooks/useAgentEvents.ts:101-107`

The new name-resolution check runs for **every** payload, including
`text_delta`:

```ts
if (st.agentNames[payload.agent_id] === undefined) {
  void listAgents().then(...).catch(...);
}
```

`registerAgents` only runs after the async `listAgents()` round-trips. Until
then `agentNames[id]` is still `undefined`, so a newly-spawned agent that
immediately starts streaming fires **one `listAgents()` IPC per token** for the
duration of the first round-trip. There is no in-flight guard or shared promise,
and no dedup in `lib/tauri.ts:91`.

**Impact:** not a correctness bug — the outcome is eventually consistent and
`list_agents` is cheap/read-only. It's an efficiency / IPC-hammer concern. A
burst of dozens of redundant IPC calls per new streaming agent is realistic.

**Suggested fix (not blocking):** track in-flight requests, e.g. a module-level
`const pending = new Set<AgentId>()` — skip the invoke if `id` is already in
`pending`, add before the call, and delete in `.finally()`. Or hoist a single
shared `listAgents()` promise.

### m2 — Dead `break` after `return` in the `child_finished` case
**File:** `frontend/src/hooks/useAgentStore.ts:1146-1151`

```ts
return {
  ...s,
  agents: { ...s.agents, [agent_id]: next },
  agentNames: { ...s.agentNames, [event.child_id]: event.name },
};
break;   // ← unreachable
```

The `break` on line 1151 is unreachable (the `return` exits first). `tsc` is
clean only because `allowUnreachableCode` defaults to permissive. Harmless but
dead code; the `break` should be removed for clarity. (Note: this same
return-then-break pattern appears elsewhere in the switch, e.g. the `error` case
at 1119-1124 — but that one correctly has no trailing `break`. Only the new
`child_finished` case introduced the dead `break`.)

---

## Nit

### n1 — `mainAgentId()` store action is dead code; logic duplicated inline in BacklogView
**Files:** `frontend/src/hooks/useAgentStore.ts:586-599` and
`frontend/src/components/views/BacklogView.tsx:212-224`

`mainAgentId()` is defined in the store but **never called** anywhere (confirmed
via search: only the interface decl at :342 and the impl at :586 reference it).
`BacklogView` re-implements the identical "smallest parentless id, fallback
smallest known id" algorithm inline inside its selector.

The duplication has a real justification (a zustand selector must read
`agentParents`/`agents` inside the selector body to stay reactive; calling
`get()` via the action would not subscribe to changes). But shipping an unused
store action + a hand-duplicated copy of its algorithm is a smell: the two can
drift. **Suggested fix (optional):** extract a pure helper
`selectMainAgentId(agentParents, agents)` and call it from both the selector and
the store action — keeps reactivity and removes the duplication. At minimum,
either use `mainAgentId()` somewhere or drop it to avoid dead API surface.

### n2 — "the only parentless one" doc comments are slightly inaccurate
**Files:** `frontend/src/hooks/useAgentStore.ts:266-268`,
`frontend/src/lib/types.ts:134-135`, `src-tauri/src/ipc/commands.rs:86-88`

All three doc comments assert the main agent is "the only parentless one." That
is not true once the user spawns an agent via the UI button: the IPC
`spawn_agent` returns `parent_id: None` (commands.rs:261) and `spawn_agent_shared`
only sets a parent for tool-spawned agents (`with_parent`, commands.rs:301-303).
So there can be **multiple** parentless agents (main + UI-spawned).

The selection is still **correct** because both the frontend `mainAgentId()` and
the backend `main_agent_id()` (src/runtime/mod.rs:124-130) take `Math.min` /
`.min()` over parentless ids, and the main agent is registered first with the
smallest id (main.rs:95-104). So behaviour matches intent; only the wording is
wrong. The backend's own doc (src/runtime/mod.rs:115-119) gets it right
("no parent ... and the smallest id"). Consider aligning the three new comments
to say "smallest-id parentless agent" rather than "the only parentless one."

---

## Things verified correct (no action needed)

- **M2 (disabled-tab no-op):** `setRightPanelTab` now guards
  `s.disabledTabs.includes(tab)`. Consistent with RightPanel.tsx which only
  renders enabled tabs as clickable and otherwise falls back to the first
  enabled tab (RightPanel.tsx:32-34). The store guard is correct defense-in-depth
  for other callers (e.g. `toggleTabAndReveal`). Correct.
- **M3 (`child_finished` name-recording):** payload contract confirmed — for
  `child_finished`, `events.rs:243` sets `agent_id: child_id`, so
  `agentNames[event.child_id]` targets the right agent. TS type has
  `name: string` (types.ts:125). No conflict with the async listAgents refresh
  (last-write-wins, same value). Correct.
- **Type contract `AgentInfo`:** Rust `parent_id: Option<AgentId>` serializes to
  `number | null`, matching TS `parent_id: AgentId | null` (`AgentId = number`).
  Serde snake_case field naming matches. Consistent.
- **`registerAgents` signature change** (`{id;name}[]` → `AgentInfo[]`): all
  callers pass the `listAgents()` result directly (App.tsx:86,
  useAgentEvents.ts:105), which is `AgentInfo[]`. No caller breaks.
- **Sidebar N3:** `handleToggleTool` wrapper removed; `onClick` calls
  `toggleTabAndReveal(tab)` directly. Behaviour unchanged. Correct.
- **agent.md N1 + never-defer rule:** concrete report filename example; step 3
  now hard-rules never deferring findings. Matches the stated intent.
- **Doc comments (constitution):** new/changed public functions and the new Rust
  field all carry doc comments. Compliant.
- **`mainAgentId()` algorithm == backend `main_agent_id()`:** both select the
  smallest parentless id. Semantically consistent. Correct.

---

## Verdict

All seven targeted findings (M2, M3, M4, N1, N2, N3) are correctly fixed, and the
never-defer rule is properly written into agent.md. No Major issues. Two Minor
issues introduced by this round: (m1) the un-guarded per-event `listAgents()` IPC
hammer for unknown streaming agents, and (m2) an unreachable `break` in the new
`child_finished` case. Two Nits: (n1) dead/duplicated `mainAgentId()` logic, and
(n2) the slightly-inaccurate "only parentless one" doc wording. None block
commit; m2 (dead `break`) is the cheapest to fix and worth doing now.
