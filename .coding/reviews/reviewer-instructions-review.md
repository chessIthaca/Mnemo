# Review: Patch reviewer instructions in agent.md (+ all uncommitted changes)

Date: 2026-04-04
Reviewer: read-only reviewer subagent (spawn_agent)
Scope: **all** uncommitted changes in the working tree (`git status` / `git diff HEAD`), per the
updated agent.md closing-sequence instructions — the agent.md constitution patch (this plan's
change), the leftover frontend work from earlier plans, and the runtime/config state files.

---

## agent.md (M) — constitution patch (this plan's change)

The new "Standard plan closing sequence" section (agent.md:34–62) is internally consistent and
unambiguous:

- Step 2 (Review) explicitly directs the reviewer to review **all** uncommitted changes including
  those from other plans/sessions, forbids complaining about or skipping out-of-task changes,
  requires a self-contained task prompt, and mandates writing findings to
  `.coding/reviews/<plan-or-date>-review.md` as the reviewer's single allowed write.
- Step 3 (Act on findings) tells the main agent to read the report, address or justify-skipping
  findings, re-run `cargo test` if code changed, and include the report in the commit.
- Step 4 (Commit) requires committing to the current feature branch with a clear message.

Steps 2–4 form a coherent sequence: review → act → commit. No contradictions with the pre-existing
constitution rules (Windows paths, doc comments, no main commits).

### Minor — M1: branch rule gap in step 4
- **agent.md:60** — Step 4 says "commit the changes with `git` to the current feature branch," but
  the repo is currently on `main` (`git branch --show-current` → `main`). The pre-existing rule
  "Never commit directly to the main branch" (agent.md:32) covers this, and step 4 does say
  "Never commit to the main branch (per the rule above)," so the text is compliant — but the
  wording assumes a feature branch already exists. The executing agent must create/switch to a
  feature branch before committing this plan's changes. (Action for the main agent, not a doc bug.)

### Nit — N1
- **agent.md:52** — "`.coding/reviews/<plan-or-date>-review.md`" placeholder syntax is fine, but no
  example of a concrete filename; harmless.

---

## Frontend leftover work (M) — agents-to-top-bar + per-tool sidebar toggles + per-agent workflow state

Reviewed as one feature cluster (plan dd2d2963 "Agents to top bar + per-tool left sidebar toggles",
plan 9aed074a "Per-agent workflow state", plan 6bf70732 "InflightBar collapsed by default",
plan 108b9212 "Auto-show/hide tools panel").

### Correctness — verified
- `useAgentStore.ts:561–621` — `setWorkflowState`/`setAgentName`/`registerAgents`/`toggleTab`/
  `toggleTabAndReveal`/`isTabEnabled` all have doc comments (constitution-compliant) and correct
  immutable-update logic. `toggleTabAndReveal` correctly: falls back to first enabled tab when the
  active tab is disabled, reveals+selects on enable, hides the panel when the last tool is disabled.
- `useAgentStore.ts:997–1006` — `workflow_state_changed` now records per-agent
  (`workflowStates[agent_id]`) instead of a single global; matches the per-agent design and the
  StatusBar/BacklogView readers (`workflowStates[activeAgent] ?? null`). No stale-global readers
  remain (`workflowState` as a store field is fully removed — search confirms only local component
  variables named `workflowState`).
- `App.tsx:82–99` — `registerAgents(agents)` seeds both state shells and names; `setWorkflowState(id, …)`
  uses the fresh `useAgentStore.getState().activeAgent`, avoiding the stale-closure issue.
- `MainPanel.tsx` — agent tabs render from `agents` + `agentNames` with `agent-<id>` fallback;
  pulsing dot class is defined in `globals.css` (`agent-running-dot`/`agent-pulse` keyframes) and
  respects no global side effects.
- `Sidebar.tsx` — agent switcher and manual spawn button removed; per-tool toggles wired to
  `toggleTabAndReveal`; show/hide-all button disabled when all tools off. `TOOL_META` matches
  `RightPanel.TABS` exactly (all 8 tabs, same icons).
- `RightPanel.tsx:30–34,77–89` — disabled tabs filtered from both the tab bar and the content
  switch; `shownTab` fallback prevents rendering a disabled tool; all-tools-off placeholder added.

