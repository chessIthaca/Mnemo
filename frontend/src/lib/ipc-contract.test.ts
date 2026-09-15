// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * IPC contract fixture suite — the TypeScript-side drift detector
 * (Maintainability H4).
 *
 * The Rust side commits golden JSON fixtures for every
 * `SerializableAgentEvent` variant + the key IPC DTOs, enforced by a Rust
 * serde-equality test (brain crate `tests/contract_fixtures.rs` + app crate
 * `ipc/contract_fixtures.rs`). This suite imports those SAME fixtures and:
 *
 *  - For each EVENT fixture: feeds it through the real
 *    `useAgentStore.getState().handleAgentEvent` reducer (cast via an
 *    `as unknown` narrowing helper — JSON imports infer `string`, not the
 *    literal unions, so a pure type-level check isn't viable) and asserts it
 *    dispatches without throwing + produces a sane state slice. This proves
 *    the TS reducer + state shapes still accept the wire shape Rust emits.
 *  - For each DTO fixture: asserts the expected top-level fields are present
 *    with the expected primitive types + the discriminant values (`kind` tags,
 *    `status`, workflow `state`, safety strings).
 *
 * If Rust changes a shape → the Rust fixture test fails → the dev updates the
 * fixture → this suite re-checks the TS expectations → drift is caught
 * end-to-end at `npm test` time. The fixtures are the single source of truth
 * for both sides.
 */

import { describe, expect, it, beforeEach } from "vitest";

import { useAgentStore } from "../hooks/useAgentStore";
import type { AgentEventPayload, AgentId } from "./types";

// ── Event fixtures ───────────────────────────────────────────────────────────
import started from "./ipc-fixtures/event-started.json";
import textDelta from "./ipc-fixtures/event-text-delta.json";
import reasoningDelta from "./ipc-fixtures/event-reasoning-delta.json";
import toolCallStart from "./ipc-fixtures/event-tool-call-start.json";
import toolCallArgDelta from "./ipc-fixtures/event-tool-call-arg-delta.json";
import approvalRequest from "./ipc-fixtures/event-approval-request.json";
import approvalRequestNewFile from "./ipc-fixtures/event-approval-request-new-file.json";
import approvalRequestNullPreview from "./ipc-fixtures/event-approval-request-null-preview.json";
import toolResult from "./ipc-fixtures/event-tool-result.json";
import usage from "./ipc-fixtures/event-usage.json";
import contextUsage from "./ipc-fixtures/event-context-usage.json";
import compacted from "./ipc-fixtures/event-compacted.json";
import compactStarted from "./ipc-fixtures/event-compact-started.json";
import visionDescribe from "./ipc-fixtures/event-vision-describe.json";
import visionDescribed from "./ipc-fixtures/event-vision-described.json";
import workflowStateChanged from "./ipc-fixtures/event-workflow-state-changed.json";
import stepCompleted from "./ipc-fixtures/event-step-completed.json";
import suggestionInjected from "./ipc-fixtures/event-suggestion-injected.json";
import promptDispatched from "./ipc-fixtures/event-prompt-dispatched.json";
import skillStarted from "./ipc-fixtures/event-skill-started.json";
import modelChanged from "./ipc-fixtures/event-model-changed.json";
import memoryRecalled from "./ipc-fixtures/event-memory-recalled.json";
import userQuestion from "./ipc-fixtures/event-user-question.json";
import finished from "./ipc-fixtures/event-finished.json";
import finishedOther from "./ipc-fixtures/event-finished-other.json";
import phaseEvent from "./ipc-fixtures/event-phase.json";
import exited from "./ipc-fixtures/event-exited.json";
import childFinished from "./ipc-fixtures/event-child-finished.json";
import errorEvent from "./ipc-fixtures/event-error.json";

// ── DTO fixtures ─────────────────────────────────────────────────────────────
import dtoAgentInfo from "./ipc-fixtures/dto-agent-info.json";
import dtoWorkflowStateInfo from "./ipc-fixtures/dto-workflow-state-info.json";
import dtoWorkflowStateInfoSkill from "./ipc-fixtures/dto-workflow-state-info-skill.json";
import dtoBacklogChangedPayload from "./ipc-fixtures/dto-backlog-changed-payload.json";
import dtoGetSettings from "./ipc-fixtures/dto-get-settings.json";
import dtoSaveEndpoints from "./ipc-fixtures/dto-save-endpoints.json";
import dtoSaveSettings from "./ipc-fixtures/dto-save-settings.json";
import dtoCodegraphStatus from "./ipc-fixtures/dto-codegraph-status.json";
import dtoCodegraphGraph from "./ipc-fixtures/dto-codegraph-graph.json";

const ID = 1 as AgentId;
const SUB = 2 as AgentId;

/**
 * Narrow a JSON-imported fixture (typed as a broad inferred shape) into an
 * `AgentEventPayload` for dispatch. JSON module imports lose literal-union
 * inference (`kind` becomes `string`), so a direct type-level assignment to
 * `AgentEventPayload` would not compile; we cast through `unknown` and rely on
 * the runtime assertions below to validate the shape. This is intentional: the
 * suite's job is to catch drift at runtime against the committed fixture, and
 * a compile-time cast would silently accept any `{ kind: string }` blob.
 */
function asPayload(fixture: unknown): AgentEventPayload {
  return fixture as AgentEventPayload;
}

/** Reset the store slices the reducers touch before each dispatch. */
function resetStore(): void {
  useAgentStore.setState({
    agents: {},
    agentNames: {},
    agentParents: {},
    agentModels: {},
    agentProviders: {},
    workflowStates: {},
    activeAgent: null,
    lastDiff: null,
    planDiffs: [],
    topPlanId: null,
    selectedDiffPath: null,
    planVersion: 0,
    backlog: [],
    autoFeed: false,
    runAll: { active: false, done: 0, total: 0, compacting: false, concurrency: 1, spawned: [], note: null },
    showToolActivity: true,
    showKnowledgeActivity: true,
    chatThreadLine: true,
    chatProseCap: true,
    chatTurnTint: true,
    chatHoverTimestamps: true,
  });
}

function agent(id: AgentId) {
  const a = useAgentStore.getState().agents[id];
  if (!a) throw new Error(`agent ${id} missing after dispatch`);
  return a;
}

describe("IPC contract — event fixtures dispatch through the reducer", () => {
  beforeEach(resetStore);

  it("event-started: marks the agent running + resets turn state", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: started }));
    expect(agent(ID).running).toBe(true);
    expect(agent(ID).tokenUsage).toEqual({ prompt: 0, completion: 0, reasoning: 0, cached: 0 });
  });

  it("event-phase: updates the agent's live turn phase", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: phaseEvent }));
    expect(agent(ID).phase).toBe("running_tools");
  });

  it("event-text-delta: appends to streaming text", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: textDelta }));
    expect(agent(ID).streamingText).toBe("hi");
  });

  it("event-reasoning-delta: appends to streaming reasoning + activity log", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: reasoningDelta }));
    const a = agent(ID);
    expect(a.streamingReasoning).toBe("thinking");
    expect(a.activityLog).toHaveLength(1);
    expect(a.activityLog[0].kind).toBe("reasoning");
  });

  it("event-tool-call-start: pushes a tool transcript entry", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: toolCallStart }));
    const a = agent(ID);
    expect(a.transcript).toHaveLength(1);
    expect(a.transcript[0].kind).toBe("tool");
  });

  it("event-tool-call-arg-delta: accumulates args into the in-flight call", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: toolCallStart }));
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: toolCallArgDelta }));
    const tool = agent(ID).transcript.find((e) => e.kind === "tool");
    if (tool && tool.kind === "tool") expect(tool.calls[0].args).toBe('{"cmd":');
  });

  it("event-approval-request: records a pending approval with a diff preview", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: approvalRequest }));
    const a = agent(ID);
    expect(a.pendingApproval).not.toBeNull();
    expect(a.pendingApproval?.preview?.kind).toBe("diff");
  });

  it("event-approval-request-new-file: records a pending approval with a new_file preview", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: approvalRequestNewFile }));
    expect(agent(ID).pendingApproval?.preview?.kind).toBe("new_file");
  });

  it("event-approval-request-null-preview: records a pending approval with null preview", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: approvalRequestNullPreview }));
    expect(agent(ID).pendingApproval?.preview).toBeNull();
  });

  it("event-tool-result: updates the call + clears the pending approval", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: toolCallStart }));
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: approvalRequest }));
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: toolResult }));
    const a = agent(ID);
    expect(a.pendingApproval).toBeNull();
    const tool = a.transcript.find((e) => e.kind === "tool");
    if (tool && tool.kind === "tool") expect(tool.calls[0].result?.success).toBe(true);
  });

  it("event-usage: accumulates tokens + records timing", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: usage }));
    const a = agent(ID);
    expect(a.tokenUsage).toEqual({ prompt: 100, completion: 50, reasoning: 10, cached: 20 });
    expect(a.lastRequestTiming?.ttft_ms).toBe(1000);
  });

  it("event-context-usage: records the context-window fill", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: contextUsage }));
    expect(agent(ID).contextUsage).toEqual({ used: 1000, max: 8000 });
  });

  it("event-compacted: appends a transcript confirmation entry", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: compacted }));
    const a = agent(ID);
    const note = a.transcript.find((e) => e.kind === "assistant");
    expect(note && note.kind === "assistant" ? note.text : "").toContain("Context compacted");
  });

  it("event-compact-started: appends a transcript announcement entry", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: compactStarted }));
    const a = agent(ID);
    const note = a.transcript.find((e) => e.kind === "assistant");
    expect(note && note.kind === "assistant" ? note.text : "").toContain("Compacting context");
  });

  it("event-vision-describe + vision-described: push and complete an image-parsing card", () => {
    // The announcement (before the vision round-trip) pushes a running
    // `vision` transcript entry; the paired described event fills it in —
    // the card then holds the query sent to the vision model and the
    // response it returned (user request: see the query/response when
    // uncollapsed).
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: visionDescribe }));
    const running = agent(ID).transcript.find((e) => e.kind === "vision");
    expect(running).toBeDefined();
    if (running?.kind === "vision") {
      expect(running.index).toBe(1);
      expect(running.total).toBe(2);
      expect(running.query).toBe("Describe this image in detail.");
      expect(running.description).toBeNull();
      expect(running.running).toBe(true);
    }
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: visionDescribed }));
    const done = agent(ID).transcript.find((e) => e.kind === "vision");
    expect(done).toBeDefined();
    if (done?.kind === "vision") {
      expect(done.description).toBe("a photo of a cat");
      expect(done.success).toBe(true);
      expect(done.running).toBe(false);
    }
    expect(agent(ID).transcript.filter((e) => e.kind === "vision")).toHaveLength(1);
  });

  it("event-workflow-state-changed: updates workflowStates + bumps planVersion", () => {
    const before = useAgentStore.getState().planVersion;
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: workflowStateChanged }));
    expect(useAgentStore.getState().workflowStates[ID]).toBe("executing");
    expect(useAgentStore.getState().planVersion).toBe(before + 1);
    // The fixture's root plan id lands in the store (no agents registered →
    // no main-agent reset fires, so topPlanId takes the event's value).
    expect(useAgentStore.getState().topPlanId).toBe("plan-1a2b3c");
  });

  it("event-step-completed: bumps planVersion", () => {
    const before = useAgentStore.getState().planVersion;
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: stepCompleted }));
    expect(useAgentStore.getState().planVersion).toBe(before + 1);
  });

  it("event-suggestion-injected: pushes a steer transcript entry", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: suggestionInjected }));
    expect(agent(ID).transcript.some((e) => e.kind === "steer")).toBe(true);
  });

  it("event-prompt-dispatched: appends a user transcript entry", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: promptDispatched }));
    expect(agent(ID).transcript.some((e) => e.kind === "user")).toBe(true);
  });

  it("event-skill-started: pushes a skill transcript entry", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: skillStarted }));
    expect(agent(ID).transcript.some((e) => e.kind === "skill")).toBe(true);
  });

  it("event-model-changed: stamps the agent model", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: modelChanged }));
    expect(useAgentStore.getState().agentModels[ID]).toBe("gpt-5");
  });

  it("event-memory-recalled: pushes an auto-recall memory entry (always — rendering hides it when off)", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: memoryRecalled }));
    const entry = agent(ID).transcript.find((e) => e.kind === "memory");
    expect(entry).toBeDefined();
    if (entry?.kind === "memory") {
      expect(entry.name).toBe("auto-recall");
      expect(entry.snippet).toContain("3 hit(s)");
      expect(entry.snippet).toContain("merge instructions");
      expect(entry.hits).toHaveLength(3);
      expect(entry.hits?.[0]).toEqual({ tier: "semantic", title: "merge instructions" });
      expect(entry.running).toBe(false);
    }
  });

  it("event-user-question: records a pending question with options", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: userQuestion }));
    const q = agent(ID).pendingQuestion;
    expect(q).not.toBeNull();
    expect(q!.questionId).toBe("q_1");
    expect(q!.question).toBe("Pick a skill to master");
    expect(q!.options.length).toBe(3);
    expect(q!.options[0].label).toBe("Music");
    expect(q!.options[0].description).toBe("Play any instrument");
    // The third option has no description (the key is omitted in the JSON
    // fixture, so it's undefined — not null).
    expect(q!.options[2].description).toBeFalsy();
  });

  it("event-finished: sets running=false + flushes streaming text", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: started }));
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: textDelta }));
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: finished }));
    const a = agent(ID);
    expect(a.running).toBe(false);
    expect(a.streamingText).toBe("");
  });

  it("event-finished-other: dispatches the { other: string } FinishReason form", () => {
    // The Other variant renders as { "other": "max_tokens" } — prove the
    // reducer accepts it without throwing (finished just needs a `reason`).
    expect(() =>
      useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: finishedOther })),
    ).not.toThrow();
    expect(agent(ID).running).toBe(false);
  });

  it("event-exited: removes the agent from all maps", () => {
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null },
      { id: SUB, name: "sub", running: true, parent_id: ID },
    ]);
    useAgentStore.getState().setActiveAgent(SUB);
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: SUB, event: exited }));
    expect(useAgentStore.getState().agents[SUB]).toBeUndefined();
    expect(useAgentStore.getState().activeAgent).toBe(ID);
  });

  it("event-child-finished: marks not running + records the child name", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: started }));
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: childFinished }));
    expect(agent(ID).running).toBe(false);
    expect(useAgentStore.getState().agentNames[SUB]).toBe("reviewer");
  });

  it("event-error (final): adds an error entry + stops the agent", () => {
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: started }));
    useAgentStore.getState().handleAgentEvent(asPayload({ agent_id: ID, event: errorEvent }));
    const a = agent(ID);
    expect(a.transcript.some((e) => e.kind === "error")).toBe(true);
    expect(a.running).toBe(false);
  });
});

