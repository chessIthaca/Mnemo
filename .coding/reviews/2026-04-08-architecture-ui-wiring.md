# Architecture Review — UI Wiring (React ↔ Tauri IPC ↔ Rust Runtime)

**Perspective:** User-interface wiring  
**Reviewer:** read-only architecture reviewer  
**Date:** 2026-04-08  
**Scope:** `frontend/src/`, `src-tauri/src/ipc/`, `src/runtime/channels.rs`, `src/workflow/`, related entry points  
**Mode:** Recommendations only — no implementation

---

## Executive summary

The UI wiring is **architecturally sound and mostly complete**. The brain remains fully decoupled via `AgentCommand` / `AgentEvent`; the Tauri adapter correctly holds non-serializable approval oneshots; the frontend mirrors every shipped `SerializableAgentEvent` kind in a pure reducer pipeline; backlog/run-all is end-to-end wired with idle + descendant checks; multi-agent tabs, per-agent workflow, and parent completion notifications work.

The largest gaps are **product-contract drift** and a few **multi-agent correctness holes**:

1. **`deny_all` is typed and backend-supported but has no UI control.**
2. **`ApprovalPreview` (Rust-computed unified diff) is never consumed** — the UI rebuilds diffs from raw tool args instead.
3. **Approvals and diffs are scoped to the active agent tab**, so a background agent’s approval can sit invisible until the user switches tabs.
4. **`spawnAgent` / `cancel` are exported but unused in the UI** (spawn only via agent tool; no kill-tab control).
5. **Right-panel defaults and auto-show behavior diverge from `PLAN.md`** (Plan open by default; diff does not auto-show on approval).
6. **Safety mode has dual writers** (StatusBar runtime toggle vs Settings → config.toml) that are mostly synced but easy to desync on partial failures.
7. **Type drift risk is real**: hand-maintained TS mirrors of Rust serde shapes with no codegen or contract tests at the boundary.

Nothing found rises to “the app can’t run a turn,” but several issues will confuse multi-agent and unattended-overnight use.

---

## End-to-end wiring diagram(s)

### Primary prompt path

```text
┌─────────────────────────── Frontend (React) ───────────────────────────┐
│ InputBar.handleSend()                                                   │
│  • optimistic user TranscriptEntry                                      │
│  • sendPrompt(activeAgent, text, images)  ──invoke──►                   │
│                                                                         │
│ useAgentEvents (module singleton listen "agent://event")                │
│  • text_delta → rAF batch → appendStreamingText                         │
│  • other → flushBuffers → handleAgentEvent (Zustand reducers)           │
│  • Conversation / InflightBar / RightPanel re-render from store         │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │ Tauri IPC
                                 ▼
┌─────────────────────────── src-tauri IPC ──────────────────────────────┐
│ commands::send_prompt                                                   │
│  manager.send(agent_id, AgentCommand::Prompt { text, images })          │
│                                                                         │
│ events::spawn forwarder (owns fan-in rx)                                │
│  AgentEvent → into_serializable()                                       │
│  ApprovalRequest → PendingApprovals.insert(agent_id, tcid, oneshot)     │
│  Started/Finished/Exited → running flag / remove / backlog resolve      │
│  app.emit("agent://event", { agent_id, event })                         │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │ channels
                                 ▼
┌─────────────────────────── Brain (lib) ────────────────────────────────┐
│ AgentManager ──cmd mpsc──► AgentTask / AgentLoop                        │
│ AgentLoop ──events──► fan-in ──► forwarder                              │
│ Workflow (per agent) · Tools · Provider · Memory · SafetyRules          │
└─────────────────────────────────────────────────────────────────────────┘
```

### Approval path

```text
AgentLoop needs gate
  → AgentEvent::ApprovalRequest { tool_call_id, tool_name, args, preview, responder }
  → forwarder stores responder in PendingApprovals[(agent_id, tool_call_id)]
  → emit serializable approval_request (NO oneshot)
  → store.reduceApprovalRequest → agent.pendingApproval
  → Conversation shows ApprovalPrompt (only if that agent is active)
  → approve(toolCallId, "approve"|"deny"|"deny_all")
  → PendingApprovals.resolve(tool_call_id) → oneshot.send(Approval)
  → agent continues / denies
  → tool_result clears pendingApproval when ids match
  → Finished/Exited → cleanup_for_agent(agent_id)
```

