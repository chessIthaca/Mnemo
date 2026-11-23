import { describe, expect, it } from "vitest";
import type { TeamEvent } from "./teamState";
import type { WorkerActivity } from "./types";
import { applyWorkerEvent, applyWorkerStatus, workerPhaseLabel } from "./workerActivity";

const NOW = 1_787_400_000_000;

/** One streamed worker event, the way the host serializes it. */
const event = (kind: string, extra: Record<string, unknown> = {}): TeamEvent => ({
  worker_id: "ana",
  generation: 1,
  event: { agent: 1, event: { kind, ...extra } },
});

const activity = (phase: WorkerActivity["phase"], extra: Partial<WorkerActivity> = {}): WorkerActivity => ({
  phase,
  toolName: null,
  toolArgs: null,
  turnStartedAtMs: null,
  phaseSinceMs: NOW,
  ...extra,
});

describe("applyWorkerEvent", () => {
  it("starts the turn with `starting` and anchors the turn timer", () => {
    const next = applyWorkerEvent(null, event("started"), NOW);
    expect(next).toEqual({ phase: "starting", toolName: null, toolArgs: null, turnStartedAtMs: NOW, phaseSinceMs: NOW });
  });

  it("moves to `thinking` on reasoning deltas", () => {
    const next = applyWorkerEvent(activity("starting", { turnStartedAtMs: NOW - 10_000 }), event("reasoning_delta"), NOW);
    expect(next?.phase).toBe("thinking");
    expect(next?.turnStartedAtMs).toBe(NOW - 10_000);
    expect(next?.phaseSinceMs).toBe(NOW);
  });

  it("moves to `writing` on text deltas", () => {
    const next = applyWorkerEvent(activity("thinking"), event("text_delta"), NOW);
    expect(next?.phase).toBe("writing");
  });

  it("moves to `tool` with the tool's name on a tool call", () => {
    const next = applyWorkerEvent(activity("thinking"), event("tool_call_start", { name: "shell" }), NOW);
    expect(next).toEqual({ phase: "tool", toolName: "shell", toolArgs: "", turnStartedAtMs: null, phaseSinceMs: NOW });
  });

  it("builds a shell call's phrase from its streamed argument fragments", () => {
    const args = '{"command":"cargo test","purpose":"running tests"}';
    let live = applyWorkerEvent(activity("thinking"), event("tool_call_start", { name: "shell", id: "c1" }), NOW);
    // Nothing readable yet — the raw name is the honest fallback.
    expect(workerPhaseLabel(live!)).toBe("Running shell…");
    live = applyWorkerEvent(live, event("tool_call_arg_delta", { index: 0, fragment: args.slice(0, 16) }), NOW);
    expect(workerPhaseLabel(live!)).toBe("Running shell…");
    // The complete purpose is in the body even before the JSON closes.
    live = applyWorkerEvent(live, event("tool_call_arg_delta", { index: 0, fragment: args.slice(16, -1) }), NOW);
    expect(live?.toolArgs).toBe(args.slice(0, -1));
    expect(workerPhaseLabel(live!)).toBe("Running tests…");
    live = applyWorkerEvent(live, event("tool_call_arg_delta", { index: 0, fragment: "}" }), NOW);
    expect(live?.toolArgs).toBe(args);
    expect(workerPhaseLabel(live!)).toBe("Running tests…");
    // The arguments survive the execution start and the output stream.
    live = applyWorkerEvent(live, event("tool_execution_start", { tool_call_id: "c1" }), NOW + 500);
    expect(workerPhaseLabel(live!)).toBe("Running tests…");
    live = applyWorkerEvent(live, event("tool_output_delta", { tool_call_id: "c1", text: "3 passing" }), NOW + 900);
    expect(workerPhaseLabel(live!)).toBe("Running tests…");
  });

  it("builds a Bash call's phrase from the same fragment stream", () => {
    const args = '{"command":"cargo test --workspace","description":"Run the workspace tests"}';
    let live = applyWorkerEvent(activity("thinking"), event("tool_call_start", { name: "Bash", id: "c2" }), NOW);
    for (const fragment of [args.slice(0, 24), args.slice(24, -1), "}"]) {
      live = applyWorkerEvent(live, event("tool_call_arg_delta", { index: 0, fragment }), NOW);
    }
    live = applyWorkerEvent(live, event("tool_execution_start", { tool_call_id: "c2" }), NOW + 500);
    expect(live?.toolArgs).toBe(args);
    expect(workerPhaseLabel(live!)).toBe("Running the workspace tests…");
    expect(workerPhaseLabel(live!)).not.toContain("Using Bash");
  });

  it("holds the tool phase through argument, execution, and output deltas", () => {
    let next = applyWorkerEvent(activity("thinking"), event("tool_call_start", { name: "shell" }), NOW);
    for (const kind of ["tool_call_arg_delta", "tool_execution_start", "tool_output_delta"]) {
      next = applyWorkerEvent(next, event(kind), NOW + 500);
      expect(next?.phase).toBe("tool");
      expect(next?.toolName).toBe("shell");
    }
  });

  it("returns to `thinking` when the tool produces its result", () => {
    const running = activity("tool", { toolName: "shell" });
    const next = applyWorkerEvent(running, event("tool_result"), NOW);
    expect(next?.phase).toBe("thinking");
    expect(next?.toolName).toBeNull();
  });

  it("waits on the human for approvals and questions", () => {
    for (const kind of ["approval_request", "user_question"]) {
      const next = applyWorkerEvent(activity("tool", { toolName: "shell" }), event(kind), NOW);
      expect(next?.phase).toBe("waiting");
      expect(next?.toolName).toBeNull();
      expect(workerPhaseLabel(next!)).toBe("Waiting on you");
    }
  });

  it("clears the activity when the turn finishes or the session exits", () => {
    for (const kind of ["finished", "exited"]) {
      expect(applyWorkerEvent(activity("writing"), event(kind), NOW)).toBeNull();
      expect(applyWorkerEvent(null, event(kind), NOW)).toBeNull();
    }
  });

  it("keeps the same record when an event changes nothing", () => {
    const thinking = activity("thinking");
    expect(applyWorkerEvent(thinking, event("reasoning_delta"), NOW)).toBe(thinking);
    expect(applyWorkerEvent(thinking, event("usage"), NOW)).toBe(thinking);
    expect(applyWorkerEvent(null, event("context_usage"), NOW)).toBeNull();
  });

  it("re-anchors the turn timer when a new turn starts", () => {
    const writing = activity("writing", { turnStartedAtMs: NOW - 30_000, phaseSinceMs: NOW - 5_000 });
    const next = applyWorkerEvent(writing, event("started"), NOW);
    expect(next).toEqual({ phase: "starting", toolName: null, toolArgs: null, turnStartedAtMs: NOW, phaseSinceMs: NOW });
  });
});

