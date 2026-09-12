# Round 3 Review — feat/reviewer-report-instructions

Date: 2026-04-04
Reviewer: read-only reviewer subagent (spawn_agent)
Scope: **all** uncommitted changes in the working tree (`git status` / `git diff HEAD`),
per the agent.md closing-sequence rule. This includes this round's four fixes (m1, m2,
n1, n2) **and** the still-uncommitted previous-round changes (setRightPanelTab guard,
child_finished name recording, AgentInfo.parent_id Rust+TS, agentParents map, agent.md
never-defer rule + concrete filename example, Sidebar wrapper removal). Reviewing
everything is expected; nothing was skipped.

Verification already done (per task): `cargo test` 392+9+5 pass, `tsc --noEmit` clean.
This review is a static read of the diff plus surrounding source for correctness,
races, reactivity, and constitution compliance.

---

## Major

No findings.

---

## Minor

No findings.

---

## Nit

- **N1 — formatting: two interface members on one line.**
  `frontend/src/hooks/useAgentStore.ts:343`
  ```ts
  mainAgentId: () => AgentId | null;  setModel: (m: string) => void;
  ```
  The `mainAgentId` declaration and `setModel` were joined onto a single line (an
  artifact of the n1 edit inserting `mainAgentId` where `setModel` previously began).
  It is valid TypeScript (semicolon-separated) and `tsc --noEmit` is clean, but every
  other member in `AppState` is on its own line. Cosmetic only; splitting
  `setModel` onto the next line would match the file's prevailing style. No
  behavioural impact.

---

## Verification of the four targeted fixes

**m1 — `nameResolutionInFlight` guard (useAgentEvents.ts:39-41, 106-117).** Sound.
The id is added to the module-level `Set` *before* the `await`/`listAgents()` call,
and removed in `.finally()`, so the guard is held for the whole round-trip regardless
of resolve/reject. A streaming unknown agent therefore fires at most one IPC per
round-trip instead of one per token. Because the Set is module-level it survives
StrictMode remounts (matching the listener-singleton rationale documented at the top
of the file). `.catch` logs and swallows, so a failed lookup does not leave the id
stuck — `.finally` still deletes it, allowing a later retry. No leak, no race.

**m2 — unreachable `break` removed (useAgentStore.ts child_finished).** The case now
ends in a `return { ... }` (lines 1155-1159); the dead `break` is gone. Correct.

**n1 — `selectMainAgentId` extracted (useAgentStore.ts:429-447, 606;
BacklogView.tsx:217-220).** The pure helper is exported and documented; both the
imperative `mainAgentId()` store action (line 606) and BacklogView's reactive
selector delegate to it, so the algorithm lives in exactly one place. The algorithm
(smallest id among `parent_id === null`, falling back to smallest known id) matches
the backend's own main-agent selection in `src/runtime/mod.rs:117-127`
(`.filter(|h| h.parent_id.is_none())` + smallest id), so frontend and backend agree.

**n2 — doc-comment alignment.** All three sites now say "parentless agent with the
smallest id": useAgentStore.ts (agentParents field doc, lines ~263-266),
frontend/src/lib/types.ts (AgentInfo.parent_id), and src-tauri/src/ipc/commands.rs
(AgentInfo.parent_id, lines 86-89). Consistent.

---

## Cross-cutting checks

**Reactivity (BacklogView selector).** Correct. The selector reads `s.agentParents`,
`s.agents`, and `s.workflowStates[mainId]` inside the selector body, so zustand
re-runs it on every store change. The returned value is `string | null`
(`s.workflowStates[mainId] ?? null`); zustand's default equality is `Object.is` on
the returned value, so the component re-renders only when the resolved phase string
actually changes — not on unrelated store updates. Reading the maps inline (rather
than via the non-reactive `mainAgentId()` action) is the right call and is documented
in the comment.

**`child_finished` keying.** `handleAgentEvent: ({ agent_id, event })` (line 817). The
backend `emit_child_finished` sets `agent_id: child_id`
(src-tauri/src/ipc/events.rs:243), so `agent_id === event.child_id` holds; marking
`agents[agent_id]` not-running and recording `agentNames[event.child_id] = event.name`
both target the same (child) agent. `AgentId = number` and `child_id: number` are
type-consistent.

**`registerAgents` type widening (`{id,name}[]` → `AgentInfo[]`).** Safe. Both callers
pass the full `listAgents()` result: `App.tsx:86` (`registerAgents(agents)`) and
`useAgentEvents.ts:114` (`registerAgents(infos)`). No caller constructs a partial
object, so the wider type is satisfied everywhere.

**`parent_id` plumbing.** `list_agents` maps `h.parent_id` (commands.rs:223);
UI-spawned `spawn_agent` returns `parent_id: None` (commands.rs:261); tool-spawned
agents get `Some(parent)` via `spawn_with_parent`. Frontend `AgentInfo.parent_id`
(types.ts) mirrors the Rust field. Consistent end to end.

**Constitution compliance.** Doc comments present on all new public/exported items:
`selectMainAgentId` (useAgentStore.ts:429-436), the `mainAgentId` action (340-342),
the `agentParents` field, the `dispatch` name-resolution block (useAgentEvents.ts:101-105),
and the Rust `parent_id` field. No shell commands were run by this reviewer; no
Linux-path violations introduced in the diff.

---

## Verdict

The diff is correct and cohesive. All four targeted fixes (m1, m2, n1, n2) are
properly implemented and verified, and the previous round's changes still hold
together as a whole. The in-flight Set guard is race-free, and the BacklogView
selector is reactivity-correct.

**One Nit finding (N1, formatting only).** No Major or Minor findings. Safe to commit
once the nit is addressed or consciously accepted.