### Backlog / Run-All path

```text
BacklogView → backlog_add / reorder / dispatch_next / run_all / stop_all
  → BacklogStore (.coding/backlog.json)
  → emit "backlog://changed" { items, auto_feed, run_all }
  → store.applyBacklogChanged

dispatch_next_impl:
  next_pending + main_agent idle + !has_running_descendants
  → Prompt to MAIN only
  → emit PromptDispatched (user bubble in transcript)
  → mark InFlight + single_in_flight id

Run-All:
  git checkpoint → Suggestion(RUN_ALL_STEER) → Prompt
  on_main_turn_resolved(success|error):
    commit / rollback · mark Done|CantResolve · next item
  halt_run_all_for_approval on MAIN ApprovalRequest
```

### Multi-agent spawn path

```text
Agent tool spawn_agent
  → IpcSpawner.spawn_with_parent(name, task, parent_id)
  → factory.build_with_id · register · agent_loops.insert
  → Prompt(task) + emit PromptDispatched
  → events fan-in under new agent_id
  → FE listAgents() on first unknown agent_id → registerAgents (tab appears)
  → child Finished/final Error → Suggestion to parent + ChildFinished event
  → FE marks child idle + “Background task finished/failed”
  → parent Complete→Executing cleanup cancels inactive subagents
```

---

## Command inventory (registered vs used)

All commands are registered in `src-tauri/src/main.rs` (`generate_handler!`, ~185–226) and wrapped in `frontend/src/lib/tauri.ts`.

| Command | Registered | FE wrapper | UI caller | Notes |
|---|---|---|---|---|
| `send_prompt` | ✓ | `sendPrompt` | `InputBar` | Active agent only |
| `send_suggestion` | ✓ | `sendSuggestion` | `InputBar` (steer while running) | |
| `interrupt` | ✓ | `interrupt` | `InputBar` Stop + `/clear` | |
| `cancel` | ✓ | `cancel` | **none** | Dead UI path |
| `approve` | ✓ | `approve` | `ApprovalPrompt` | No `deny_all` button |
| `set_safety_mode` / `get_safety_mode` | ✓ | ✓ | `StatusBar`, `ApprovalPrompt`, `App` mount | Runtime `RwLock` |
| `get/save_safety_rules`, `add_safety_rule` | ✓ | ✓ | Safety tab, Mark Safe | |
| `get_startup_error` | ✓ | ✓ | `App` | Error screen |
| `list_agents` | ✓ | ✓ | `App`, `useAgentEvents` name resolve | |
| `spawn_agent` | ✓ | `spawnAgent` | **none** | Only agent-tool path used |
| `set_model` | ✓ | ✓ | `StatusBar` | Live provider swap |
| `get_workflow_state` | ✓ | ✓ | `PlanProgress`, `StatusBar`, `App` | Poll + event bump |
| `enter_skill` | ✓ | ✓ | Merge-to-main dialog | Emits `SkillStarted` |
| `get_config` | ✓ | ✓ | `App`, `StatusBar` | No secrets |
| `get_settings` / `save_settings` | ✓ | ✓ | Settings dialog | Full non-endpoint patch |
| `get_api_keys` / `list_models` / `save_endpoints` | ✓ | ✓ | Settings → Endpoints | Keys isolated |
| `get_session_stats` / `get_project_stats` / `get_session_list` | ✓ | ✓ | `StatsView` | |
| `read_file` / `list_files` / `list_markdown_files` | ✓ | ✓ | File browser, Md viewer | Sandboxed |
| `get_git_branch` | ✓ | ✓ | `App` poll + focus | |
| `save_conversation` / `load_conversation` | ✓ | ✓ | `/save`, `/load` slash | FE owns transcript JSON |
| `backlog_*` (add/list/remove/reorder/clear/retry/set_auto_feed/dispatch/run_all/stop_all) | ✓ | ✓ | `BacklogView` + `useAgentEvents` list seed | Full coverage |