### Minor — M2: `setRightPanelTab` does not guard against selecting a disabled tab
- **useAgentStore.ts:583 / RightPanel.tsx:47** — `setRightPanelTab` sets `rightPanelTab`
  unconditionally. Today the only caller path (`RightPanel` tab buttons) renders only enabled tabs,
  so a disabled tab can never be clicked, and `shownTab` also falls back. Safe as-is, but if any
  future caller sets a disabled tab, the content switch silently shows the fallback while
  `rightPanelTab` disagrees — a latent inconsistency. Consider a guard or deriving instead of storing.

### Minor — M3: tool-spawned child agents show as `agent-<id>` in the top bar
- **MainPanel.tsx:24 / useAgentStore.ts** — `agentNames` is only populated at startup from
  `listAgents()` and via `setAgentName`, which has no callers anywhere (search confirms:
  defined, never invoked). Agents spawned at runtime via the `spawn_agent` tool appear through the
  event stream (`getOrCreate` makes a state entry), so they get a tab — but labeled `agent-<id>`
  rather than their name. `setAgentName` is dead code until a runtime event (e.g. a child-spawned
  event carrying the name) calls it. Cosmetic only; flag for a follow-up.

### Minor — M4: backlog comment overstates an assumption
- **BacklogView.tsx:212–214** — comment says the backlog "dispatches to the main agent, which is
  normally the active one," but the selector reads `workflowStates[activeAgent]` — if the user has
  a *subagent* active while the main agent executes a backlog item, the badge falls back to the
  static `in_flight` label instead of the live phase. The comment acknowledges the assumption; the
  behavior is a graceful degradation, not a bug. Would be more correct to track the main agent's id
  explicitly, but low impact.

### Nit — N2
- **RightPanel.tsx:67–69** — pre-existing comment still says "all six views" while there are eight
  tabs (md, plan, diff, output, files, safety, stats, backlog). Not introduced by this diff
  (comment predates it) but adjacent to the changed lines; worth fixing while nearby.

### Nit — N3
- **Sidebar.tsx:45–49** — `handleToggleTool` is a one-line passthrough to `toggleTabAndReveal`;
  the wrapper adds nothing. Harmless.

---

## Runtime/config state files (M)

- **.coding/backlog.json** — valid JSON (parsed OK). Item 2 flipped to `done` (matches the
  agents-to-top-bar feature being implemented), item 6 added ("memory write and backlog are always
  safe… remove the safety checks"), `next_id` bumped 6→7. Consistent, no corruption. No trailing
  newline (pre-existing style).
- **.coding/plans/stack.json** — valid JSON; contains the active plan id
  `358812c7-…` which matches `.coding/plans/358812c7-02c2-421d-adf6-8b48539715d4.md` (this plan).
  Consistent with the Executing workflow state.
- **.coding/safety.toml** — 10 new auto-approve rules appended, all syntactically consistent with
  the existing `[[rule]] tool/pattern` schema. Notable entries: `^spawn_agent:$` and
  `^memory_write:$` — i.e. every `spawn_agent` and `memory_write` call is now auto-approved. These
  match the user's backlog item 6 intent ("memory write… always safe") and were presumably added
  interactively via "Mark Safe". Worth flagging to the user (informational): auto-approving
  `spawn_agent` means the agent can spawn subagents without per-call confirmation.

## Plan files (?? .coding/plans/*.md)

Skimmed all 6 new files: 108b9212 (auto-show/hide tools panel), 358812c7 (this plan),
6bf70732 (InflightBar collapsed), 9aed074a (per-agent workflow state), ad63b4e7 (closing-sequence
constitution), dd2d2963 (agents to top bar). All are well-formed plan documents (Goal/Context/Steps)
consistent with the code changes above. Nothing surprising.

---

## Constitution compliance

- Doc comments on public functions: ✅ all new exported store actions and components documented.
- Windows paths / PowerShell: ✅ no violations introduced (the diff adds no shell code).
- No main-branch commit: ⚠️ informational — HEAD is on `main`; per agent.md:32/60 the closing commit
  must go on a feature branch.
- `cargo test` before step completion: main agent's responsibility (plan step 3 already checked off).

## Security

No new network calls, no injection surfaces, no secret handling. The only security-relevant change
is the safety.toml auto-approve expansion noted above (M-adjacent, informational).

---

## Verdict

**Approve with minors.** The agent.md patch is coherent and does exactly what the plan set out to
do. The frontend cluster is correct and constitution-compliant. Findings: 0 Major, 4 Minor
(M1 branch gap to handle at commit time, M2 latent `setRightPanelTab` guard, M3 dead `setAgentName`
/ unnamed runtime-spawned agents, M4 backlog phase-badge fallback), 3 Nits. None block commit;
M1 is an action item for the closing commit (create a feature branch first).
