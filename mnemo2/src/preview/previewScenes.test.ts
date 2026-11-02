import { describe, expect, it } from "vitest";
import { createGraphTransport } from "../lib/graph";
import type { AppState, LeadEntry, Worker, WorkerThreadEntry } from "../lib/types";
import { costPreviewInsights } from "../lib/cost";
import {
  defaultPreviewScene,
  previewScene,
  previewScenes,
  type PreviewScene,
} from "../test/previewScenes";

/** One tool row of a worker thread. */
type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

const isToolRow = (entry: WorkerThreadEntry): entry is ToolEntry => entry.kind === "tool";

const isMessage = (entry: WorkerThreadEntry): entry is Exclude<WorkerThreadEntry, ToolEntry> =>
  entry.kind !== "tool";

function build(name: string): AppState {
  const scene = previewScene(name);
  if (!scene) throw new Error(`no preview scene named ${name}`);
  return scene.build();
}

function selectedWorker(state: AppState): Worker {
  const worker = state.workers.find((candidate) => candidate.id === state.selectedWorkerId);
  if (!worker) throw new Error("the scene selects no worker");
  return worker;
}

function toolRows(worker: Worker): ToolEntry[] {
  return worker.messages.filter(isToolRow);
}

describe("preview scenes", () => {
  it("builds every scene's state without throwing", () => {
    for (const scene of previewScenes) {
      const state = scene.build();
      // A scene must render an opened project, never the start screen.
      expect(state.route, scene.name).not.toBe("start");
      expect(state.projectId, scene.name).not.toBeNull();
      expect(state.workers.length, scene.name).toBeGreaterThan(0);
    }
  });

  it("graph routes to the explorer and seeds its preview transport", async () => {
    const state = build("graph");
    expect(state.route).toBe("graph");
    expect(state.projectId).not.toBeNull();
    const transport = createGraphTransport();
    expect(transport).not.toBeNull();
    expect(await transport?.status()).toMatchObject({ available: true, indexing: false });
    expect((await transport?.top(3))?.length).toBe(3);
  });

  it("resolves the default scene for an empty parameter and null for an unknown one", () => {
    const scenes: PreviewScene[] = previewScenes;
    expect(scenes.map((scene) => scene.name)).toEqual([
      "mission-control",
      "graph",
      "files",
      "worker-thread",
      "lead-chat",
      "settings-models",
      "settings-laya",
      "lead-idle",
      "delete-sheet",
      "cost",
      "plans",
    ]);
    expect(previewScene(null)?.name).toBe(defaultPreviewScene);
    expect(previewScene("")?.name).toBe(defaultPreviewScene);
    expect(previewScene("  ")?.name).toBe(defaultPreviewScene);
    expect(previewScene("worker-thread")?.name).toBe("worker-thread");
    expect(previewScene("no-such-scene")).toBeNull();
  });

  it("mission-control selects a worker whose thread shows Markdown prose and every tool row state", () => {
    const state = build("mission-control");
    expect(state.route).toBe("team");

    const worker = selectedWorker(state);
    const messages = worker.messages.filter(isMessage);

    // A lead message with inline code.
    expect(
      messages.some((message) => message.from === "lead" && /`[^`]+`/.test(message.body)),
    ).toBe(true);

    // A quoted plan carrying an h2, an h3, a list and a fenced code block.
    const quotedPlan = messages.find(
      (message) => message.from === "lead" && message.body.includes("## ") && message.body.includes("### "),
    );
    expect(quotedPlan).toBeDefined();
    const plan = quotedPlan?.body ?? "";
    expect(plan.split("\n").some((line) => line.startsWith("> ## "))).toBe(true);
    expect(plan.split("\n").some((line) => line.startsWith("> ### "))).toBe(true);
    expect(plan.split("\n").some((line) => line.startsWith("> - "))).toBe(true);
    expect(plan.includes("```")).toBe(true);

    // Tool rows: one finished, one failed, one still running.
    const rows = toolRows(worker);
    expect(rows.some((row) => row.result?.success === true)).toBe(true);
    expect(rows.some((row) => row.result?.success === false)).toBe(true);
    expect(rows.some((row) => row.result === null && row.liveOutput.length > 0)).toBe(true);

    // A worker reply and an inbox message.
    expect(messages.some((message) => message.from === "worker")).toBe(true);
    expect(worker.inbox.length).toBeGreaterThan(0);
  });

  it("worker-thread keeps the same thread on a needs-you worker beside the lead chat", () => {
    const state = build("worker-thread");
    const worker = selectedWorker(state);
    expect(state.route).toBe("lead");
    expect(state.workerPanelId).toBe(worker.id);
    expect(worker.status).toBe("needs_you");
    expect(worker.needsYou).toBe(true);
    expect(toolRows(worker).length).toBe(toolRows(selectedWorker(build("mission-control"))).length);
  });

  it("lead-chat shows the lead conversation with a tool row", () => {
    const state = build("lead-chat");
    expect(state.route).toBe("lead");
    expect(state.leadEntries.some((entry) => entry.kind === "message")).toBe(true);
    expect(state.leadEntries.some((entry) => entry.kind === "tool")).toBe(true);
    expect(state.leadEntries.length).toBeGreaterThan(4);
  });

  it("lead-idle marks the external lead idle without attaching it", () => {
    const { externalLead } = build("lead-idle");
    expect(externalLead.attached).toBe(false);
    expect(externalLead.root).toBeTruthy();
    expect(externalLead.sinceMs).toBeNull();
    expect(typeof externalLead.idleSinceMs).toBe("number");
  });

  it("delete-sheet opens the delete-old sheet with one superseded worker", () => {
    const request = build("delete-sheet").workerDeletion;
    expect(request).not.toBeNull();
    expect(request?.busy).toBe(false);
    expect(request?.previews).toHaveLength(1);
    expect(request?.previews[0].superseded_by).toBeTruthy();
    expect(request?.previews[0].unmerged_commits).toBeGreaterThan(0);
  });

  it("cost opens the Cost tab with the reports the browser preview renders", () => {
    const state = build("cost");
    expect(state.route).toBe("cost");
    const insights = costPreviewInsights();
    expect(insights).not.toBeNull();
    expect(insights?.cost.models.length).toBe(3);
    expect(insights?.cost.unpriced_models).toEqual(["ollama/qwen3:32b"]);
    expect(insights?.cost.per_day).toHaveLength(14);
    expect(insights?.savings.levers.some((lever) => lever.kind === "delta_read")).toBe(true);
  });

  it("mission-control shows one running worker in every live phase", () => {
    const state = build("mission-control");
    const live = new Map(
      state.workers
        .filter((worker) => worker.activity)
        .map((worker) => [worker.activity!.phase, worker.id] as const),
    );
    expect([...live.keys()].sort()).toEqual(["starting", "thinking", "tool", "waiting", "writing"]);
    // The selected worker runs the shell its thread's last tool row streams.
    expect(live.get("tool")).toBe("kai");
    // Every live row ticks its timer from when the worker was created.
    for (const worker of state.workers.filter((candidate) => candidate.activity)) {
      expect(worker.createdAtMs).toBeGreaterThan(0);
    }
  });

  it("worker-thread waits on the human with the turn timer still anchored", () => {
    const worker = selectedWorker(build("worker-thread"));
    expect(worker.activity?.phase).toBe("waiting");
    expect(worker.activity?.turnStartedAtMs).not.toBeNull();
    expect(worker.createdAtMs).toBeGreaterThan(0);
  });
});