**Summary:** 41 registered commands; **2 wrappers with zero UI callers** (`cancel`, `spawnAgent`). Everything else has at least one call site.

---

## Event inventory (emitted vs handled)

Channel: `agent://event` (`AGENT_EVENT_CHANNEL`). Payload: `{ agent_id, event: SerializableAgentEvent }` with `#[serde(tag = "kind", rename_all = "snake_case")]`.

Extra channel: `backlog://changed` → `applyBacklogChanged`.

| Event kind | Emitted by | FE handled | Notes |
|---|---|---|---|
| `started` | agent | ✓ `reduceStarted` | Resets per-turn stream/usage |
| `text_delta` | agent | ✓ rAF batch + `appendStreamingText` | Not via main reducer path (by design) |
| `reasoning_delta` | agent | ✓ | InflightBar activity log |
| `tool_call_start` | agent | ✓ | Merge consecutive same-name tools |
| `tool_call_arg_delta` | agent | ✓ | Match by `index` + `result === null` |
| `approval_request` | agent (+ oneshot held) | ✓ | **`preview` field ignored** |
| `tool_result` | agent | ✓ | Clears matching approval; lastDiff snapshot |
| `usage` / `context_usage` | agent | ✓ | InflightBar |
| `workflow_state_changed` | agent | ✓ + autoRevealPlan | planVersion bump |
| `step_completed` | agent | ✓ + autoRevealPlan | planVersion bump |
| `suggestion_injected` | agent | ✓ | Steer landed + TTL remove |
| `prompt_dispatched` | IPC (backlog / IpcSpawner) | ✓ | User bubble for external prompts |
| `skill_started` | IPC `enter_skill` | ✓ | Toolbar skill announcement |
| `finished` | agent | ✓ | running=false; forwarder backlog resolve |
| `exited` | agent | ✓ removeAgent | Tab disappears; fallback main |
| `child_finished` | forwarder (synth) | ✓ | Done note on **child** transcript |
| `error` | agent | ✓ | retrying flag respected |
| `backlog://changed` | backlog mutations | ✓ | items + auto_feed + run_all |

**Dead / underused payloads (not dead events):**

- `ApprovalPreview` on `approval_request` — always present in type, never read in FE.
- `Finished.reason` — received but unused for UX (no length/content-filter messaging).
- `WorkflowStateInfo.depth` / `parents` / `skill` — returned by `get_workflow_state`, barely/not shown in Plan UI (skill overlay weak; sub-plan stack not visualized).

No FE handler is missing for a shipped `SerializableAgentEvent` variant (the `default` arm in the reducer is only a future-proof sink).

---

## Findings by severity

### Critical

_None identified._ Core prompt → stream → tool → approve → finish loop is wired and multi-agent isolation for approvals at the Rust map layer is correct.

### High

#### H1. Approvals only render on the active agent tab
- **Where:** `Conversation.tsx` (~70–72) reads `state.pendingApproval` for the **active** agent only; `ApprovalPrompt` is not global.
- **Effect:** If a subagent (or non-focused agent) hits `approval_request`, the oneshot sits in `PendingApprovals` with **no visible UI** until the user switches tabs. Under Run-All / multi-agent orchestration this can look like a hang.
- **Related:** Diff tab also keys off `activeAgent`’s pending approval (`DiffViewer.tsx` ~22–26).

#### H2. `deny_all` is a first-class backend value with no UI
- **Where:** `channels.rs` `Approval::DenyAll` (`deny_all`); TS `Approval = "approve" | "deny" | "deny_all"`; `ApprovalPrompt` only calls approve/deny.
- **Effect:** Users cannot “deny rest of turn” from the GUI even though the agent loop supports it. Contract incomplete at the interaction layer.

