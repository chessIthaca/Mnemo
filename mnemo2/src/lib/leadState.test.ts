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
  it("restores the actual lead plan and marks only its first open step active", () => {
    const snapshot: LeadSnapshot = {
      generation: 3,
      model: "test/scripted-a",
      model_id: "scripted-a",
      models: [],
      default_model: null,
      transcript: { messages: [] },
      plan: {
        id: "plan-1",
        title: "Actual plan",
        goal: "Read real state",
        steps: [
          { index: 0, text: "**Read** — src/a.rs", header: "Read", done: true },
          { index: 1, text: "**Build** — src/b.rs", header: "Build", done: false },
          { index: 2, text: "Verify src/c.rs", done: false },
        ],
      },
      used_tokens: 0,
      max_tokens: 1000,
      pending: null,
      busy: false,
    };
    const next = applyLeadSnapshot(nativeState(), snapshot);
    expect(next.plan?.title).toBe("Actual plan");
    expect(next.planItems.map((step) => step.status)).toEqual(["done", "active", "queued"]);
    expect(next.planItems[0].detail).toBe("src/a.rs");
    expect(next.planItems.every((step) => step.owner === "lead")).toBe(true);
  });

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
    expect(state.leadEntries).toHaveLength(3);
    expect(state.leadEntries[0]).toMatchObject({ author: "user", body: "Remember this" });
    expect(state.leadEntries[1]).toMatchObject({ author: "lead", body: "Saved" });
    expect(state.leadEntries[2]).toMatchObject({
      kind: "tool",
      toolCallId: "",
      toolName: "tool",
      result: { success: true, output: "ignored" },
    });
  });

  it("keeps real tool activity between the text that surrounds it", () => {
    let state = applyLeadEvent(nativeState(3), textEvent(3, "Checking the tree."));
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"command":' },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_call_arg_delta",
          index: 0,
          fragment: ' "git status"}',
        },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_output_delta",
          tool_call_id: "call-1",
          stream: "stdout",
          text: "clean",
        },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_result",
          tool_call_id: "call-1",
          result: { success: true, output: "working tree clean" },
        },
      },
    });
    state = applyLeadEvent(state, textEvent(3, "The tree is clean."));

    expect(state.leadEntries).toHaveLength(3);
    expect(state.leadEntries[0]).toMatchObject({
      kind: "message",
      body: "Checking the tree.",
    });
    expect(state.leadEntries[1]).toMatchObject({
      kind: "tool",
      toolCallId: "call-1",
      toolName: "shell",
      arguments: '{"command": "git status"}',
      liveOutput: "",
      result: { success: true, output: "working tree clean" },
    });
    expect(state.leadEntries[2]).toMatchObject({
      kind: "message",
      body: "The tree is clean.",
    });
  });

  it("shows a host tool as running and clears its pending state on the result", () => {
    const running = applyLeadEvent(nativeState(3), {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "host_tool_call",
          call_id: "host-1",
          name: "spawn_agent",
          args: { task: "review the diff" },
        },
      },
    });
    expect(running.leadEntries.at(-1)).toMatchObject({
      kind: "tool",
      toolCallId: "host-1",
      toolName: "spawn_agent",
      arguments: '{"task":"review the diff"}',
      result: null,
    });
    expect(running.leadRuntime.pending).toMatchObject({
      kind: "host_tool_call",
      request_id: "host-1",
      tool_call_id: "host-1",
      tool_name: "spawn_agent",
    });

    const completed = applyLeadEvent(running, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_result",
          tool_call_id: "host-1",
          result: { success: true, output: "review started" },
        },
      },
    });
    expect(completed.leadRuntime.pending).toBeNull();
    expect(completed.leadEntries.at(-1)).toMatchObject({
      kind: "tool",
      toolCallId: "host-1",
      result: { success: true, output: "review started" },
    });
  });

  it("ignores unknown and late output chunks without changing state", () => {
    let state = applyLeadEvent(nativeState(3), {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
      },
    });
    const unknown = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_output_delta",
          tool_call_id: "call-unknown",
          stream: "stdout",
          text: "stray",
        },
      },
    });
    expect(unknown).toBe(state);

    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_result",
          tool_call_id: "call-1",
          result: { success: true, output: "done" },
        },
      },
    });
    const late = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_output_delta",
          tool_call_id: "call-1",
          stream: "stdout",
          text: "late",
        },
      },
    });
    expect(late).toBe(state);
  });

  it("bounds live tool output to its 16 KiB tail", () => {
    let state = applyLeadEvent(nativeState(3), {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_output_delta",
          tool_call_id: "call-1",
          stream: "stdout",
          text: "a".repeat(20_000),
        },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_output_delta",
          tool_call_id: "call-1",
          stream: "stdout",
          text: "b",
        },
      },
    });
    const entry = state.leadEntries.at(-1);
    expect(entry).toMatchObject({ kind: "tool" });
    if (entry?.kind === "tool") {
      expect(entry.liveOutput).toHaveLength(16 * 1024);
      expect(entry.liveOutput.endsWith("b")).toBe(true);
    }
  });

  it("drops a split surrogate at the live-tail cut", () => {
    const cap = 16 * 1024;
    let state = applyLeadEvent(nativeState(3), {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_output_delta",
          tool_call_id: "call-1",
          stream: "stdout",
          text: "\uD83D\uDE00" + "a".repeat(cap - 1),
        },
      },
    });
    const entry = state.leadEntries.at(-1);
    expect(entry).toMatchObject({ kind: "tool" });
    if (entry?.kind === "tool") {
      expect(entry.liveOutput).toHaveLength(cap - 1);
      expect(entry.liveOutput).toBe("a".repeat(cap - 1));
      expect(entry.liveOutput.charCodeAt(0)).toBe("a".charCodeAt(0));
    }
  });

  it("restores persisted tool calls and their results from a snapshot", () => {
    const state = applyLeadSnapshot(nativeState(), {
      generation: 4,
      model: "test/scripted-a",
      model_id: "scripted-a",
      models: [],
      default_model: null,
      transcript: {
        messages: [
          { role: "user", content: "Run the checks" },
          {
            role: "assistant",
            content: "Starting with the tests.",
            tool_calls: [
              {
                id: "call-1",
                name: "shell",
                arguments: '{"command":"cargo test"}',
              },
            ],
          },
          {
            role: "tool",
            content: "all tests passed",
            tool_call_id: "call-1",
            name: "shell",
          },
          {
            role: "assistant",
            content: "",
            tool_calls: [
              { id: "call-2", name: "read_files", arguments: '{"files":[]}' },
            ],
          },
          {
            role: "tool",
            content: "no files matched",
            tool_call_id: "call-2",
            name: "read_files",
            tool_is_error: true,
          },
        ],
      },
      used_tokens: 10,
      max_tokens: 1000,
      pending: null,
      busy: false,
    });

    expect(state.leadEntries).toHaveLength(4);
    expect(state.leadEntries[0]).toMatchObject({ kind: "message", author: "user" });
    expect(state.leadEntries[1]).toMatchObject({
      kind: "message",
      author: "lead",
      body: "Starting with the tests.",
    });
    expect(state.leadEntries[2]).toMatchObject({
      kind: "tool",
      toolCallId: "call-1",
      toolName: "shell",
      arguments: '{"command":"cargo test"}',
      result: { success: true, output: "all tests passed" },
    });
    expect(state.leadEntries[3]).toMatchObject({
      kind: "tool",
      toolCallId: "call-2",
      toolName: "read_files",
      result: { success: false, output: "no files matched" },
    });
  });

  it("scopes argument fragments to the current turn", () => {
    let state = applyLeadEvent(nativeState(3), {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "old-call", name: "shell" },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"old":true}' },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: { agent: 1, event: { kind: "started" } },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "new-call", name: "shell" },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"new":true}' },
      },
    });

    expect(
      state.leadEntries.find(
        (entry) => entry.kind === "tool" && entry.toolCallId === "old-call",
      ),
    ).toMatchObject({ arguments: '{"old":true}' });
    expect(
      state.leadEntries.find(
        (entry) => entry.kind === "tool" && entry.toolCallId === "new-call",
      ),
    ).toMatchObject({ arguments: '{"new":true}' });
  });

  it("preserves restored finalized calls and scopes live calls after a snapshot", () => {
    let state = applyLeadSnapshot(nativeState(3), {
      generation: 3,
      model: "test/scripted-a",
      model_id: "scripted-a",
      models: [],
      default_model: null,
      transcript: {
        messages: [
          {
            role: "assistant",
            content: "",
            tool_calls: [
              { id: "restored-1", name: "shell", arguments: '{"old":1}' },
            ],
          },
          {
            role: "tool",
            content: "done",
            tool_call_id: "restored-1",
            name: "shell",
          },
        ],
      },
      used_tokens: 0,
      max_tokens: 1000,
      pending: null,
      busy: false,
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: { agent: 1, event: { kind: "started" } },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "restored-1", name: "shell" },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_start", index: 0, id: "live-2", name: "shell" },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"new":2}' },
      },
    });
    state = applyLeadEvent(state, {
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "tool_result",
          tool_call_id: "live-2",
          result: { success: true, output: "new done" },
        },
      },
    });

    expect(
      state.leadEntries.find(
        (entry) => entry.kind === "tool" && entry.toolCallId === "restored-1",
      ),
    ).toMatchObject({
      arguments: '{"old":1}',
      result: { success: true, output: "done" },
    });
    expect(
      state.leadEntries.find(
        (entry) => entry.kind === "tool" && entry.toolCallId === "live-2",
      ),
    ).toMatchObject({
      arguments: '{"new":2}',
      result: { success: true, output: "new done" },
    });
  });
});
