# Review — Skill start announcement (toolbar path) + display-event refactor

**Date:** 2026-04-04
**Scope:** All uncommitted changes (`git diff HEAD` + `git status`).
**Plan:** "Announce skill start in the transcript (toolbar path) + light display-event refactor."

## Files reviewed
- `src/runtime/channels.rs` — `AgentEvent::SkillStarted` + `SerializableAgentEvent::SkillStarted` + `into_serializable` arm + roundtrip test.
- `src-tauri/src/ipc/commands.rs` — `emit_agent_event` helper; `enter_skill` gains `app: tauri::AppHandle` + emits `SkillStarted` after `start_skill` succeeds, before the `Prompt`.
- `frontend/src/lib/types.ts` — `skill_started` event + `skill` `TranscriptEntry`.
- `frontend/src/hooks/useAgentStore.ts` — `pushTranscriptEntry` helper; `reduceSkillStarted`; `case "skill_started"`.
- `frontend/src/components/chat/Message.tsx` — `skill` render case + `arePropsEqual` arm + `argLabel` `skill_start` branch.
- `frontend/src/hooks/useAgentStore.test.ts` — `testSkillStartedPushesSkillEntry`.
- `.coding/plans/stack.json` + `.coding/plans/4c4e771c-*.md` — plan metadata (ignored).

## Correctness

### C1 — Pass — Toolbar path now announces the skill
`enter_skill` (`src-tauri/src/ipc/commands.rs:585-594`) emits `SkillStarted { name, prompt }` via the shared `emit_agent_event` helper immediately after `wf.start_skill(...)` succeeds (line 582) and before `manager.send(..., AgentCommand::Prompt)` (line 599). The frontend reducer `reduceSkillStarted` (`useAgentStore.ts:954-958`) pushes a `skill` transcript entry, and `Message.tsx:149-167` renders `▶ skill "name" start` with the goal as a muted subtitle. The toolbar path (the actual bug — previously emitted nothing) is fixed.