#### H3. Rust `ApprovalPreview` discarded; UI rebuilds diffs from args
- **Where:** Event carries `preview: Option<ApprovalPreview>` (`Diff { path, diff }` / `NewFile { path, content }`). FE `PendingApproval` stores only `{ toolCallId, toolName, args }` (`useAgentStore.ts` ~201–205, `reduceApprovalRequest` ~949–960). Diff UI parses `old_string`/`new_string`/`content` from args.
- **Effect:**
  - Diverges from PLAN.md “diff-based approval” (Rust `similar` preview is the intended payload).
  - Large edits may stream incomplete args into a partial client-side diff.
  - Tools that only populate `preview` (if any future tool does) would show “No diff content.”
- **PLAN.md** explicitly calls out the Rust preview path (gotcha #5 + UI table).

#### H4. Multi-agent approval visibility + no cross-agent badge
- **Where:** `MainPanel` tabs show running dots only (`running`), not pending-approval / error state. PLAN.md tab status: running / idle / **error** — error/approval badges missing.
- **Effect:** Compounded with H1: user has no signal that another tab needs attention.

### Medium

#### M1. `spawnAgent` / `cancel` IPC exist but UI never calls them
- **Where:** `tauri.ts` exports; no component imports `spawnAgent` or `cancel`.
- **Effect:** Users cannot spawn a blank agent from the UI or kill a stuck subagent tab (must wait for cleanup on Complete→Executing or process exit). Agent-tool spawn still works.

#### M2. Right-panel defaults vs PLAN.md
- **PLAN.md:** right panel “Hidden by default; toggled or auto-shown when an approval diff is pending.”
- **Code:** `rightPanelVisible: true`, `rightPanelTab: "plan"`, all other tabs disabled at start (`useAgentStore.ts` ~1198–1204). Diff is **not** auto-revealed on `approval_request` (only Plan auto-reveals on plan activity via `autoRevealPlan`).
- **Effect:** Intentional product evolution is fine, but approval UX is weaker than design (no auto-diff panel).

#### M3. Safety mode dual source of truth
- **Runtime:** `IpcState.safety_mode: Arc<RwLock<SafetyMode>>` — `set_safety_mode` / StatusBar / Allow-for-project.
- **Disk:** `save_settings` writes `config.toml` and **also** updates the runtime lock when `safety` is patched; `get_settings` prefers **runtime** over disk.
- **Gaps:**
  - StatusBar `set_safety_mode` does **not** persist to config.toml → restart reverts unless Settings was used.
  - FE store `safetyMode` is updated optimistically; if `invoke` fails after a local set in some paths, UI can lie (StatusBar mostly awaits then sets; still no re-fetch on focus).
  - `auto-read-approve-writes` is a valid mode in types/backend but StatusBar dropdown only offers three modes (approve-each / auto-project / autonomous) — fourth mode only via Settings/config.

#### M4. Theme / show_token_usage: localStorage vs config.toml
- **Where:** `App.tsx` applies backend `[ui]` only if LS keys absent; Appearance writes LS; Settings can write config.
- **Effect:** Two preferences systems; easy for Settings “theme” and live UI theme to disagree across machines/restarts.

#### M5. Plan UI incomplete relative to workflow stack
- **Where:** `get_workflow_state` returns `depth`, `parents`, `skill`; `PlanProgress` only shows title/goal/steps/state string. No sub-plan breadcrumb, no skill banner in the plan panel (skill appears only as transcript `skill` entry / workflow state badge color treats non-executing/complete as yellow — `"skill"` falls through).
- **Effect:** Nested plans and merge_to_main skill state are hard to see in the dedicated Plan view.

#### M6. `child_finished` applied to child agent_id, not parent UI
- **Where:** forwarder emits payload with `agent_id: child_id`; reducer marks **child** finished. Parent only gets a backend `Suggestion` (steer text).
- **Effect:** Parent transcript does not get a structured “child X done” card unless the suggestion inject path surfaces it — it does as a steer entry, but only when the parent is at a turn boundary. Fine for orchestration, weak for parent-tab UX if user is watching parent during child work.

#### M7. Backlog ▶ on every card always dispatches **top** pending, not that card
- **Where:** `BacklogItemCard.handleDispatch` → `backlogDispatchNext()` (backend `next_pending()`).
- **Effect:** Play button on item #3 still runs item #1. Title text admits this, but the control placement implies per-item dispatch. Reorder-then-play is the only workaround.

#### M8. Run-All halt on approval marks item Failed and clears run state immediately
- **Where:** `halt_run_all_for_approval` sets stop, marks item Failed with note, sets `run_all = None`.
- **Effect:** Correct safety posture, but the in-flight agent turn may still be waiting on the same approval — user must still approve in chat. Item already Failed while turn continues → status/UX mismatch until turn resolves. (Single-dispatch path is cleaner via `single_in_flight`.)

#### M9. No contract tests / codegen at the TS↔Rust boundary
- Hand-written `SerializableAgentEvent`, `Approval`, `WorkflowState`, `PlanFile`, stats DTOs, settings DTOs.
- **Effect:** Serde rename or new field can silently break FE (unknown kind → default no-op reducer; wrong casing → runtime undefined).

#### M10. `get_api_keys` returns secrets into the webview when Settings opens
- By design (password fields). Not a wiring bug, but a security boundary note: any XSS in the FE would see keys. Keys are correctly excluded from `get_config` / `get_settings`.

### Low

#### L1. Reasoning deltas not rAF-batched
- Only `text_delta` is buffered; `reasoning_delta` hits the store every token. InflightBar can thrash on heavy thinkers.

#### L2. Conversation always force-scrolls (no “stick to bottom only if already near bottom”)
- Throttled, but user scroll-up during stream is fought every 100ms.

#### L3. Approval keyboard shortcuts are window-level A/D without focus trap / dialog role
- `ApprovalPrompt` is an inline card, not a modal. PLAN.md wanted Radix dialogs for a11y-critical flows. Focus is not moved to the prompt; screen-reader announcement is weak. A/D correctly ignore editable targets.

#### L4. `Finished.reason` unused; length/content_filter finishes look like normal stops.

#### L5. `save_conversation` ignores `_agent_id` on the backend
- Harmless; path is FE-chosen. Load restores into store without validating agent id.

#### L6. Initial `listAgents` does not set `running` from `AgentInfo.running`
- `registerAgents` only seeds empty state + names/parents. If an agent were mid-turn at FE reconnect (not a normal cold start), dots would be wrong until next event. Acceptable for current process model (FE starts with agents idle).

#### L7. PlanProgress 2s poll continues even when Plan tab disabled/hidden
- Minor IPC chatter (`get_workflow_state` every 2s while component mounted — only when tab content mounted, so OK when tab off).

#### L8. `backlogDispatchNext` FE wrapper types `Promise<void>` but backend returns `bool`
- Success/no-op indistinguishable in UI (no toast when agent busy).

---

## Multi-agent & approval deep-dive

### Identity & routing

| Concern | Implementation | Verdict |
|---|---|---|
| Main agent | Smallest parentless id (`selectMainAgentId` / `manager.main_agent_id`) | ✓ Consistent FE/BE |
| Prompt target | `InputBar` → `activeAgent` | ✓ Intentional; user can prompt subagents |
| Backlog target | Always MAIN; refuses if descendants running | ✓ Matches comments in commands.rs |
| Tabs | `Object.entries(agents)` + names from `listAgents` / `child_finished` | ✓ Dynamic |
| Unknown agent stream | `listAgents` once per id (`nameResolutionInFlight`) | ✓ StrictMode-safe singleton listener |
| Exit cleanup | `Exited` removes maps; active falls back to main | ✓ |
| Stale subagents | Complete→Executing cancels inactive parented agents | ✓ Good hygiene |

### Approval map correctness (Rust)

- Key: `(AgentId, tool_call_id)` — cleanup is agent-scoped (`cleanup_for_agent`).
- Resolve: by `tool_call_id` only (globally unique assumption) — documented; OK if ids are unique across agents.
- Cleanup on `Finished` **and** `Exited` — defensive; dropping sender if FE never answered → agent sees closed oneshot (deny/error path in loop — verify separately in agent code if needed).
- Tests cover multi-agent cleanup isolation (`approval.rs` tests).

### FE approval correctness gaps

1. **Visibility** tied to active tab (H1/H4).
2. **No Deny all** (H2).
3. **Local `resolved` state** in `ApprovalPrompt` can desync if backend rejects resolve (`approve` returns `false` when unknown) — UI still shows approved/denied after await without checking return value.
4. **Clear mid-approval:** `/clear` interrupts + clears `pendingApproval` but does **not** call `approve(..., deny)` — oneshot may remain until `Finished` cleanup. Prefer explicit deny on clear.
5. **Mark Safe** saves rule then approves even if save fails (intentional) — good.
6. **Allow for project** sets runtime mode then approves — does not persist config (M3).

### Parent completion

- First `Finished` or final `Error` on child → one `Suggestion` to parent + `ChildFinished` UI event.
- Dedup via `notified_children` HashSet; cleared on `Exited`.
- Multi-turn children only notify once (by design) — parent is not told about later turns.

### Streaming / interrupt

- Stop → `interrupt` → agent emits `Finished`; running clears.
- `/clear` mid-stream: interrupt + `clearStreamingBuffer` + `clearConversation` — well thought out against StrictMode/rAF races.
- Steer while running: `send_suggestion` + local steer queue; `suggestion_injected` lands + 5s TTL.

---

## Strengths

1. **Clean channel boundary** — brain unaware of Tauri; oneshot held correctly in adapter.
2. **Complete event reducer matrix** — every `SerializableAgentEvent` kind handled; pure reducers + effects are unit-testable (`useAgentStore.test.ts`).
3. **StrictMode-safe listeners** — module-level singleton for agent + backlog events prevents double-subscribe duplication.
4. **Text delta batching** — rAF coalescing is the right streaming UX tradeoff.
5. **Per-agent workflow** — `agent_loops` map + `get_workflow_state(agent_id)` + FE `workflowStates` map avoid cross-agent plan bleed.
6. **Backlog system** — persistence, auto-feed, run-all checkpoint/rollback, descendant-aware idle, PromptDispatched parity with manual input.
7. **Settings secret hygiene** — keys only via `get_api_keys`; config payloads documented secret-free.
8. **Sandbox on FE file commands** — `read_file` / `list_files` / conversation paths validated.
9. **Right-panel tool enable model** — Plan-first defaults; per-tool toggles; panel hide when empty.
10. **Spawn parity** — UI button path and agent-tool path share `spawn_agent_shared`; tool path emits PromptDispatched for transcript parity.

---

## Prioritized wiring fixes (recommendations only)

### P0 — correctness / multi-agent ops

1. **Global approval attention**
   - Badge on agent tabs when `pendingApproval != null`.
   - Optional: sticky approval banner when any non-active agent has pending approval, with “Switch & review”.
   - Consider auto-switching only if user preference allows (don’t steal focus by default).

2. **Wire Deny all**
   - Button + shortcut (e.g. Shift+D) calling `approve(id, "deny_all")`.

3. **Honor `approve()` boolean**
   - If `false`, show error and keep prompt open (stale/expired request).

4. **On conversation clear with pending approval**
   - `approve(toolCallId, "deny")` before wiping store (best-effort).

### P1 — contract fidelity

5. **Consume `ApprovalPreview`**
   - Prefer `event.preview` for DiffViewer / ApprovalPrompt; fall back to args parsing.
   - Align TS type with Rust tagged enum (`kind: "diff" | "new_file"`, path as string).

6. **Auto-reveal Diff tab on `approval_request` for file tools** (mirror `autoRevealPlan`), without yanking if user disabled Diff.

7. **Expose or remove dead commands**
   - Add UI: “+ Agent” / “Close agent” → `spawnAgent` / `cancel`, **or** drop unused exports and document tool-only spawn.

8. **Per-item backlog dispatch** (or move ▶ to toolbar only) so card controls match behavior.

### P2 — state sync & settings

9. **Single safety-mode write path**
   - StatusBar toggle should either persist via `save_settings({ safety })` or clearly label “session only”.
   - Include `auto-read-approve-writes` in StatusBar if it’s a supported mode.

10. **Surface workflow skill + plan stack** in `PlanProgress` (`skill`, `parents`, `depth`).

11. **Return `bool` from `backlogDispatchNext` to FE** and toast “agent busy” / “dispatched”.

12. **Run-All approval halt status**
    - Keep item `in_flight` until user resolves approval, or add status `halted_approval` instead of immediate `failed`.

### P3 — hygiene & drift prevention

13. **Contract tests**: JSON fixtures from Rust `SerializableAgentEvent` / `Approval` / `PlanFile` golden files asserted in FE vitest (or ts-rs / specta codegen).

14. **rAF-batch `reasoning_delta`** like text.

15. **Smart scroll**: only auto-scroll when user is near bottom.

16. **Use `Finished.reason`** for a one-line system note on `length` / `content_filter`.

17. **Approval as Radix Dialog** when pending (focus trap, `aria-modal`) per PLAN.md a11y intent — keep A/D shortcuts.

---

## Open questions

1. **Is prompting subagents from InputBar intentional long-term?** Backlog forbids it; chat allows it. Should subagent input be steer-only?
2. **Should Complete→Executing cleanup also clear FE transcripts for cancelled subagents**, or is tab disappearance enough?
3. **Is session-only safety mode (StatusBar) desired**, or was persistence omitted by accident?
4. **Will any tool rely solely on `ApprovalPreview` without parallel args fields?** If yes, H3 is a latent break.
5. **Are `tool_call_id`s guaranteed unique across agents for the process lifetime?** Resolve-by-id-only depends on this.
6. **Run-All + strict safety:** product expects overnight only in autonomous modes — UI warns but still allows start; is that final?
7. **PLAN.md right-panel “hidden by default”** — confirm Plan-open default is the new locked decision so docs can be updated.
8. **UI spawn button:** deliberately omitted to force plan-scoped tool spawns, or just not built yet?

---

## Appendix A — File map (primary)

| Layer | Path | Role |
|---|---|---|
| Design | `PLAN.md` | IPC/UI intent |
| Channels | `src/runtime/channels.rs` | `AgentCommand` / `AgentEvent` / `Approval` |
| IPC commands | `src-tauri/src/ipc/commands.rs` | All invokes + backlog/run-all |
| IPC events | `src-tauri/src/ipc/events.rs` | Fan-in forwarder |
| Approvals | `src-tauri/src/ipc/approval.rs` | Oneshot map |
| State | `src-tauri/src/ipc/state.rs` | `IpcState` |
| Register | `src-tauri/src/main.rs` | `generate_handler!` |
| FE bridge | `frontend/src/lib/tauri.ts` | invoke/listen wrappers |
| FE types | `frontend/src/lib/types.ts` | Mirror shapes |
| Store | `frontend/src/hooks/useAgentStore.ts` | Zustand + reducers |
| Events hook | `frontend/src/hooks/useAgentEvents.ts` | Listeners + rAF |
| Chat I/O | `InputBar.tsx`, `Conversation.tsx`, `ApprovalPrompt.tsx` | |
| Agents UI | `MainPanel.tsx` | Tabs |
| Plan/Diff/Backlog | `views/PlanProgress.tsx`, `DiffViewer.tsx`, `BacklogView.tsx` | |
| Shell | `App.tsx`, `StatusBar.tsx`, `Sidebar.tsx`, `RightPanel.tsx` | |

## Appendix B — Severity legend

- **Critical:** Data loss, stuck oneshots with no recovery, wrong agent mutation, security break.
- **High:** Multi-agent hang/invisible gate, missing shipped control, preview contract ignored.
- **Medium:** Desync, dead API surface, UX/contract drift, incomplete plan/skill display.
- **Low:** Perf polish, a11y depth, unused fields, minor type mismatches.
