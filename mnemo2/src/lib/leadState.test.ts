import { describe, expect, it } from "vitest";
import {
  applyLeadEvent,
  applyLeadSnapshot,
  initialLeadRuntime,
} from "./leadState";
import { createInitialState } from "./store";
import type { AppState, LeadEvent, LeadSnapshot } from "./types";

function nativeState(generation = 3): AppState {
  const state = createInitialState();
  state.projectId = "mnemo";
  state.leadRuntime = {
    ...initialLeadRuntime(),
    mode: "native",
    status: "ready",
    generation,
  };
  return state;
}

function textEvent(generation: number, text: string): LeadEvent {
  return {
    generation,
    event: { agent: 1, event: { kind: "text_delta", text } },
  };
}

describe("lead event reducer", () => {
  it("accumulates streamed deltas into one lead entry", () => {
    const first = applyLeadEvent(nativeState(3), textEvent(3, "Hello "));
    const second = applyLeadEvent(first, textEvent(3, "world"));
    expect(second.leadEntries.at(-1)).toMatchObject({
      kind: "message",
      author: "lead",
      body: "Hello world",
    });
    expect(second.leadRuntime.streamingEntryId).toBeTruthy();
  });

  it("ignores events from a replaced session generation", () => {
    const state = nativeState(3);
    expect(applyLeadEvent(state, textEvent(2, "stale"))).toBe(state);
  });

  it("tracks cancellation from Started through Finished", () => {
    const started = applyLeadEvent(nativeState(), {
      generation: 3,
      event: { agent: 1, event: { kind: "started" } },
    });
    expect(started.leadRuntime.busy).toBe(true);
    const finished = applyLeadEvent(started, {
      generation: 3,
      event: { agent: 1, event: { kind: "finished" } },
    });
    expect(finished.leadRuntime.busy).toBe(false);
    expect(finished.leadRuntime.streamingEntryId).toBeNull();
  });

  it("maps a real approval event to a resolvable pending interaction", () => {
    const pending = applyLeadEvent(nativeState(3), {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "approval_request",
          tool_call_id: "call-1",
          tool_name: "write_file",
          args: {},
          core_operation: false,
        },
      },
    });
    expect(pending.leadRuntime.busy).toBe(true);
    expect(pending.leadRuntime.pending).toMatchObject({
      kind: "approval",
      request_id: "call-1",
      tool_name: "write_file",
    });

    const merged = applyLeadSnapshot(pending, {
      generation: 3,
      model: "test/scripted-a",
      model_id: "scripted-a",
      models: [],
      default_model: null,
      transcript: { messages: [] },
      used_tokens: 0,
      max_tokens: 1000,
      pending: { kind: "approval", request_id: "call-1" },
      busy: true,
    });
    expect(merged.leadRuntime.pending).toMatchObject({
      kind: "approval",
      request_id: "call-1",
      tool_name: "write_file",
    });
    expect(merged.leadRuntime.busy).toBe(true);
  });

  it("preserves a real question across the snapshot merge", () => {
    const pending = applyLeadEvent(nativeState(3), {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "user_question",
          question_id: "q-1",
          question: "Which branch?",
          options: [{ label: "main" }, { label: "wt/mnemo" }],
        },
      },
    });
    expect(pending.leadRuntime.busy).toBe(true);
    expect(pending.leadRuntime.pending).toMatchObject({
      kind: "question",
      question_id: "q-1",
      question: "Which branch?",
    });

    const merged = applyLeadSnapshot(pending, {
      generation: 3,
      model: "test/scripted-a",
      model_id: "scripted-a",
      models: [],
      default_model: null,
      transcript: { messages: [] },
      used_tokens: 0,
      max_tokens: 1000,
      pending: { kind: "question", question_id: "q-1" },
      busy: true,
    });
    expect(merged.leadRuntime.pending).toMatchObject({
      kind: "question",
      question_id: "q-1",
      question: "Which branch?",
      options: [{ label: "main" }, { label: "wt/mnemo" }],
    });
  });

  it("restores the transcript and configured model from a snapshot", () => {
    const snapshot: LeadSnapshot = {
      generation: 7,
      model: "test/scripted-b",
      model_id: "scripted-b",
      models: [
        {
          endpoint: "test",
          model: "scripted-b",
          spec: "test/scripted-b",
          is_default: false,
        },
      ],
      default_model: "test/scripted-a",
      transcript: {
        messages: [
          { role: "user", content: "Remember this" },
          { role: "assistant", content: [{ type: "text", text: "Saved" }] },
          { role: "tool", content: "ignored" },
        ],
      },
      used_tokens: 42,
      max_tokens: 2000,
      pending: null,
      busy: false,
    };
    const state = applyLeadSnapshot(nativeState(), snapshot);
    expect(state.leadRuntime).toMatchObject({
      mode: "native",
      status: "ready",
      generation: 7,
      model: "test/scripted-b",
      modelId: "scripted-b",
      usedTokens: 42,
      maxTokens: 2000,
    });
    expect(state.leadEntries).toHaveLength(2);
    expect(state.leadEntries[0]).toMatchObject({ author: "user", body: "Remember this" });
    expect(state.leadEntries[1]).toMatchObject({ author: "lead", body: "Saved" });
    expect(state.projects.find((project) => project.id === "mnemo")?.leadModel).toBe(
      "scripted-b",
    );
  });
});
