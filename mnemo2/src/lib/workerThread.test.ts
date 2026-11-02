import { afterEach, describe, expect, it, vi } from "vitest";
import type { TeamEvent, TeamWorkerRecord } from "./teamState";
import type { WorkerThreadEntry } from "./types";
import {
  applyWorkerLiveEvent,
  CONTEXT_FOOTER,
  emptyWorkerLive,
  workerThreadFromRecord,
} from "./workerThread";

function record(messages: TeamWorkerRecord["transcript"]["messages"]): TeamWorkerRecord {
  return {
    id: "w1",
    name: "Worker w1",
    brief: "Build the parser",
    model: "test/scripted-a",
    model_id: "scripted-a",
    branch: "wt/agent-w1",
    worktree: "C:/repo/.worktrees/agent-w1",
    status: "working",
    generation: 1,
    transcript: { messages },
    pending: null,
    used_tokens: 0,
    max_tokens: 0,
    total_tokens: 0,
    prompt_tokens: 0,
    completion_tokens: 0,
    cached_tokens: 0,
    requests: 0,
    cost_micros: null,
    created_at_ms: 0,
    elapsed_ms: 0,
    turns: 0,
    last_activity_ms: 0,
    checks: [],
    findings: [],
    review: null,
    fixer_finding_id: null,
    merge: null,
    error: null,
  } as TeamWorkerRecord;
}

/** A compact view of a thread: who said what, or which tool ran. */
function shape(entries: WorkerThreadEntry[]): string[] {
  return entries.map((entry) =>
    entry.kind === "tool"
      ? `tool:${entry.toolName}:${entry.result === null ? "running" : entry.result.success ? "ok" : "failed"}`
      : `${entry.from}:${entry.body}`,
  );
}

const event = (kind: string, extra: Record<string, unknown> = {}): TeamEvent => ({
  worker_id: "w1",
  generation: 1,
  event: { agent: 7, event: { kind, ...extra } },
});