describe("IPC contract — DTO fixture field shapes", () => {
  it("dto-agent-info: id/name/running/parent_id/model with expected types", () => {
    expect(typeof dtoAgentInfo.id).toBe("number");
    expect(typeof dtoAgentInfo.name).toBe("string");
    expect(typeof dtoAgentInfo.running).toBe("boolean");
    // parent_id is null for the main agent (no skip — emitted as null).
    expect(dtoAgentInfo.parent_id === null || typeof dtoAgentInfo.parent_id === "number").toBe(true);
    // model is present (Some) for a live agent; skipped when None.
    expect(typeof dtoAgentInfo.model).toBe("string");
  });

  it("dto-workflow-state-info: state + plan + depth + parents (skill skipped when None)", () => {
    expect(dtoWorkflowStateInfo.state).toBe("executing");
    expect(typeof dtoWorkflowStateInfo.depth).toBe("number");
    expect(Array.isArray(dtoWorkflowStateInfo.parents)).toBe(true);
    expect(dtoWorkflowStateInfo.plan).not.toBeNull();
    expect(typeof dtoWorkflowStateInfo.plan.title).toBe("string");
    expect(Array.isArray(dtoWorkflowStateInfo.plan.steps)).toBe(true);
    // skill is skipped (skip_serializing_if) when None → key absent. JSON
    // imports infer the object shape WITHOUT the absent key, so access it via
    // a record cast (the assertion is that the key is absent at runtime).
    expect((dtoWorkflowStateInfo as Record<string, unknown>).skill).toBeUndefined();
  });

  it("dto-workflow-state-info-skill: skill overlay present + plan null", () => {
    expect(dtoWorkflowStateInfoSkill.state).toBe("skill");
    expect(dtoWorkflowStateInfoSkill.plan).toBeNull();
    expect(dtoWorkflowStateInfoSkill.skill).toBeDefined();
    expect(typeof dtoWorkflowStateInfoSkill.skill.name).toBe("string");
    expect(typeof dtoWorkflowStateInfoSkill.skill.prompt).toBe("string");
    expect(dtoWorkflowStateInfoSkill.skill.target_state).toBe("complete");
  });

  it("dto-backlog-changed-payload: items + auto_feed + run_all shapes", () => {
    expect(Array.isArray(dtoBacklogChangedPayload.items)).toBe(true);
    expect(typeof dtoBacklogChangedPayload.auto_feed).toBe("boolean");
    expect(typeof dtoBacklogChangedPayload.run_all.active).toBe("boolean");
    expect(typeof dtoBacklogChangedPayload.run_all.done).toBe("number");
    expect(typeof dtoBacklogChangedPayload.run_all.total).toBe("number");
    // compacting (run-all auto-compact): true while the between-items
    // compaction is in flight — the brief "compacting…" progress state.
    expect(typeof dtoBacklogChangedPayload.run_all.compacting).toBe("boolean");
    const item = dtoBacklogChangedPayload.items[0];
    expect(typeof item.id).toBe("string");
    expect(typeof item.text).toBe("string");
    expect(Array.isArray(item.images)).toBe(true);
    // status is the snake_case discriminant; note may be null.
    expect(["pending", "in_flight", "done", "failed", "cant_resolve"]).toContain(item.status);
    expect(item.note === null || typeof item.note === "string").toBe(true);
    // checkpoint_sha (backlog 212c14af): the run-all resume/rollback anchor
    // parsed from the note head — null for never-checkpointed items, a sha
    // string for checkpointed ones (whose note head IS that sha).
    expect(item.checkpoint_sha).toBeNull();
    const checkpointed = dtoBacklogChangedPayload.items[1];
    expect(typeof checkpointed.checkpoint_sha).toBe("string");
    expect(checkpointed.note?.startsWith(checkpointed.checkpoint_sha ?? "")).toBe(true);
    // plan_id / plan_title (backlog f45513b2): the item↔plan linkage — the
    // ROOT plan id recorded at the Executing-entry stamp + its title read
    // from the plan file's heading. The backend omits both when None (the
    // first item pins the absent case); the in-flight item pins the present
    // case.
    expect("plan_id" in item).toBe(false);
    expect("plan_title" in item).toBe(false);
    expect(typeof checkpointed.plan_id).toBe("string");
    expect(typeof checkpointed.plan_title).toBe("string");
  });

  it("dto-get-settings: full surface with null-emitting optional fields", () => {
    expect(typeof dtoGetSettings.config_dir).toBe("string");
    expect(dtoGetSettings.general.default_provider === null || typeof dtoGetSettings.general.default_provider === "string").toBe(true);
    // vision_model must be emitted as null (not skipped) when unconfigured.
    expect(dtoGetSettings.general.vision_model).toBeNull();
    expect(typeof dtoGetSettings.context.summarize_at_fill_rate).toBe("number");
    expect(typeof dtoGetSettings.context.proxy_cache_ceiling_tokens).toBe("number");
    expect(typeof dtoGetSettings.ui.theme).toBe("string");
    expect(typeof dtoGetSettings.ui.show_token_usage).toBe("boolean");
    expect(typeof dtoGetSettings.ui.show_tool_activity).toBe("boolean");
    expect(typeof dtoGetSettings.ui.show_knowledge_activity).toBe("boolean");
    expect(typeof dtoGetSettings.ui.show_delegation_notes).toBe("boolean");
    expect(typeof dtoGetSettings.ui.show_tool_images).toBe("boolean");
    // Per-kind steering-note flags (the per-note toggles): one RESOLVED
    // boolean per MarkerKind label + the auto_delegated family.
    expect(Object.keys(dtoGetSettings.ui.steering_notes).sort()).toEqual([
      "auto_delegated",
      "consolidation_due",
      "edit_stale_read",
      "graph_miss",
      "known_memory_hit",
      "literal_tip",
      "read_nudge",
      "recall_rider",
      "search_nudge",
      "shell_redirect",
      "shell_tip",
    ]);
    expect(dtoGetSettings.ui.steering_notes.auto_delegated).toBe(false);
    expect(dtoGetSettings.ui.steering_notes.literal_tip).toBe(true);
    // The four chat-readability flags ride the ui object (plan afa81f0a).
    expect(typeof dtoGetSettings.ui.chat_thread_line).toBe("boolean");
    expect(typeof dtoGetSettings.ui.chat_prose_cap).toBe("boolean");
    expect(typeof dtoGetSettings.ui.chat_turn_tint).toBe("boolean");
    expect(typeof dtoGetSettings.ui.chat_hover_timestamps).toBe("boolean");
    expect(Array.isArray(dtoGetSettings.endpoints)).toBe(true);
    expect(Array.isArray(dtoGetSettings.projects)).toBe(true);
    // The [models] section is always present (per-context overrides).
    expect(dtoGetSettings.models).toBeDefined();
    expect(dtoGetSettings.models.planning === null || typeof dtoGetSettings.models.planning === "object").toBe(true);
    expect(dtoGetSettings.models.executing === null || typeof dtoGetSettings.models.executing === "object").toBe(true);
    expect(dtoGetSettings.models.complete === null || typeof dtoGetSettings.models.complete === "object").toBe(true);
    expect(dtoGetSettings.models.subagent === null || typeof dtoGetSettings.models.subagent === "object").toBe(true);
    expect(typeof dtoGetSettings.models.skill).toBe("object");
    // The [markdown] section is always present (skip-directory list).
    expect(dtoGetSettings.markdown).toBeDefined();
    expect(Array.isArray(dtoGetSettings.markdown.skip_dirs)).toBe(true);
    // The [git] section is always present (core-operations list).
    expect(dtoGetSettings.git).toBeDefined();
    expect(Array.isArray(dtoGetSettings.git.core_operations)).toBe(true);
  });

  it("dto-save-endpoints: default_provider/default_model/provider_swapped", () => {
    expect(dtoSaveEndpoints.default_provider === null || typeof dtoSaveEndpoints.default_provider === "string").toBe(true);
    expect(dtoSaveEndpoints.default_model === null || typeof dtoSaveEndpoints.default_model === "string").toBe(true);
    expect(typeof dtoSaveEndpoints.provider_swapped).toBe("boolean");
  });

  it("dto-save-settings: ok + safety (string|null) + vision_configured", () => {
    expect(dtoSaveSettings.ok).toBe(true);
    expect(dtoSaveSettings.safety === null || typeof dtoSaveSettings.safety === "string").toBe(true);
    expect(typeof dtoSaveSettings.vision_configured).toBe("boolean");
  });

  it("dto-codegraph-status: available/indexing + counts + last_indexed_at", () => {
    expect(typeof dtoCodegraphStatus.available).toBe("boolean");
    expect(typeof dtoCodegraphStatus.indexing).toBe("boolean");
    expect(typeof dtoCodegraphStatus.files).toBe("number");
    expect(typeof dtoCodegraphStatus.symbols).toBe("number");
    expect(typeof dtoCodegraphStatus.edges).toBe("number");
    expect(
      dtoCodegraphStatus.last_indexed_at === null ||
        typeof dtoCodegraphStatus.last_indexed_at === "number",
    ).toBe(true);
  });

  it("dto-codegraph-graph: nodes (Symbol wire) + edges (EdgeRow wire)", () => {
    expect(Array.isArray(dtoCodegraphGraph.nodes)).toBe(true);
    expect(Array.isArray(dtoCodegraphGraph.edges)).toBe(true);
    const node = dtoCodegraphGraph.nodes[0];
    expect(typeof node.id).toBe("string");
    expect(typeof node.name).toBe("string");
    // kind is the snake_case SymbolKind form ("function", "type_alias", …).
    expect(typeof node.kind).toBe("string");
    expect(typeof node.file).toBe("string");
    expect(typeof node.start_line).toBe("number");
    expect(typeof node.end_line).toBe("number");
    const e = dtoCodegraphGraph.edges[0];
    expect(typeof e.from_id).toBe("string");
    expect(typeof e.to_id).toBe("string");
    // edge kind is "calls" | "imports" | "contains".
    expect(["calls", "imports", "contains"]).toContain(e.kind);
  });
});