describe("applyWorkerStatus", () => {
  it("shows `starting` while the worker is still dispatching, with no turn anchor yet", () => {
    const next = applyWorkerStatus(null, "dispatching", NOW);
    expect(next).toEqual({ phase: "starting", toolName: null, toolArgs: null, turnStartedAtMs: null, phaseSinceMs: NOW });
  });

  it("keeps an existing `starting` record when the snapshot repeats `dispatching`", () => {
    const starting = activity("starting");
    expect(applyWorkerStatus(starting, "dispatching", NOW + 2_000)).toBe(starting);
  });

  it("waits on the user when the worker needs an answer", () => {
    const next = applyWorkerStatus(activity("tool", { toolName: "shell" }), "needs_you", NOW);
    expect(next?.phase).toBe("waiting");
    expect(next?.turnStartedAtMs).toBeNull();
    // Repeated snapshots must not restart the clock.
    expect(applyWorkerStatus(next, "needs_you", NOW + 3_000)).toBe(next);
  });

  it("hands `waiting` back to `thinking` once the worker works again", () => {
    const next = applyWorkerStatus(activity("waiting", { turnStartedAtMs: NOW - 60_000 }), "working", NOW);
    expect(next?.phase).toBe("thinking");
    expect(next?.turnStartedAtMs).toBe(NOW - 60_000);
  });

  it("keeps the streamed phase while a snapshot says the worker is working", () => {
    const writing = activity("writing", { turnStartedAtMs: NOW - 5_000 });
    expect(applyWorkerStatus(writing, "working", NOW)).toBe(writing);
    expect(applyWorkerStatus(writing, "fixing", NOW)).toBe(writing);
    expect(applyWorkerStatus(writing, "merging", NOW)).toBe(writing);
  });

  it("falls back to `starting` when a working worker has not streamed anything yet", () => {
    expect(applyWorkerStatus(null, "working", NOW)?.phase).toBe("starting");
  });

  it("clears the activity for every status that is not running", () => {
    const writing = activity("writing");
    for (const status of ["idle", "paused", "interrupted", "completed", "stopped", "in_review", "conflict", "failed", "merged", "cancelled"]) {
      expect(applyWorkerStatus(writing, status as Parameters<typeof applyWorkerStatus>[1], NOW)).toBeNull();
      expect(applyWorkerStatus(null, status as Parameters<typeof applyWorkerStatus>[1], NOW)).toBeNull();
    }
  });
});

describe("workerPhaseLabel", () => {
  it("labels every plain phase", () => {
    expect(workerPhaseLabel(activity("starting"))).toBe("Starting…");
    expect(workerPhaseLabel(activity("thinking"))).toBe("Thinking…");
    expect(workerPhaseLabel(activity("writing"))).toBe("Writing…");
    expect(workerPhaseLabel(activity("waiting"))).toBe("Waiting on you");
  });

  it("names what the running tool is for, with the raw name as the fallback", () => {
    expect(workerPhaseLabel(activity("tool", { toolName: "shell" }))).toBe("Running shell…");
    expect(workerPhaseLabel(activity("tool", { toolName: "team_status" }))).toBe("Reading team status…");
    expect(workerPhaseLabel(activity("tool", { toolName: "team_message" }))).toBe("Messaging a worker…");
    expect(workerPhaseLabel(activity("tool"))).toBe("Running a tool…");
  });

  it("uses the running call's arguments: a shell purpose and a Bash description", () => {
    const purpose = activity("tool", { toolName: "shell", toolArgs: JSON.stringify({ purpose: "building game" }) });
    expect(workerPhaseLabel(purpose)).toBe("Building game…");
    const bash = activity("tool", {
      toolName: "Bash",
      toolArgs: JSON.stringify({ command: "cargo test --workspace", description: "Run the workspace tests" }),
    });
    expect(workerPhaseLabel(bash)).toBe("Running the workspace tests…");
    expect(workerPhaseLabel(bash)).not.toContain("Using Bash");
  });

  it("falls back to the command's own program without a purpose or description", () => {
    const shell = activity("tool", { toolName: "shell", toolArgs: JSON.stringify({ command: "cargo test --workspace" }) });
    expect(workerPhaseLabel(shell)).toBe("Running tests…");
    const bash = activity("tool", { toolName: "Bash", toolArgs: JSON.stringify({ command: "cargo test --workspace" }) });
    expect(workerPhaseLabel(bash)).toBe("Running tests…");
  });
});