describe("worker thread", () => {
  afterEach(() => vi.restoreAllMocks());

  it("shows the worker's Markdown, tools, and reports — not prompts or leaked scaffolding", () => {
    const thread = workerThreadFromRecord(record([
      { role: "system", content: "You are a Mnemo2 executor worker." },
      { role: "user", content: "Build the parser" },
      {
        role: "assistant",
        content: "## Plan\n\n- read the spec",
        tool_calls: [
          { id: "c1", name: "read_files", arguments: '{"files":[{"path":"SPEC.md"}]}' },
          { id: "c2", name: "message_parent", arguments: '{"text":"**Blocked:** need the API key"}' },
        ],
      },
      { role: "tool", tool_call_id: "c1", name: "read_files", content: "=== SPEC.md ===" },
      { role: "tool", tool_call_id: "c2", name: "message_parent", content: "message sent to the lead" },
      { role: "user", content: "# WORKFLOW STATE\n\nCurrent state: Executing" },
      { role: "user", content: CONTEXT_FOOTER },
      { role: "user", content: "[harness note] continue from where you left off." },
      { role: "assistant", content: "Done: parser committed." },
    ]));
    expect(shape(thread)).toEqual([
      "lead:Build the parser",
      "worker:## Plan\n\n- read the spec",
      "tool:read_files:ok",
      "worker:**Blocked:** need the API key",
      "harness:[harness note] continue from where you left off.",
      "worker:Done: parser committed.",
    ]);
  });

  it("shows inbox reports before the first turn without a dispatched brief bubble", () => {
    const thread = workerThreadFromRecord(record([]), [
      { worker_id: "w1", text: "halfway there", at_ms: 1_700_000_000_000 },
      { worker_id: "other", text: "not mine", at_ms: 1_700_000_000_000 },
    ]);
    expect(shape(thread)).toEqual(["worker:halfway there"]);
    expect(thread.some((entry) => entry.time === "dispatched")).toBe(false);
  });

  it("does not repeat an inbox report the transcript already holds", () => {
    const thread = workerThreadFromRecord(
      record([
        { role: "user", content: "Build the parser" },
        { role: "assistant", content: "", tool_calls: [{ id: "c1", name: "message_parent", arguments: '{"text":"halfway there"}' }] },
      ]),
      [{ worker_id: "w1", text: "halfway there", at_ms: 1_700_000_000_000 }],
    );
    expect(shape(thread)).toEqual(["lead:Build the parser", "worker:halfway there"]);
  });

  it("streams text, tool calls, output, results, and reports live", () => {
    let live = emptyWorkerLive(1, 0);
    for (const next of [
      event("started"),
      event("text_delta", { text: "## Progress\n\n" }),
      event("text_delta", { text: "Parser **done**" }),
      event("tool_call_start", { index: 0, id: "c1", name: "shell" }),
      event("tool_call_arg_delta", { index: 0, fragment: '{"command":' }),
      event("tool_call_arg_delta", { index: 0, fragment: '"npm test"}' }),
      event("tool_output_delta", { tool_call_id: "c1", stream: "stdout", text: "3 passing" }),
    ]) {
      live = applyWorkerLiveEvent(live, next);
    }
    expect(shape(live.entries)).toEqual(["worker:## Progress\n\nParser **done**", "tool:shell:running"]);
    const running = live.entries[1];
    expect(running.kind === "tool" && running.arguments).toBe('{"command":"npm test"}');
    expect(running.kind === "tool" && running.liveOutput).toBe("3 passing");

    for (const next of [
      event("tool_result", { tool_call_id: "c1", tool_name: "shell", result: { success: true, output: "3 passing" } }),
      event("tool_call_start", { index: 0, id: "c2", name: "message_parent" }),
      event("host_tool_call", { call_id: "c2", name: "message_parent", args: { text: "Ready for **review**" } }),
      event("tool_result", { tool_call_id: "c2", result: { success: true, output: "message sent to the lead" } }),
      event("text_delta", { text: "Committed." }),
    ]) {
      live = applyWorkerLiveEvent(live, next);
    }
    expect(shape(live.entries)).toEqual([
      "worker:## Progress\n\nParser **done**",
      "tool:shell:ok",
      "worker:Ready for **review**",
      "worker:Committed.",
    ]);
  });

  it("times sequentially executed calls from execution start, not batch announcement", () => {
    const now = vi.spyOn(Date, "now");
    let live = emptyWorkerLive(1, 0);
    now.mockReturnValue(1_000);
    live = applyWorkerLiveEvent(live, event("tool_call_start", { index: 0, id: "c1", name: "shell" }));
    live = applyWorkerLiveEvent(live, event("tool_call_start", { index: 1, id: "c2", name: "shell" }));

    now.mockReturnValue(2_000);
    live = applyWorkerLiveEvent(live, event("tool_execution_start", { tool_call_id: "c1" }));
    now.mockReturnValue(4_000);
    live = applyWorkerLiveEvent(live, event("tool_result", {
      tool_call_id: "c1",
      result: { success: true, output: "first" },
    }));
    now.mockReturnValue(6_000);
    live = applyWorkerLiveEvent(live, event("tool_execution_start", { tool_call_id: "c2" }));
    now.mockReturnValue(9_000);
    live = applyWorkerLiveEvent(live, event("tool_result", {
      tool_call_id: "c2",
      result: { success: true, output: "second" },
    }));

    const tools = live.entries.filter((entry) => entry.kind === "tool");
    expect(tools.map((entry) => entry.kind === "tool" && [entry.startedAtMs, entry.endedAtMs])).toEqual([
      [2_000, 4_000],
      [6_000, 9_000],
    ]);

    // A late or duplicate execution event must never reopen a finished card.
    now.mockReturnValue(12_000);
    live = applyWorkerLiveEvent(live, event("tool_execution_start", { tool_call_id: "c1" }));
    const first = live.entries.find((entry) => entry.kind === "tool" && entry.toolCallId === "c1");
    expect(first?.kind === "tool" && first.startedAtMs).toBe(2_000);
    expect(first?.kind === "tool" && first.endedAtMs).toBe(4_000);
  });
});
