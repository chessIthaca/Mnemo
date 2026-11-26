import { fireEvent, screen, waitFor } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";
import { createGraphTransport } from "../lib/graph";
import { canMoveMain, createGitTransport } from "../lib/git";
import type { AppState, LeadEntry, Worker, WorkerErrorEntry, WorkerModelSwitchEntry, WorkerThreadEntry } from "../lib/types";
import { costPreviewInsights } from "../lib/cost";
import { createMemoryTransport } from "../lib/memoryView";
import { hostAway, hostBadgeTitle } from "../lib/workerHost";
import {
  defaultPreviewScene,
  previewScene,
  previewScenes,
  previewWorkerHostsTransport,
  type PreviewScene,
} from "../test/previewScenes";

/** One tool row of a worker thread. */
type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

const isToolRow = (entry: WorkerThreadEntry): entry is ToolEntry => entry.kind === "tool";

const isMessage = (
  entry: WorkerThreadEntry,
): entry is Exclude<WorkerThreadEntry, ToolEntry | WorkerErrorEntry | WorkerModelSwitchEntry> =>
  entry.kind !== "tool" && entry.kind !== "error" && entry.kind !== "model_switch";

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

  it("graph opens in the panel and seeds its preview transport", async () => {
    const state = build("graph");
    expect(state.workbenchOpen).toBe(true);
    expect(state.workbenchTab).toBe("graph");
    expect(state.projectId).not.toBeNull();
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
      "remote-workers",
      "graph",
      "files",
      "git",
      "worker-thread",
      "lead-chat",
      "settings-models",
      "settings-machine",
      "settings-tiers",
      "settings-safety",
      "settings-mcp",
      "settings-appearance",
      "settings-project",
      "worker-hosts",
      "mission-control-dark",
      "lead-chat-dark",
      "graph-dark",
      "lead-idle",
      "delete-sheet",
      "cost",
      "plans",
      "memory",
      "trace",
      "trace-empty",
      "panel-team-diff",
      "panel-team-graph",
      "panel-cost-trace",
      "panel-plans-memory",
      "lead-tool-rows",
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
    expect(state.workbenchOpen).toBe(true);
    expect(state.workbenchTab).toBe("memory");
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
    // The composer's status line carries the running memory re-embed.
    expect(state.indexing?.embeddings).toMatchObject({ state: "running", done: 3200, total: 12444 });
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
    expect(insights?.savings.levers.some((lever) => lever.kind === "outline")).toBe(true);
    // The retired levers are not in the sample: the engine never reports them.
    expect(insights?.savings.levers.some((lever) => lever.kind === "delta_read" || lever.kind === "skeleton")).toBe(false);
    // The label table dropped `truncation` — nothing emits it any more.
    expect(insights?.savings.levers.some((lever) => lever.kind === "truncation")).toBe(false);
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

  it("remote-workers shows a host badge, an away badge only while active, and no away state once merged", () => {
    const state = build("remote-workers");
    const workers = new Map(state.workers.map((worker) => [worker.id, worker]));

    // The local worker keeps no host at all: the snapshot leaves `remote` out.
    expect(workers.get("milo")?.host ?? null).toBeNull();

    // A connected host: the badge names the computer and shows no away time.
    const connected = workers.get("sol");
    expect(connected?.host).toEqual({ name: "studio", os: "macos", offlineSinceMs: null, ssh: null });
    expect(hostAway(connected!, Date.now())).toBeNull();

    // The away host of an active worker: the badge reads "studio · away …".
    const away = workers.get("kai");
    expect(away?.status).toBe("working");
    expect(away?.host?.name).toBe("studio");
    expect(hostBadgeTitle(away!)).toBe("Runs on studio (macOS)");
    const offlineSinceMs = away?.host?.offlineSinceMs ?? 0;
    expect(offlineSinceMs).toBeGreaterThan(0);
    expect(hostAway(away!, offlineSinceMs + 95_500)).toBe(95);
    expect(hostAway(away!, Date.now())).toBeGreaterThanOrEqual(95);

    // A merged worker keeps its host's offline mark but never shows an away
    // state: the link loss no longer changes the worker (D6).
    const merged = workers.get("ren");
    expect(merged?.status).toBe("merged");
    expect(merged?.host?.offlineSinceMs).not.toBeNull();
    expect(hostAway(merged!, Date.now())).toBeNull();

    // The away worker is selected, so the detail shows the host pill too.
    expect(state.selectedWorkerId).toBe(away?.id);
  });

  it("worker-hosts mounts the real page against its in-memory hosts", async () => {
    const scene = previewScene("worker-hosts");
    if (!scene?.mount) throw new Error("the worker-hosts scene mounts the page");
    const container = document.createElement("div");
    document.body.append(container);
    const unmount = scene.mount(container);
    try {
      // The two paired computers, with the connected one's link, load and busy
      // state and the away one's failed checks.
      await waitFor(() => expect(screen.getByText("studio")).toBeTruthy());
      expect(screen.getByText("old-laptop")).toBeTruthy();
      expect(screen.getByText("Linked to studio-ssh")).toBeTruthy();
      expect(screen.getByText("1/2 busy")).toBeTruthy();
      expect(screen.getByText("checks failed: cargo not found")).toBeTruthy();
      expect(screen.getByText(/away since 2 h ago/)).toBeTruthy();

      // Pairing shows the code, the countdown and both node commands.
      fireEvent.click(screen.getByRole("button", { name: "Get pairing code" }));
      expect(await screen.findByText("K7Q-4MZ")).toBeTruthy();
      expect(screen.getByText(/Expires in/)).toBeTruthy();
      expect(screen.getByText("mnemo2-node pair --hub 192.168.1.20:47615 --code K7Q-4MZ")).toBeTruthy();
      expect(
        screen.getByText("mnemo2-node run --workspace <an empty folder for Mnemo's work>"),
      ).toBeTruthy();
    } finally {
      unmount();
      container.remove();
    }
  });

  it("worker-hosts' fake transport pairs, links, unlinks and revokes in memory", async () => {
    const transport = previewWorkerHostsTransport();
    expect((await transport.list()).hosts.map((host) => host.name)).toEqual(["studio", "old-laptop"]);

    // Linking is the user's standing approval; unlinking withdraws it.
    await transport.link("old-laptop", "laptop-ssh");
    expect((await transport.list()).hosts.find((host) => host.name === "old-laptop")?.ssh).toBe("laptop-ssh");
    await transport.unlink("old-laptop");
    expect((await transport.list()).hosts.find((host) => host.name === "old-laptop")?.ssh).toBeNull();

    // A revoke drops the host from the list the page re-reads.
    await transport.revoke("studio");
    expect((await transport.list()).hosts.map((host) => host.name)).toEqual(["old-laptop"]);

    // The offer keeps the lead tool's format, including an optional host name.
    const offer = await transport.pair();
    expect(offer).toMatchObject({ code: "K7Q-4MZ", expires_in_s: 600, hub: { listening: true, address: "192.168.1.20:47615" } });
    expect(offer.commands).toEqual([
      "mnemo2-node pair --hub 192.168.1.20:47615 --code K7Q-4MZ",
      "mnemo2-node run --workspace <an empty folder for Mnemo's work>",
    ]);
    const named = await transport.pair(" new-mac ");
    expect(named.commands[0]).toBe("mnemo2-node pair --hub 192.168.1.20:47615 --code K7Q-4MZ --name new-mac");
  });
});