### C2 — Pass — Event emitted at the right time
The emit is positioned after `start_skill` succeeds (so a failed start does not announce) and before the `Prompt` send (so the announcement lands before the agent's `Started` event). This mirrors `emit_prompt_dispatched` in `dispatch_next_impl` (`commands.rs:1178`) and `run_all_dispatch_next` (`commands.rs:1310`), which emit after the send — a minor inconsistency (prompt-dispatched emits *after* send; skill emits *before* send), but both land before the agent's `Started` event because the agent task processes the `Prompt` asynchronously. No ordering bug.

### C3 — Pass — Reducer flushes streaming text before pushing the skill entry
`reduceSkillStarted` calls `pushTranscriptEntry(next, ...)` (`useAgentStore.ts:956`), which calls `flushStreamingText(agent)` then appends (`useAgentStore.ts:500-503`). The test `testSkillStartedPushesSkillEntry` (`useAgentStore.test.ts:253-284`) seeds in-flight `text_delta` and asserts (a) `streamingText === ""` after, (b) the flushed assistant text lands *above* the skill entry (`asstIdx < skillIdx`). Correct.

### C4 — Pass — `arePropsEqual` skill case is correct
`Message.tsx:53-57` compares `pe.name === ne.name && pe.prompt === ne.prompt` for the `skill` kind. Skill entries are immutable once pushed (no reducer ever mutates a `skill` entry in place), so string equality on both fields is sufficient and correct. Falls through to `default: return false` is unreachable for this kind after the kind check at line 32.

### C5 — Pass — `argLabel` `skill_start` branch
`Message.tsx:244-249` extracts `parsed.skill` for `skill_start` tool calls so the agent-driven ToolCard reads `skill_start (merge_to_main)`. The `SkillStartArgs` struct (`src/tool/workflow/skill.rs:30-41`) uses `skill` as the field name, matching. Returns `null` on missing/blank, consistent with the `shell`/`spawn_agent` branches.

## Bugs

### B1 — Pass — No duplication: `enter_skill` is the sole emitter of `SkillStarted`
A repo-wide search for `SkillStarted` (`src/runtime/channels.rs` + `src-tauri/src/ipc/commands.rs`) confirms the only construction site outside tests is `commands.rs:590`. The agent-driven `skill_start` tool (`src/tool/workflow/skill.rs:110-151`) calls `wf.start_skill` and returns a `ToolResult` — it does **not** emit any `AgentEvent`. Its announcement is the normal `ToolCallStart`→`ToolResult` flow (a ToolCard), now name-enhanced via `argLabel`. `Workflow::start_skill` (`src/workflow/mod.rs:391-413`) emits nothing. No duplication.

### B2 — Pass — `app: tauri::AppHandle` param does not break the frontend invoke
`enter_skill` now has `app: tauri::AppHandle` as its first param (`commands.rs:541-547`). Tauri injects `AppHandle`/`State`/`Window` automatically by type, regardless of position, and they are **not** part of the JS-side argument object. The frontend call (`frontend/src/lib/tauri.ts:293`) is `invoke("enter_skill", { agentId, skill, prompt: prompt ?? null })` — unchanged. The other params (`state`, `agent_id`, `skill`, `prompt`) map by name (snake_case ↔ camelCase via Tauri's default). This matches the existing pattern in `backlog_add` (`commands.rs:1050-1054`), which also takes `app: tauri::AppHandle` first. No break.

### B3 — Pass — `WorkflowState` import removal is safe
The original `enter_skill` had a local `use myharness::workflow::WorkflowState;` (HEAD line 540). The new body replaced it with `use myharness::runtime::channels::SerializableAgentEvent;` (`commands.rs:548`). The function no longer references `WorkflowState` (the `available_in` check uses `registry.is_available_in(&skill, current)` where `current` is already a `WorkflowState` from `wf.state()`). The top-level import `use myharness::workflow::{PlanFile, WorkflowState};` (`commands.rs:20`) still covers `WorkflowStateInfo` + `get_workflow_state`. No dangling/unused import.

### B4 — Pass — `reduceSuggestionInjected` refactor is behavior-preserving
The old code called `flushStreamingText(next)` then `next.transcript = [...next.transcript, { kind: "steer", text: event.text }]`. The new code calls `pushTranscriptEntry(next, { kind: "steer", text: event.text })`, which does `flushStreamingText(agent); agent.transcript = [...agent.transcript, entry]` — identical semantics. The flush happens *after* the steers array is mutated but *before* the transcript append, same as before. The `scheduleSteerRemoval` effect is unaffected (computed before the push). No behavior change.

### B5 — Pass — Exhaustive matches updated
`AgentEvent::into_serializable` (`channels.rs:234-335`) is an exhaustive `match` on `AgentEvent` and now includes the `SkillStarted` arm (`channels.rs:312-315`). The event forwarder (`src-tauri/src/ipc/events.rs:92-128, 141-186`) uses non-exhaustive `match &serial { ... _ => {} }` patterns, so the new `SkillStarted` variant correctly falls through to the emit with no special handling needed (it's display-only, like `PromptDispatched`). No unhandled-variant compile error.

## Security

### S1 — Pass — `SkillStarted` is display-only
`SkillStarted { name, prompt }` carries only the skill name + goal prompt — both already known to the frontend (the toolbar dialog shows them, and `get_workflow_state` returns the active skill). It triggers no mutation, no tool execution, no approval bypass. The reducer only appends a transcript entry. No new mutation path.

### S2 — Pass — No approval-gate regression
`enter_skill` remains the UI-initiated path (the confirm dialog *is* the approval). The agent-driven `skill_start` tool remains `AutoRun` with protection on the operations inside the skill (`never_auto_for` on git merge/push). The new emit does not change any gating. Constitution-compliant.

## Constitution compliance

### CC1 — Pass — Public functions have doc comments
- `enter_skill` (`commands.rs:521-539`) — doc comment updated to describe the new emit.
- `emit_agent_event` (`commands.rs:992-1001`) — doc comment present.
- `emit_prompt_dispatched` (`commands.rs:1010-1013`) — doc comment present.
- `AgentEvent::SkillStarted` (`channels.rs:120-130`) — doc comment present.
- `SerializableAgentEvent::SkillStarted` (`channels.rs:213-215`) — doc comment present.
- `pushTranscriptEntry` (`useAgentStore.ts:492-499`) — doc comment present.
- `reduceSkillStarted` (`useAgentStore.ts:945-953`) — doc comment present.

### CC2 — Pass — Follows existing code style
The Rust helpers mirror the existing `emit_prompt_dispatched` / `emit_child_finished` pattern (build `AgentEventPayload`, `app.emit`, log on `Err`). The frontend reducer + helper mirror `reducePromptDispatched` / `flushStreamingText`. The `Message.tsx` `skill` case uses the same em-based sizing + class structure as the `steer`/`tool` cases. Consistent.

### CC3 — Note (pre-existing, not introduced here) — Frontend tests are type-checked only, not executed
`useAgentStore.test.ts` has no test runner (no vitest/jest configured); `testSkillStartedPushesSkillEntry` is only type-checked via `tsc --noEmit`. This is a pre-existing limitation noted in prior reviews (`.coding/reviews/2026-04-04-clear-midstream-crash-review.md:75`, `.coding/reviews/2026-04-04-review-fixes-final-review.md:128`). The Rust roundtrip test (`skill_started_roundtrips_json`, `channels.rs:505-529`) **is** executed by `cargo test`. Not a finding against this change.

## Summary

**No findings.** The diff is clean. The toolbar path now announces the skill in the transcript; the agent-driven path shows the skill name via `argLabel`; `enter_skill` is the sole `SkillStarted` emitter (no duplication); the `app: AppHandle` param is Tauri-injected and does not break the frontend invoke; the reducer correctly flushes streaming text before pushing the skill entry; `arePropsEqual` is correct; all public functions are documented; the change is display-only with no new mutation or gating impact.
