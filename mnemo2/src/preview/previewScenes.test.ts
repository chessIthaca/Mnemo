import { describe, expect, it } from "vitest";
import { createGraphTransport } from "../lib/graph";
import { canMoveMain, createGitTransport } from "../lib/git";
import type { AppState, LeadEntry, Worker, WorkerThreadEntry } from "../lib/types";
import { costPreviewInsights } from "../lib/cost";
import { createMemoryTransport } from "../lib/memoryView";
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
    // The scene opens mid-rebuild, so the Graph tab's rebuild notice is visible.
    expect(state.indexing?.code).toMatchObject({ state: "running", done: 1200, total: 2400 });
    expect(await transport?.status()).toMatchObject({ available: true, indexing: true });
    expect((await transport?.top(3))?.length).toBe(3);
    // The scene opens on a whole-graph module map, not an empty canvas.
    const overview = await transport?.overview(2, 60);
    expect(overview?.level).toBe(2);
    expect(overview?.nodes.length).toBeGreaterThanOrEqual(30);
    expect(overview?.edges.length).toBeGreaterThan(20);
    expect(overview?.nodes.every((node) => node.kind === null)).toBe(true);
    const symbols = await transport?.overview(0, 40);
    expect(symbols?.level).toBe(0);
    expect(symbols?.nodes.every((node) => node.kind !== null)).toBe(true);
  });

  it("resolves the default scene for an empty parameter and null for an unknown one", () => {
    const scenes: PreviewScene[] = previewScenes;
    expect(scenes.map((scene) => scene.name)).toEqual([
      "mission-control",
      "graph",
      "files",
      "trace",
      "git",
      "worker-thread",
      "lead-chat",
      "settings-models",
      "settings-laya",
      "settings-safety",
      "settings-mcp",
      "settings-appearance",
      "mission-control-dark",
      "lead-chat-dark",
      "graph-dark",
      "lead-idle",
      "delete-sheet",
      "cost",
      "plans",
      "memory",
    ]);
    expect(previewScene(null)?.name).toBe(defaultPreviewScene);
    expect(previewScene("")?.name).toBe(defaultPreviewScene);
    expect(previewScene("  ")?.name).toBe(defaultPreviewScene);
    expect(previewScene("worker-thread")?.name).toBe("worker-thread");
    expect(previewScene("no-such-scene")).toBeNull();
  });

  it("pins the dark palette on the dark variants only", () => {
    const dark = previewScenes.filter((scene) => scene.theme === "dark").map((scene) => scene.name);
    expect(dark).toEqual(["mission-control-dark", "lead-chat-dark", "graph-dark"]);
    expect(previewScenes.filter((scene) => scene.theme === undefined).length).toBe(previewScenes.length - dark.length);
  });

  it("memory seeds the store with 20 records and opens the newest one", async () => {
    const state = build("memory");
    expect(state.route).toBe("memory");
    expect(state.projectId).not.toBeNull();

    const transport = createMemoryTransport();
    expect(transport).not.toBeNull();

    const page = await transport!.list({});
    expect(page.rows.length).toBeGreaterThan(0);
    expect(page.has_more).toBe(false);
    expect(new Set(page.rows.map((row) => row.kind))).toEqual(
      new Set(["spec", "decision", "bug", "how"]),
    );

    const newest = page.rows[0];
    const detail = await transport!.read(newest.id);
    expect(detail.markdown).toContain("## Why the store and not the files");
    expect(detail.path).toContain(".coding/knowledge/spec/");
    expect(detail.links.length).toBeGreaterThan(0);
    expect(detail.backlinks.length).toBeGreaterThan(0);

    const specs = await transport!.list({ kind: "spec" });
    expect(specs.rows.every((row) => row.kind === "spec")).toBe(true);
    const search = await transport!.search("graph");
    expect(search.length).toBeGreaterThan(0);

    const overview = await transport!.overview();
    expect(overview.available).toBe(true);
    expect(overview.counts.semantic).toBeGreaterThan(0);

    const recall = await transport!.recallTest("memory store", 4);
    expect(recall.length).toBe(4);
    expect(recall[0].score).toBeGreaterThanOrEqual(recall[3].score);
  });

  it("git seeds one fast-forwardable branch and a commit lane", async () => {
    const state = build("git");
    expect(state.workbenchOpen).toBe(true);
    expect(state.workbenchTab).toBe("git");

    const transport = createGitTransport();
    expect(transport).not.toBeNull();
    const overview = await transport!.overview();
    expect(overview.branch).toBe("wt/agent-gitTab95");
    expect(overview.main_worktree).toBeNull();
    expect(overview.commits.length).toBeGreaterThan(1);
    const movable = overview.branches.filter((branch) => canMoveMain(branch, overview.main_worktree));
    expect(movable.map((branch) => branch.name)).toEqual(["wt/agent-gitTab95"]);

    const moved = await transport!.moveMain(movable[0].name);
    expect(moved.commits).toBeGreaterThan(0);
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
    expect(request?.phase).toBe("ready");
    expect(request?.rows).toHaveLength(1);
    expect(request?.rows[0].preview?.superseded_by).toBeTruthy();
    expect(request?.rows[0].preview?.unmerged_commits).toBeGreaterThan(0);
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
