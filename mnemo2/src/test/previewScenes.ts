/**
 * Sample data for the dev-only preview page (`preview.html?scene=<name>`).
 *
 * This is test-side code: it builds on `fixtures.ts` (which the production
 * entry never imports) and is imported only by `src/preview/main.ts` and its
 * test. `index.html` never reaches it, so `vite build` bundles none of it and
 * `npm run check:no-mock` keeps passing.
 */

import { mount as svelteMount, unmount } from "svelte";
import App from "../App.svelte";
import SettingsDialog from "../components/SettingsDialog.svelte";
import { seedBoardPreview } from "../lib/boardEngine";
import { seedBacklogPreview, type BacklogItem, type BacklogTransport } from "../lib/backlog";
import type { BoardPlan } from "../lib/boardState";
import { app } from "../lib/store";
import { seedGraphPreview, type GraphEdge, type GraphImpactSymbol, type GraphNode, type GraphStatus, type GraphSymbol, type GraphTransport } from "../lib/graph";
import type { WorkerDeletionPreview } from "../lib/teamState";
import { seedPreviewSettings } from "./previewSettings";
import { seedPreviewLaya } from "./previewLaya";
import { seedPreviewSafety } from "./previewSafety";
import { seedPreviewMcp } from "./previewMcp";
import { seedPreviewAppearance } from "./previewAppearance";
import type { ThemeChoice } from "../lib/theme";
import type { AppState, LeadEntry, Worker, WorkerActivity, WorkerThreadEntry } from "../lib/types";

import { createLeadEntries, createWorkers, fixtureState } from "./fixtures";
import { seedFilesPreview, type FileEntry } from "../lib/files";
import {
  seedTracePagePreview,
  type TraceAttribution,
  type TraceDetail,
  type TraceListPage,
  type TraceListQuery,
  type TracePageTransport,
  type TraceRow,
  type TraceSettings,
} from "../lib/trace";
import { seedGitPreview, type GitOverview } from "../lib/git";
import { seedCostPreview, type CostInsights } from "../lib/cost";
import {
  LIST_PAGE_SIZE,
  seedMemoryPreview,
  type MemoryHit,
  type MemoryRow,
  type MemoryTransport,
} from "../lib/memoryView";

/** The fixture worker whose thread the preview shows in full. */
const RICH_WORKER_ID = "kai";

/** One row of a worker's thread: a Markdown message or a tool activity. */
type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

/** The clock the scene is built against, so relative stamps read "just now". */
const NOW_MS = Date.now();

/**
 * One worker's live activity at the scene's clock: `agoMs` is how long the
 * phase (and its turn) has been running. A dispatching worker has no turn
 * anchor yet — its turn timer starts when the turn's first event streams.
 */
function liveActivity(phase: WorkerActivity["phase"], agoMs: number, toolName: string | null = null): WorkerActivity {
  return {
    phase,
    toolName,
    turnStartedAtMs: phase === "starting" ? null : NOW_MS - agoMs,
    phaseSinceMs: NOW_MS - agoMs,
  };
}

/** The open plan on the sample board (and the plan id a sample lead entry names). */
const PREVIEW_PLAN: BoardPlan = {
  id: "plan-preview-thread",
  title: "Render the worker thread as Markdown",
  goal: "Every worker message renders like the lead chat; tool rows keep their own cards.",
  kind: "implementation",
  status: "open",
  closed_at_ms: null,
  report_path: null,
  steps: [
    {
      number: 1,
      header: "Thread rendering",
      text: "Route every message body through the same Markdown pipeline as the lead chat.",
      done: true,
      workers: [{ id: RICH_WORKER_ID, name: "Kai", status: "working" }],
    },
    {
      number: 2,
      header: null,
      text: "Keep tool rows in their own cards, one per call.",
      done: false,
      workers: [],
    },
    {
      number: 3,
      header: null,
      text: "Verify with `npm run check` and the component tests.",
      done: false,
      workers: [],
    },
  ],
};

/** A finished read: the worker opened the thread component before editing. */
const READ_TOOL_ROW: ToolEntry = {
  id: "preview-tool-read",
  kind: "tool",
  time: "09:48",
  toolCallId: "call-preview-read",
  toolName: "read_files",
  arguments: JSON.stringify({ files: ["mnemo2/src/components/WorkerThread.svelte"] }),
  liveOutput: "",
  result: {
    success: true,
    output: [
      "mnemo2/src/components/WorkerThread.svelte (151 lines)",
      "  1  <script lang=\"ts\">",
      "  2    import { Send } from \"@lucide/svelte\";",
      "  3    import { onMount } from \"svelte\";",
    ].join("\n"),
  },
};

/** A failed check: the first pass still rendered raw text. */
const FAILED_TOOL_ROW: ToolEntry = {
  id: "preview-tool-check-failed",
  kind: "tool",
  time: "09:53",
  toolCallId: "call-preview-check-failed",
  toolName: "shell",
  arguments: JSON.stringify({ command: "npm run check --workspace mnemo2" }),
  liveOutput: "",
  result: {
    success: false,
    output: [
      "mnemo2/src/components/WorkerThread.svelte:42:7",
      "Error: 'entry.body' does not exist on type 'WorkerMessage | ToolEntry'.",
      "  svelte-check found 1 error and 0 warnings in 1 file",
    ].join("\n"),
  },
};

/** A running check: the fixed pass, still going while the thread is read. */
const RUNNING_TOOL_ROW: ToolEntry = {
  id: "preview-tool-check-running",
  kind: "tool",
  time: "09:57",
  toolCallId: "call-preview-check-running",
  toolName: "shell",
  arguments: JSON.stringify({ command: "npx vitest run src/components/WorkerThread.test.ts" }),
  liveOutput: [
    " RUN  v2.1.9 C:/Mnemo/.worktrees/agent-previewP/mnemo2",
    "",
    " ❯ src/components/WorkerThread.test.ts (4 tests | 1 failed)",
    "   × keeps tool rows in their own cards",
  ].join("\n"),
  startedAtMs: NOW_MS - 96_000,
  result: null,
};

/** The thread the preview renders: Markdown prose and tool rows interleaved. */
const RICH_THREAD: WorkerThreadEntry[] = [
  {
    id: "preview-lead-inline-code",
    from: "lead",
    time: "09:44",
    body: "Kai — the thread still shows raw text where the lead chat shows rich Markdown. Route every `message` entry through `Markdown.svelte` and keep `ToolActivity` for `kind: \"tool\"` entries.",
  },
  READ_TOOL_ROW,
  {
    id: "preview-lead-plan",
    from: "lead",
    time: "09:50",
    body: [
      "> ## Step 2 · Thread rendering",
      "> ### What changes",
      "> - Render every message body through `Markdown`",
      "> - Keep one card per tool call in `ToolActivity`",
      "> - Never collapse the newest message",
      ">",
      "> ```svelte",
      "> {#each worker.messages as entry (entry.id)}",
      ">   {#if entry.kind === \"tool\"}<ToolActivity {entry} />{:else}<Markdown source={entry.body} />{/if}",
      "> {/each}",
      "> ```",
    ].join("\n"),
  },
  FAILED_TOOL_ROW,
  {
    id: "preview-worker-reply",
    from: "worker",
    time: "09:55",
    body: [
      "Fixed — every message body now renders through the same component as the lead chat.",
      "",
      "- Tool rows keep their own cards, one per call",
      "- The newest message is never collapsed",
      "",
      "`npm run check` is green again; the component test re-run below is still going.",
    ].join("\n"),
  },
  RUNNING_TOOL_ROW,
];

/** One durable inbox entry, so the received-messages ledger has content. */
const RICH_INBOX = [
  {
    sender: "lead" as const,
    kind: "steer" as const,
    text: "Keep the tool rows unchanged — only the message bodies need the Markdown pass.",
    atMs: NOW_MS - 6 * 60_000,
  },
];

/** The deletion preview: a worker whose plan step already landed through Ren. */
const SUPERSEDED_PREVIEW: WorkerDeletionPreview = {
  worker_id: "noa",
  name: "Noa",
  branch: "wt/agent-noa",
  live: false,
  checkout: true,
  uncommitted: false,
  unmerged_commits: 2,
  shared_branch: false,
  blocked: null,
  superseded_by: "Ren",
};

/** One scene of the preview page. */
export interface PreviewScene {
  /** The `?scene=` value that selects this scene. */
  name: string;
  /** Short heading for the index page. */
  title: string;
  /** One sentence for the index page: what the scene renders. */
  description: string;
  /** Build the store state this scene renders. */
  build(): AppState;
  /**
   * Mount a bespoke frame instead of the app shell — for dev-only scenes
   * that render something the store cannot express (the settings dialog
   * opens from component-local state, not the store). `build` still
   * applies; the scenes test uses it. The returned cleanup is for tests
   * that mount and unmount within a run; the preview page ignores it.
   */
  mount?: (target: HTMLElement) => () => void;
  /**
   * The palette the scene paints before it mounts (default: the mirrored
   * `[ui] theme`). The dark variants of the main screens set `"dark"` so the
   * same scene can be reviewed in both themes; `?theme=` overrides it.
   */
  theme?: ThemeChoice;
}

/**
 * The fixture worker carrying the rich thread. `overrides` changes only what a
 * scene needs (a needs-you status, a different last activity), never the
 * fixture's own values, which many tests depend on.
 */
function richThreadWorker(base: Worker, overrides: Partial<Worker> = {}): Worker {
  return {
    ...base,
    status: "working",
    needsYou: false,
    task: "Render the worker thread with the same Markdown pipeline as the lead chat — headings, quoted plans, lists, fenced code — and keep every tool call in its own card.",
    summary: "Messages render as Markdown now; the command check re-run is still going.",
    planId: PREVIEW_PLAN.id,
    planStep: 0,
    lastActivity: "2m ago",
    messages: RICH_THREAD,
    inbox: RICH_INBOX,
    ...overrides,
  };
}

/** The fixture workers, with the rich thread on one of them. */
function previewWorkers(overrides: Partial<Worker> = {}): Worker[] {
  return createWorkers().map((worker) =>
    worker.id === RICH_WORKER_ID ? richThreadWorker(worker, overrides) : worker,
  );
}

/** An already-opened project state: fixtures everywhere the scoped change does not reach. */
function baseState(workerOverrides: Partial<Worker> = {}): AppState {
  const workers = previewWorkers(workerOverrides);
  return {
    ...fixtureState(),
    workers,
    leadEntries: createLeadEntries(workers),
    board: [PREVIEW_PLAN],
  };
}

/** Mission control: the rich-thread worker selected, one running worker per live phase. */
function missionControl(): AppState {
  const state = baseState();
  const minutes = (value: number) => value * 60_000;
  // The rows' live statuses: June is dispatching, Sol thinks, Milo writes,
  // Kai runs its re-check shell (the thread's last tool row), Noa waits on you.
  const live: Record<string, Partial<Worker> & Pick<Worker, "activity" | "createdAtMs">> = {
    june: { createdAtMs: NOW_MS - minutes(2), activity: liveActivity("starting", minutes(2)) },
    sol: { createdAtMs: NOW_MS - minutes(21), activity: liveActivity("thinking", minutes(1.5)) },
    milo: { createdAtMs: NOW_MS - minutes(24), activity: liveActivity("writing", 45_000) },
    kai: { createdAtMs: NOW_MS - minutes(18), activity: liveActivity("tool", 72_000, "shell") },
    noa: {
      status: "needs_you",
      needsYou: true,
      createdAtMs: NOW_MS - minutes(14),
      activity: liveActivity("waiting", minutes(5)),
    },
  };
  return {
    ...state,
    route: "team",
    selectedWorkerId: RICH_WORKER_ID,
    teamFilter: "all",
    teamActive: 4,
    teamSlots: 8,
    workerPanelId: null,
    workerDeletion: null,
    workers: state.workers.map((worker) =>
      worker.id in live ? { ...worker, ...live[worker.id] } : worker,
    ),
  };
}

/** One symbol of the sample code graph; `degree` is its global link count. */
interface SampleSymbol extends GraphSymbol {
  degree: number;
}

/** A plausible slice of this project's own code graph for the Graph scene. */
const SAMPLE_SYMBOLS: SampleSymbol[] = [
  { id: "src/agent/turn.rs::turn::1", name: "turn", kind: "module", file: "src/agent/turn.rs", line: 1, degree: 3 },
  { id: "src/agent/turn.rs::run_turn::812", name: "run_turn", kind: "function", file: "src/agent/turn.rs", line: 812, degree: 14 },
  { id: "src/agent/turn.rs::record_request_stats::3908", name: "record_request_stats", kind: "function", file: "src/agent/turn.rs", line: 3908, degree: 5 },
  { id: "src/agent/mod.rs::start_agent::96", name: "start_agent", kind: "function", file: "src/agent/mod.rs", line: 96, degree: 9 },
  { id: "src/headless/engine.rs::engine::1", name: "engine", kind: "module", file: "src/headless/engine.rs", line: 1, degree: 4 },
  { id: "src/headless/engine.rs::Engine::stats::680", name: "Engine::stats", kind: "method", file: "src/headless/engine.rs", line: 680, degree: 11 },
  { id: "src/headless/stats.rs::EngineStats::188", name: "EngineStats", kind: "struct", file: "src/headless/stats.rs", line: 188, degree: 8 },
  { id: "src/headless/stats.rs::ProjectStats::844", name: "ProjectStats", kind: "struct", file: "src/headless/stats.rs", line: 844, degree: 7 },
  { id: "src/memory/types.rs::SavingsStats::682", name: "SavingsStats", kind: "struct", file: "src/memory/types.rs", line: 682, degree: 6 },
  { id: "src/memory/mod.rs::MemoryStore::savings_stats::2457", name: "MemoryStore::savings_stats", kind: "method", file: "src/memory/mod.rs", line: 2457, degree: 6 },
  { id: "src/codegraph/query.rs::GraphView::slice::382", name: "GraphView::slice", kind: "method", file: "src/codegraph/query.rs", line: 382, degree: 7 },
  { id: "src/codegraph/query.rs::GraphSliceFilter::117", name: "GraphSliceFilter", kind: "struct", file: "src/codegraph/query.rs", line: 117, degree: 3 },
  { id: "src/codegraph/extract.rs::extract::1", name: "extract", kind: "module", file: "src/codegraph/extract.rs", line: 1, degree: 4 },
  { id: "src/codegraph/extract.rs::SymbolKind::18", name: "SymbolKind", kind: "enum", file: "src/codegraph/extract.rs", line: 18, degree: 9 },
  { id: "src/codegraph/extract.rs::EdgeKind::31", name: "EdgeKind", kind: "enum", file: "src/codegraph/extract.rs", line: 31, degree: 8 },
];

/** The sample graph's directed edges. */
const SAMPLE_EDGES: GraphEdge[] = [
  { from: "src/agent/mod.rs::start_agent::96", to: "src/agent/turn.rs::run_turn::812", kind: "calls" },
  { from: "src/agent/turn.rs::turn::1", to: "src/agent/turn.rs::run_turn::812", kind: "contains" },
  { from: "src/agent/turn.rs::turn::1", to: "src/agent/turn.rs::record_request_stats::3908", kind: "contains" },
  { from: "src/agent/turn.rs::run_turn::812", to: "src/agent/turn.rs::record_request_stats::3908", kind: "calls" },
  { from: "src/agent/turn.rs::run_turn::812", to: "src/headless/engine.rs::Engine::stats::680", kind: "calls" },
  { from: "src/agent/turn.rs::run_turn::812", to: "src/headless/stats.rs::ProjectStats::844", kind: "imports" },
  { from: "src/headless/engine.rs::engine::1", to: "src/headless/engine.rs::Engine::stats::680", kind: "contains" },
  { from: "src/headless/engine.rs::Engine::stats::680", to: "src/memory/mod.rs::MemoryStore::savings_stats::2457", kind: "calls" },
  { from: "src/headless/engine.rs::Engine::stats::680", to: "src/memory/types.rs::SavingsStats::682", kind: "imports" },
  { from: "src/headless/stats.rs::EngineStats::188", to: "src/headless/stats.rs::ProjectStats::844", kind: "contains" },
  { from: "src/headless/stats.rs::EngineStats::188", to: "src/memory/types.rs::SavingsStats::682", kind: "contains" },
  { from: "src/memory/mod.rs::MemoryStore::savings_stats::2457", to: "src/memory/types.rs::SavingsStats::682", kind: "imports" },
  { from: "src/codegraph/query.rs::GraphView::slice::382", to: "src/codegraph/query.rs::GraphSliceFilter::117", kind: "imports" },
  { from: "src/codegraph/query.rs::GraphView::slice::382", to: "src/codegraph/extract.rs::EdgeKind::31", kind: "imports" },
  { from: "src/codegraph/extract.rs::extract::1", to: "src/codegraph/extract.rs::SymbolKind::18", kind: "contains" },
  { from: "src/codegraph/extract.rs::extract::1", to: "src/codegraph/extract.rs::EdgeKind::31", kind: "contains" },
];

/**
 * The sample repository's directory modules (overview level 2) with their
 * symbol counts — a plausible spread over this project's packages and the
 * neighbours a monorepo of this shape carries, so the Graph scene opens on a
 * whole-graph map instead of an empty canvas.
 */
const SAMPLE_MODULES = [
  ["src/agent", 142],
  ["src/app", 38],
  ["src/browser", 61],
  ["src/codegraph", 168],
  ["src/config", 44],
  ["src/headless", 96],
  ["src/host", 72],
  ["src/mcp", 88],
  ["src/memory", 154],
  ["src/project", 66],
  ["src/provider", 94],
  ["src/runtime", 31],
  ["src/safety_rules", 22],
  ["src/tool", 181],
  ["src/bin", 18],
  ["src/daemon", 27],
  ["src/export", 34],
  ["src/index", 57],
  ["src/plugin", 41],
  ["src/telemetry", 29],
  ["mnemo2/src", 212],
  ["mnemo2/plugin", 24],
  ["mnemo2/scripts", 9],
  ["mnemo2/claude-desktop-mod", 16],
  ["src-tauri/src", 74],
  ["src-tauri/tests", 21],
  ["frontend/src", 48],
  ["frontend/bench", 12],
  ["tests/integration", 57],
  ["tests/e2e", 33],
  ["examples", 19],
  ["docs", 12],
  ["scripts", 26],
  ["vendor", 14],
  ["assets", 7],
  ["benches", 15],
  ["tools", 23],
  [".coding/skills", 11],
  ["migrations", 8],
  ["ops", 13],
] as const;

/** Aggregated symbol edges between those modules: `[from, to, kind, count]`. */
/** Symbols across the sample repository's modules — the whole graph the
 * overview aggregates (the backend reads every symbol from the store). */
const SAMPLE_TOTAL_SYMBOLS = SAMPLE_MODULES.reduce((total, [, symbols]) => total + symbols, 0);

const SAMPLE_MODULE_EDGES = [
  ["src/agent", "src/headless", "calls", 41],
  ["src/agent", "src/provider", "calls", 33],
  ["src/agent", "src/memory", "imports", 18],
  ["src/agent", "src/tool", "calls", 27],
  ["src/app", "src/agent", "calls", 22],
  ["src/app", "src/config", "imports", 14],
  ["src/app", "src/runtime", "calls", 9],
  ["src/tool", "src/codegraph", "calls", 24],
  ["src/tool", "src/memory", "calls", 19],
  ["src/tool", "src/project", "imports", 11],
  ["src/mcp", "src/tool", "calls", 37],
  ["src/mcp", "src/codegraph", "calls", 21],
  ["src/mcp", "src/headless", "imports", 12],
  ["src/headless", "src/memory", "calls", 29],
  ["src/headless", "src/provider", "calls", 34],
  ["src/headless", "src/codegraph", "calls", 16],
  ["src/headless", "src/config", "imports", 10],
  ["src/memory", "src/project", "imports", 15],
  ["src/memory", "src/codegraph", "contains", 8],
  ["src/codegraph", "src/memory", "imports", 12],
  ["src/codegraph", "src/project", "imports", 9],
  ["src/provider", "src/config", "imports", 7],
  ["src/project", "src/config", "imports", 6],
  ["src/host", "src/mcp", "calls", 18],
  ["src/host", "src/agent", "calls", 14],
  ["src/browser", "src/tool", "calls", 11],
  ["src/browser", "src/host", "imports", 6],
  ["src/daemon", "src/host", "calls", 12],
  ["src/daemon", "src/runtime", "calls", 7],
  ["src/plugin", "src/tool", "calls", 9],
  ["src/plugin", "src/config", "imports", 5],
  ["src/index", "src/codegraph", "calls", 14],
  ["src/index", "src/memory", "calls", 10],
  ["src/export", "src/codegraph", "imports", 8],
  ["src/telemetry", "src/provider", "calls", 6],
  ["src/safety_rules", "src/tool", "contains", 4],
  ["src/bin", "src/app", "calls", 8],
  ["src-tauri/src", "src/agent", "calls", 23],
  ["src-tauri/src", "src/host", "calls", 17],
  ["src-tauri/src", "src/mcp", "calls", 21],
  ["src-tauri/src", "src/codegraph", "imports", 13],
  ["mnemo2/src", "src-tauri/src", "imports", 6],
  ["mnemo2/src", "src/mcp", "calls", 9],
  ["frontend/src", "mnemo2/src", "imports", 4],
  ["tests/integration", "src/headless", "calls", 11],
  ["tests/integration", "src/codegraph", "calls", 7],
  ["tests/e2e", "mnemo2/src", "calls", 5],
  ["scripts", "src/tool", "calls", 3],
  ["docs", "src/agent", "imports", 2],
  ["mnemo2/claude-desktop-mod", "mnemo2/src", "calls", 4],
  ["vendor", "src/codegraph", "contains", 3],
  ["benches", "src/codegraph", "calls", 6],
] as const;

/** The directory module a sample file belongs to, at overview level 2. */
function sampleModuleOf(file: string): string {
  const parts = file.split("/");
  return parts.length > 2 ? `${parts[0]}/${parts[1]}` : parts[0];
}


const SAMPLE_SYMBOL_BY_ID = new Map(SAMPLE_SYMBOLS.map((symbol) => [symbol.id, symbol]));

const toSymbol = ({ id, name, kind, file, line }: SampleSymbol): GraphSymbol => ({ id, name, kind, file, line });

/** A tiny in-memory answer service over the sample symbols, so the preview's search, focus and refocus all work.
 *  The graph scene opens mid-pass (see `graphScene`), so this starts out indexing. */
function sampleGraphTransport(): GraphTransport {
  let indexing = true;
  let pollsLeft = 0;

  const status = (): GraphStatus => ({
    available: true,
    indexing,
    files: 3945,
    symbols: 12840,
    edges: 34210,
    last_indexed_at: Math.floor(NOW_MS / 1000) - 7 * 60,
  });

  return {
    status: async () => {
      if (indexing && pollsLeft > 0) {
        pollsLeft -= 1;
        if (pollsLeft === 0) indexing = false;
      }
      return status();
    },
    refresh: async () => {
      indexing = true;
      pollsLeft = 2;
      return status();
    },
    search: async (query, kind, limit = 50) =>
      SAMPLE_SYMBOLS
        .filter((symbol) => symbol.name.toLowerCase().includes(query.toLowerCase()))
        .filter((symbol) => !kind || symbol.kind === kind)
        .slice(0, limit)
        .map(toSymbol),
    neighbourhood: async (symbolId, depth = 1) => {
      const center = SAMPLE_SYMBOL_BY_ID.get(symbolId);
      if (!center) throw new Error(`unknown symbol ${symbolId}`);
      const distance = new Map([[center.id, 0]]);
      const queue = [center.id];
      while (queue.length > 0) {
        const current = queue.shift() as string;
        const hops = distance.get(current) as number;
        if (hops >= depth) continue;
        for (const edge of SAMPLE_EDGES) {
          const next = edge.from === current ? edge.to : edge.to === current ? edge.from : null;
          if (next && !distance.has(next)) {
            distance.set(next, hops + 1);
            queue.push(next);
          }
        }
      }
      const ids = new Set(distance.keys());
      const nodes = SAMPLE_SYMBOLS.filter((symbol) => ids.has(symbol.id) && symbol.id !== center.id);
      return {
        center,
        nodes,
        edges: SAMPLE_EDGES.filter((edge) => ids.has(edge.from) && ids.has(edge.to)),
        truncated: false,
      };
    },
    impact: async (symbolId) => {
      const distance = new Map<string, number>();
      const queue = [symbolId];
      while (queue.length > 0) {
        const current = queue.shift() as string;
        const hops = current === symbolId ? 0 : (distance.get(current) as number);
        for (const edge of SAMPLE_EDGES.filter((entry) => entry.to === current)) {
          if (!distance.has(edge.from) && edge.from !== symbolId) {
            distance.set(edge.from, hops + 1);
            queue.push(edge.from);
          }
        }
      }
      const rows: GraphImpactSymbol[] = [...distance.entries()].map(([id, hops]) => ({
        ...toSymbol(SAMPLE_SYMBOL_BY_ID.get(id) as SampleSymbol),
        distance: hops,
      }));
      return rows.sort((a, b) => a.distance - b.distance || a.name.localeCompare(b.name));
    },
    top: async (limit = 20) => [...SAMPLE_SYMBOLS].sort((a, b) => b.degree - a.degree).slice(0, limit),
    overview: async (level = 2, limit = 60) => {
      if (level === 0) {
        const nodes = [...SAMPLE_SYMBOLS]
          .sort((a, b) => b.degree - a.degree || a.id.localeCompare(b.id))
          .slice(0, limit)
          .map((symbol) => ({
            id: symbol.id,
            label: symbol.name,
            kind: symbol.kind,
            file: symbol.file,
            line: symbol.line,
            symbols: 1,
            degree: symbol.degree,
            top: [],
          }));
        const drawn = new Set(nodes.map((node) => node.id));
        return {
          level,
          nodes,
          edges: SAMPLE_EDGES.filter((edge) => drawn.has(edge.from) && drawn.has(edge.to)).map(
            (edge) => ({ from: edge.from, to: edge.to, kind: edge.kind, count: 1 }),
          ),
          total_nodes: SAMPLE_SYMBOLS.length,
          truncated: nodes.length < SAMPLE_SYMBOLS.length,
          total_symbols: SAMPLE_TOTAL_SYMBOLS,
          sampled_symbols: SAMPLE_SYMBOLS.length,
        };
      }
      const weight = new Map<string, number>();
      for (const [from, to, , count] of SAMPLE_MODULE_EDGES) {
        weight.set(from, (weight.get(from) ?? 0) + count);
        weight.set(to, (weight.get(to) ?? 0) + count);
      }
      const modules = SAMPLE_MODULES.map(([id, symbols]) => ({
        id,
        label: id.slice(id.lastIndexOf("/") + 1),
        kind: null,
        file: id,
        line: 0,
        symbols,
        degree: weight.get(id) ?? 0,
        top: SAMPLE_SYMBOLS.filter((symbol) => sampleModuleOf(symbol.file) === id)
          .sort((a, b) => b.degree - a.degree || a.id.localeCompare(b.id))
          .slice(0, 12)
          .map((symbol) => ({ ...toSymbol(symbol), degree: symbol.degree })),
      }));
      modules.sort((a, b) => b.symbols - a.symbols || b.degree - a.degree || a.id.localeCompare(b.id));
      const nodes = modules.slice(0, limit);
      const drawn = new Set(nodes.map((node) => node.id));
      return {
        level,
        nodes,
        edges: SAMPLE_MODULE_EDGES.filter(([from, to]) => drawn.has(from) && drawn.has(to)).map(
          ([from, to, kind, count]) => ({ from, to, kind, count }),
        ),
        total_nodes: SAMPLE_MODULES.length,
        truncated: nodes.length < SAMPLE_MODULES.length,
        total_symbols: SAMPLE_TOTAL_SYMBOLS,
        sampled_symbols: SAMPLE_TOTAL_SYMBOLS,
      };
    },
    context: async (symbolId) => {
      const center = SAMPLE_SYMBOL_BY_ID.get(symbolId);
      if (!center) throw new Error(`unknown symbol ${symbolId}`);
      const callers = SAMPLE_EDGES.filter((edge) => edge.to === symbolId).map((edge) => SAMPLE_SYMBOL_BY_ID.get(edge.from) as SampleSymbol);
      const callees = SAMPLE_EDGES.filter((edge) => edge.from === symbolId).map((edge) => SAMPLE_SYMBOL_BY_ID.get(edge.to) as SampleSymbol);
      const uniqueBy = (symbols: SampleSymbol[]) => [...new Map(symbols.map((symbol) => [symbol.id, symbol])).values()];
      return { symbol: center, callers: uniqueBy(callers).map(toSymbol), callees: uniqueBy(callees).map(toSymbol), caller_total: uniqueBy(callers).length, callee_total: uniqueBy(callees).length };
    },
  };
}

/** The panel's Graph tab: the code-graph explorer seeded with a sample slice of this project. */
function graphScene(): AppState {
  seedGraphPreview(sampleGraphTransport());
  return {
    ...baseState(),
    route: "lead",
    selectedWorkerId: null,
    workerPanelId: null,
    workbenchOpen: true,
    workbenchTab: "graph",
    workerDeletion: null,
    // The scene opens on a rebuild in progress: the notice shows 1,200/2,400 files.
    indexing: {
      revision: 1,
      firstRun: false,
      code: { state: "running", done: 1200, total: 2400, detail: null },
      memory: null,
    },
  };
}

/** The tree the Files tab preview opens on, keyed by the listed directory. */
const PREVIEW_FILES: Record<string, FileEntry[]> = {
  "": [
    { path: ".coding", name: ".coding", kind: "dir" },
    { path: "mnemo2", name: "mnemo2", kind: "dir" },
    { path: "src-tauri", name: "src-tauri", kind: "dir" },
    { path: "src", name: "src", kind: "dir" },
    { path: "agent.md", name: "agent.md", kind: "file" },
    { path: "Cargo.toml", name: "Cargo.toml", kind: "file" },
    { path: "README.md", name: "README.md", kind: "file" },
  ],
  "src-tauri": [
    { path: "src-tauri/src", name: "src", kind: "dir" },
    { path: "src-tauri/Cargo.toml", name: "Cargo.toml", kind: "file" },
    { path: "src-tauri/tauri.conf.json", name: "tauri.conf.json", kind: "file" },
  ],
  "src-tauri/src": [
    { path: "src-tauri/src/mnemo2", name: "mnemo2", kind: "dir" },
  ],
  "src-tauri/src/mnemo2": [
    { path: "src-tauri/src/mnemo2/files_view.rs", name: "files_view.rs", kind: "file" },
    { path: "src-tauri/src/mnemo2/mod.rs", name: "mod.rs", kind: "file" },
  ],
  mnemo2: [
    { path: "mnemo2/src", name: "src", kind: "dir" },
    { path: "mnemo2/package.json", name: "package.json", kind: "file" },
  ],
  "mnemo2/src": [
    { path: "mnemo2/src/components", name: "components", kind: "dir" },
    { path: "mnemo2/src/lib", name: "lib", kind: "dir" },
    { path: "mnemo2/src/App.svelte", name: "App.svelte", kind: "file" },
  ],
  "mnemo2/src/lib": [{ path: "mnemo2/src/lib/files.ts", name: "files.ts", kind: "file" }],
};

/** The file the preview's viewer shows: a slice of the module it documents. */
const PREVIEW_FILE_TEXT = [
  "//! The Files tab: a lazy, Git-aware project tree and a read-only viewer.",
  "",
  "/// Resolve a project-relative path to an absolute one inside the project.",
  "fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {",
  "    for component in Path::new(relative).components() {",
  "        match component {",
  "            Component::Normal(_) | Component::CurDir => {}",
  "            Component::ParentDir => {",
  "                return Err(format!(\"'{relative}' is outside the project\"));",
  "            }",
  "            Component::RootDir | Component::Prefix(_) => {",
  "                return Err(\"only project-relative paths are allowed\".to_string());",
  "            }",
  "        }",
  "    }",
  "    let root = dunce::canonicalize(root)?;",
  "    let canonical = dunce::canonicalize(root.join(relative))?;",
  "    if !canonical.starts_with(&root) {",
  "        return Err(format!(\"'{relative}' is outside the project\"));",
  "    }",
  "    Ok(canonical)",
  "}",
].join("\n");

/** A short TypeScript sample, so the preview's viewer also shows a second grammar. */
const PREVIEW_TS_TEXT = [
  "/** The tree asks the backend for one directory per expand. */",
  "export function visibleRows(",
  "  listings: Record<string, FileEntry[]>,",
  "  expanded: Record<string, boolean>,",
  "): TreeRow[] {",
  "  const rows: TreeRow[] = [];",
  "  walk(\"\", 0, rows, listings, expanded);",
  "  return rows;",
  "}",
].join("\n");

/** The Files tab: the project tree, and one file opened read-only beside it. */
function filesTab(): AppState {
  seedFilesPreview({
    list: async (path) => {
      const entries = PREVIEW_FILES[path];
      if (entries === undefined) throw new Error(`no preview listing for '${path}'`);
      return { path, entries, truncated: false };
    },
    read: async (path) => {
      const text = path.endsWith(".rs")
        ? PREVIEW_FILE_TEXT
        : path.endsWith(".ts")
          ? PREVIEW_TS_TEXT
          : "";
      return text
        ? { path, text, bytes: text.length, truncated: false, binary: false }
        : { path, text: "", bytes: 0, truncated: false, binary: true };
    },
    find: async (query) =>
      Object.values(PREVIEW_FILES)
        .flat()
        .filter((entry) => entry.path.toLowerCase().includes(query.toLowerCase())),
  });
  return { ...baseState(), workbenchOpen: true, workbenchTab: "files", workbenchWidth: 460 };
}

/** A request row as the trace log's list returns it, with the fields the tab reads. */
function previewTraceRow(
  fields: Partial<TraceRow> & { id: number; ts_ms: number; model: string },
): TraceRow {
  return {
    base_url: "https://api.example.invalid/v1",
    provider: "example",
    http_status: null,
    usage: null,
    finish_reason: null,
    is_complete: true,
    version: 2,
    ttft_ms: null,
    generation_ms: null,
    reasoning_ms: null,
    connect_ms: null,
    upload_ms: null,
    server_ms: null,
    tools_ms: null,
    prep_ms: null,
    compact_ms: null,
    backoff_ms: null,
    stall_ms: null,
    chunks: null,
    error: null,
    cancelled: false,
    response_truncated: false,
    response_evicted: false,
    guard_cut: null,
    run_id: "preview-run",
    attribution: null,
    cost_micros: null,
    priced: false,
    retained: false,
    ...fields,
  };
}

/** A trace attribution with the preview session's defaults. */
function previewTraceAttribution(
  fields: Pick<TraceAttribution, "actor_kind" | "agent_id" | "agent_name"> & Partial<TraceAttribution>,
): TraceAttribution {
  return {
    session_id: "session-preview",
    worker_id: null,
    plan_id: null,
    plan_step: null,
    turn_id: null,
    purpose: "turn",
    ...fields,
  };
}

/** The implementation worker the preview's running calls belong to. */
function previewTraceWorkerAttribution(): TraceAttribution {
  return previewTraceAttribution({
    actor_kind: "worker",
    agent_id: "agent-kai",
    agent_name: "Kai",
    worker_id: RICH_WORKER_ID,
    plan_id: "c3ba2b58",
    plan_step: 5,
    purpose: "turn",
  });
}

/** The tool declarations every preview request carries. */
const PREVIEW_TRACE_TOOLS = [
  {
    type: "function",
    function: {
      name: "read_file",
      description: "Read a file inside the worktree.",
      parameters: {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
        additionalProperties: false,
      },
    },
  },
  {
    type: "function",
    function: {
      name: "apply_patch",
      description: "Apply a unified diff inside the worktree.",
      parameters: {
        type: "object",
        properties: { patch: { type: "string" } },
        required: ["patch"],
        additionalProperties: false,
      },
    },
  },
];

/** The refusal the trace exists to explain: a strict schema the endpoint rejected. */
const PREVIEW_TRACE_STRICT_REQUEST = {
  model: "worker-model",
  stream: true,
  temperature: 0,
  messages: [
    {
      role: "system",
      content:
        "You are Mnemo2's implementation worker. Work only inside your worktree, run the checks the lead names, and commit on your branch.",
    },
    {
      role: "user",
      content:
        "The worker-thread component test still fails on the tool rows. Fix it, run the checks and commit.",
    },
  ],
  tools: PREVIEW_TRACE_TOOLS,
  response_format: {
    type: "json_schema",
    json_schema: {
      name: "turn",
      strict: true,
      schema: {
        type: "object",
        properties: { summary: { type: "string" } },
        required: ["summary"],
      },
    },
  },
};

const PREVIEW_TRACE_STRICT_RESPONSE = JSON.stringify(
  {
    error: {
      message:
        "Invalid schema for response_format 'turn': In context=(), 'additionalProperties' is required to be supplied and to be false.",
      type: "invalid_request_error",
      param: "response_format",
      code: null,
    },
  },
  null,
  2,
);

/** The rows, oldest first, exactly as the log's ring returns them. */
const PREVIEW_TRACE_ROWS: TraceRow[] = [
  previewTraceRow({
    id: 101,
    ts_ms: NOW_MS - 5 * 60_000,
    model: "lead-model",
    provider: "primary",
    base_url: "https://api.example.invalid/v1",
    http_status: 200,
    usage: { prompt: 18_420, completion: 640, reasoning: 0, cached: 12_288 },
    finish_reason: "tool_calls",
    ttft_ms: 640,
    generation_ms: 3_400,
    attribution: previewTraceAttribution({
      actor_kind: "lead",
      agent_id: "agent-lead",
      agent_name: "Lead",
      turn_id: "turn-4f2a",
    }),
    cost_micros: 84_000,
    priced: true,
  }),
  previewTraceRow({
    id: 102,
    ts_ms: NOW_MS - 3 * 60_000,
    model: "worker-model",
    provider: "primary",
    base_url: "https://api.example.invalid/v1",
    http_status: 200,
    usage: { prompt: 9_120, completion: 220, reasoning: 0, cached: 8_192 },
    finish_reason: "stop",
    ttft_ms: 410,
    generation_ms: 1_800,
    attribution: previewTraceWorkerAttribution(),
    cost_micros: 11_200,
    priced: true,
  }),
  previewTraceRow({
    id: 103,
    ts_ms: NOW_MS - 90_000,
    model: "worker-model",
    provider: "primary",
    base_url: "https://api.example.invalid/v1",
    http_status: 400,
    usage: null,
    finish_reason: null,
    ttft_ms: 120,
    generation_ms: 120,
    error:
      "Invalid schema for response_format 'turn': In context=(), 'additionalProperties' is required to be supplied and to be false.",
    attribution: previewTraceWorkerAttribution(),
    cost_micros: null,
    priced: true,
  }),
  previewTraceRow({
    id: 104,
    ts_ms: NOW_MS - 8_000,
    model: "lead-model",
    provider: "primary",
    base_url: "https://api.example.invalid/v1",
    usage: { prompt: 12_040, completion: 120, reasoning: 0, cached: 8_192 },
    is_complete: false,
    version: 3,
    ttft_ms: 520,
    generation_ms: 1_200,
    attribution: previewTraceAttribution({
      actor_kind: "lead",
      agent_id: "agent-lead",
      agent_name: "Lead",
      purpose: "turn",
      turn_id: "turn-91bc",
    }),
    cost_micros: 52_000,
    priced: true,
  }),
  // Retained metadata from an earlier session: the model has no configured
  // price and the bodies are gone, so the row shows a dash and the archive mark.
  previewTraceRow({
    id: 105,
    ts_ms: NOW_MS - 26 * 60_000,
    model: "legacy-model",
    provider: "secondary",
    base_url: "https://secondary.example.invalid/v1",
    http_status: 200,
    usage: { prompt: 4_200, completion: 180, reasoning: 0, cached: 0 },
    finish_reason: "stop",
    ttft_ms: 300,
    generation_ms: 900,
    attribution: previewTraceAttribution({
      actor_kind: "worker",
      agent_id: "agent-noa",
      agent_name: "Noa",
      worker_id: "noa",
      plan_id: "c3ba2b58",
      plan_step: 2,
      purpose: "turn",
    }),
    cost_micros: null,
    priced: false,
    retained: true,
  }),
];

/** One row in full: the exact body POSTed and the raw response. */
const PREVIEW_TRACE_DETAILS: Record<number, TraceDetail> = Object.fromEntries(
  PREVIEW_TRACE_ROWS.map((row) => {
    const refused = row.id === 103;
    const streaming = row.id === 104;
    const request = refused
      ? PREVIEW_TRACE_STRICT_REQUEST
      : {
          model: row.model,
          stream: true,
          temperature: 0,
          messages: [
            {
              role: "system",
              content:
                "You are Mnemo2's lead. Dispatch workers, review their branches, and keep the plan's steps honest.",
            },
            {
              role: "user",
              content: "Two workers are running. Keep the trace clean and report the strict-mode refusal.",
            },
          ],
          tools: PREVIEW_TRACE_TOOLS,
        };
    return [
      row.id,
      {
        ...row,
        request_json: request,
        response_raw: refused
          ? PREVIEW_TRACE_STRICT_RESPONSE
          : streaming
            ? 'data: {"choices":[{"delta":{"content":"The refusal is in the row"}}]}\n\ndata: {"choices":[{"delta":{"content":" above."}}]}\n'
            : 'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n',
        request_truncated: false,
        raw_tool_calls: null,
      } satisfies TraceDetail,
    ];
  }),
);

/** The page the preview's trace transport answers with, newest first. */
function previewTraceListPage(query: TraceListQuery, rows: TraceRow[]): TraceListPage {
  const matching = rows
    .filter((row) => {
      if (query.agent === undefined) return true;
      if (query.agent === "unattributed") return row.attribution === null;
      if (query.agent === "lead") return row.attribution?.actor_kind === "lead";
      return row.attribution?.worker_id === query.agent || row.attribution?.agent_id === query.agent;
    })
    .sort((left, right) => right.ts_ms - left.ts_ms);
  const limit = Math.min(query.limit ?? matching.length, matching.length);
  return {
    rows: matching.slice(0, limit),
    total: matching.length,
    unpriced: matching.filter((row) => !row.priced).length,
    has_more: matching.length > limit,
    next_cursor: null,
    capped: false,
  };
}

/**
 * The Trace view model's transport over the preview rows. Every data control
 * answers, so the export, settings and clear/delete states are previewable.
 */
function previewTraceTransport(rows: TraceRow[]): TracePageTransport {
  const settings = (logBodies: boolean): TraceSettings => ({
    log_bodies: logBodies,
    ring_capacity: 128,
    journal_bytes: 18_432,
    history_bytes: 96_256,
  });
  let logBodies = false;
  return {
    list: async (query) => previewTraceListPage(query, rows),
    detail: async (_runId, id) => {
      const row = rows.find((entry) => entry.id === id);
      // A retained row kept its metadata only; its bodies are gone.
      if (row === undefined || row.retained) return null;
      return PREVIEW_TRACE_DETAILS[id] ?? null;
    },
    settings: async () => settings(logBodies),
    setLogBodies: async (on) => {
      logBodies = on;
      return settings(logBodies);
    },
    clearLive: async () => undefined,
    deleteHistory: async () => ({ deleted_files: 2, bytes: 96_256 }),
    exportCalls: async (request) => ({
      path: ".coding/logs/trace-exports/trace-20261102-140000.jsonl",
      count: request.keys?.length ?? rows.length,
      includes_bodies: logBodies,
    }),
    revealExport: async () => undefined,
  };
}

/** The panel's Trace tab: streaming, failed, unpriced and retained calls in one list. */
function traceScene(): AppState {
  seedTracePagePreview(previewTraceTransport(PREVIEW_TRACE_ROWS));
  return {
    ...baseState(),
    route: "lead",
    selectedWorkerId: RICH_WORKER_ID,
    workerPanelId: null,
    workbenchOpen: true,
    workbenchTab: "trace",
  };
}

/** The panel's Trace tab before any model call was recorded. */
function traceEmpty(): AppState {
  seedTracePagePreview(previewTraceTransport([]));
  return {
    ...baseState(),
    route: "lead",
    selectedWorkerId: RICH_WORKER_ID,
    workerPanelId: null,
    workbenchOpen: true,
    workbenchTab: "trace",
  };
}

/** The panel beside Mission Control: the diff of the worker under review. */
function panelTeamDiff(): AppState {
  return { ...missionControl(), workbenchOpen: true, workbenchTab: "diff", workbenchWidth: 460 };
}

/** The panel's Graph tab beside Mission Control. */
function panelTeamGraph(): AppState {
  seedGraphPreview(sampleGraphTransport());
  return { ...missionControl(), workbenchOpen: true, workbenchTab: "graph" };
}

/** The panel's Trace tab beside the Cost tab. */
function panelCostTrace(): AppState {
  seedTracePagePreview(previewTraceTransport(PREVIEW_TRACE_ROWS));
  return { ...costTab(), workerPanelId: null, workbenchOpen: true, workbenchTab: "trace" };
}

/** The panel's Memory tab beside the Plans tab. */
function panelPlansMemory(): AppState {
  seedMemoryPreview(sampleMemoryTransport());
  return { ...plansTab(), workerPanelId: null, workbenchOpen: true, workbenchTab: "memory" };
}

/** The Git tab: branches at different distances from main, and the newest commits on one lane. */
function gitTab(): AppState {
  const overview: GitOverview = {
    branch: "wt/agent-gitTab95",
    main: "1f1621c",
    main_worktree: null,
    main_dirty: false,
    branches: [
      { name: "wt/agent-gitTab95", current: true, ahead: 3, behind: 0 },
      { name: "main", current: false, ahead: 0, behind: 3 },
      { name: "wt/dark-theme", current: false, ahead: 2, behind: 1 },
      { name: "wt/files-tab", current: false, ahead: 0, behind: 2 },
    ],
    commits: [
      {
        id: "9e2b7a1c4d5e6f708192a3b4c5d6e7f8091a2b3c",
        short: "9e2b7a1",
        summary: "mnemo2: the Git tab reads branches, distances and the commit lane",
        author: "Carsten Hess",
        date: "4 minutes ago",
        refs: ["HEAD -> wt/agent-gitTab95"],
      },
      {
        id: "5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d",
        short: "5c4d3e2",
        summary: "mnemo2: the Trace tab reads the engine's shared request log",
        author: "Carsten Hess",
        date: "2 hours ago",
        refs: [],
      },
      {
        id: "1f1621c0ca075d9b8c17a4c9d2e3f40516273849",
        short: "1f1621c",
        summary: "mnemo2: a Files tab over a lazy, Git-aware project tree",
        author: "Carsten Hess",
        date: "yesterday",
        refs: ["main"],
      },
      {
        id: "404a8fb3a1b2c3d4e5f60718293a4b5c6d7e8f90",
        short: "404a8fb",
        summary: "team: merge filesTab90 (filesTab90)",
        author: "Mnemo2 Team",
        date: "yesterday",
        refs: [],
      },
    ],
    truncated: false,
  };
  seedGitPreview({
    overview: async () => overview,
    moveMain: async (branch) => ({ branch, main: overview.commits[0].short, commits: 3 }),
  });
  return { ...baseState(), workbenchOpen: true, workbenchTab: "git", workbenchWidth: 460 };
}

/** The same thread, on a worker that stopped to ask: the needs-you banner shows. */
function workerThread(): AppState {
  return {
    ...baseState({
      status: "needs_you",
      needsYou: true,
      summary: "The thread renders as Markdown — the component test still fails on the tool rows. Which way do you want them?",
      lastActivity: "just now",
      createdAtMs: NOW_MS - 18 * 60_000,
      activity: liveActivity("waiting", 75_000),
    }),
    route: "lead",
    selectedWorkerId: RICH_WORKER_ID,
    workerPanelId: RICH_WORKER_ID,
    workbenchOpen: false,
    workerDeletion: null,
  };
}

/** The lead chat: a brief, a plan, a dispatch, a review note, a merge, a tool row. */
function leadChat(): AppState {
  const state = baseState();
  const extra: LeadEntry[] = [
    {
      id: "preview-lead-tool-row",
      kind: "tool",
      time: "09:58",
      toolCallId: "call-preview-team-status",
      toolName: "team_status",
      arguments: JSON.stringify({ worker_id: RICH_WORKER_ID }),
      liveOutput: "",
      result: { success: true, output: "Kai · working · 7 turns · 0 open findings" },
    },
    {
      id: "preview-lead-closing",
      kind: "message",
      author: "lead",
      time: "10:01",
      body: [
        "## Where the team stands",
        "",
        "Ana's review is closed and Ren's merge is in. Kai is re-running a component test.",
        "",
        "- **Done:** shell, review evidence, delete-old sheet",
        "- **Running:** thread Markdown, tool-row cards",
        "",
        "```bash",
        "npm run check:mnemo2",
        "npx vitest run",
        "```",
      ].join("\n"),
    },
  ];
  return {
    ...state,
    route: "lead",
    leadEntries: [...state.leadEntries, ...extra],
    selectedWorkerId: null,
    workerPanelId: null,
    workbenchOpen: false,
    workerDeletion: null,
  };
}

/** The lead chat with an idle Claude Code lead: the top bar shows the badge. */
function leadIdle(): AppState {
  return {
    ...leadChat(),
    externalLead: {
      attached: false,
      root: "C:/Mnemo",
      sinceMs: null,
      idleSinceMs: NOW_MS - 12 * 60_000,
    },
  };
}

/** The delete-old sheet, opened over mission control, with one superseded worker. */
function deleteSheet(): AppState {
  return {
    ...missionControl(),
    workerDeletion: {
      phase: "ready",
      rows: [
        {
          workerId: SUPERSEDED_PREVIEW.worker_id,
          name: SUPERSEDED_PREVIEW.name,
          preview: SUPERSEDED_PREVIEW,
          error: null,
          deleted: false,
        },
      ],
    },
  };
}

/** Seconds in a day — the engine's Cost series buckets UTC days. */
const DAY_SECONDS = 86_400;

/** The UTC day bucket of "today" at the time the scene is built. */
const TODAY_SECONDS = Math.floor(NOW_MS / 1000 / DAY_SECONDS) * DAY_SECONDS;

/**
 * The sample the `cost` scene seeds: two priced models and one unpriced, every
 * lever active, and a two-week series that ramps up. The browser preview has no
 * Tauri host, so these reports stand in for the two `insights_*` commands.
 */
function previewCostInsights(): CostInsights {
  const spendPerDay = [
    240_000, 510_000, 380_000, 720_000, 660_000, 990_000, 1_240_000, 860_000,
    1_120_000, 1_460_000, 1_020_000, 1_580_000, 1_810_000, 2_050_000,
  ];
  const savedPerDay = [
    380_000, 420_000, 330_000, 560_000, 520_000, 640_000, 760_000, 590_000,
    700_000, 810_000, 660_000, 890_000, 980_000, 706_000,
  ];
  const day = (index: number): number => TODAY_SECONDS - (13 - index) * DAY_SECONDS;
  return {
    cost: {
      total: {
        input_tokens: 4_540_000,
        cached_tokens: 6_060_000,
        output_tokens: 580_000,
        cost_micros: 13_510_000,
      },
      models: [
        {
          model: "anthropic/claude-opus-5-5",
          input_tokens: 3_200_000,
          cached_tokens: 5_100_000,
          output_tokens: 420_000,
          cost_micros: 12_470_000,
        },
        {
          model: "glm-5.3-flash",
          input_tokens: 1_100_000,
          cached_tokens: 900_000,
          output_tokens: 130_000,
          cost_micros: 1_040_000,
        },
        {
          model: "ollama/qwen3:32b",
          input_tokens: 240_000,
          cached_tokens: 60_000,
          output_tokens: 30_000,
          cost_micros: null,
        },
      ],
      unpriced_models: ["ollama/qwen3:32b"],
      per_day: spendPerDay.map((costMicros, index) => ({ day: day(index), cost_micros: costMicros })),
    },
    savings: {
      tokens_saved: 8_946_000,
      estimated_saved_micros: 2_380_000,
      levers: [
        { kind: "delta_read", events: 1_240, tokens_saved: 4_180_000, estimated_saved_micros: 1_090_000 },
        { kind: "skeleton", events: 862, tokens_saved: 1_910_000, estimated_saved_micros: 505_000 },
        { kind: "recall_delta", events: 214, tokens_saved: 1_060_000, estimated_saved_micros: 280_000 },
        { kind: "compression", events: 388, tokens_saved: 940_000, estimated_saved_micros: 248_000 },
        { kind: "archive", events: 46, tokens_saved: 580_000, estimated_saved_micros: 153_000 },
        { kind: "outline", events: 102, tokens_saved: 246_000, estimated_saved_micros: 65_000 },
        { kind: "truncation", events: 61, tokens_saved: 150_000, estimated_saved_micros: 39_000 },
        { kind: "compaction_checkpoint", events: 9, tokens_saved: 0, estimated_saved_micros: 0 },
        { kind: "archive_expand", events: 6, tokens_saved: -120_000, estimated_saved_micros: 0 },
      ],
      cache: {
        prompt_tokens: 12_400_000,
        cached_tokens: 8_900_000,
        hit_rate: 0.718,
        estimated_saved_micros: 1_850_000,
      },
      per_day: savedPerDay.map((tokensSaved, index) => ({ day: day(index), tokens_saved: tokensSaved })),
    },
  };
}

/** The Cost tab over the same project: spend, savings, cache, and two weeks of bars. */
function costTab(): AppState {
  seedCostPreview(previewCostInsights());
  return { ...baseState(), route: "cost" };
}

/** The plans the Plans tab scene shows: two open, one closed. */
const PREVIEW_PLANS: BoardPlan[] = [
  {
    id: "plan-preview-plans-tab",
    title: "Plans tab with expandable details",
    goal: "Plans become a top-level tab whose cards expand to the whole plan document.",
    kind: "implementation",
    status: "open",
    closed_at_ms: null,
    report_path: null,
    steps: [
      {
        number: 1,
        header: "Plans tab and route",
        text: "Add the Plans route, the top-bar button, and a full-width view that reuses the plan cards.",
        done: true,
        workers: [{ id: RICH_WORKER_ID, name: "Kai", status: "working" }],
      },
      {
        number: 2,
        header: "Expandable details",
        text: "Give every card a Details control that lazily reads board_plan_document and renders the plan as Markdown; re-read an open card when the board changes.",
        done: false,
        workers: [{ id: "ana", name: "Ana", status: "working" }],
      },
      {
        number: 3,
        header: "Preview scene",
        text: "Add the plans preview scene with several cards and one expanded.",
        done: false,
        workers: [],
      },
    ],
  },
  {
    id: "plan-preview-cost-cache",
    title: "Cost tab: prompt-cache savings",
    goal: "Show prompt-cache tokens saved and their dollar value beside the other levers.",
    kind: "implementation",
    status: "open",
    closed_at_ms: null,
    report_path: null,
    steps: [
      {
        number: 1,
        header: "Cache report",
        text: "Read cache tokens per model from the usage ledger and price them at the model's input rate.",
        done: true,
        workers: [],
      },
      {
        number: 2,
        header: null,
        text: "Render the cache lever in the savings table with its event count.",
        done: false,
        workers: [{ id: "sol", name: "Sol", status: "working" }],
      },
    ],
  },
  {
    id: "plan-preview-shell-close",
    title: "Close the project from the top bar",
    goal: "One control closes the project after a clean-worktree review.",
    kind: "implementation",
    status: "closed",
    closed_at_ms: NOW_MS - 3 * 60 * 60_000,
    report_path: ".coding/reviews/shell-close.md",
    steps: [
      {
        number: 1,
        header: null,
        text: "Add the close request, the review sheet and the conflict route.",
        done: true,
        workers: [],
      },
    ],
  },
];

/** Join plan-file lines into the markdown the preview seed serves. */
const planMarkdown = (...lines: string[]) => lines.join("\n");

/** The plan files the seed serves, in the shape `.coding/plans/<id>.md` writes. */
const PREVIEW_PLAN_DOCUMENTS: Record<string, string> = {
  "plan-preview-plans-tab": planMarkdown(
    "# Plan: Plans tab with expandable details",
    "",
    "## Goal",
    "Plans become a top-level tab whose cards expand to the whole plan document.",
    "",
    "## Kind",
    "implementation",
    "",
    "## Context",
    "The user opened the Workbench's Plan tab and saw a title, a collapsed goal and one",
    "line per step — then asked whether plans are really that short. They are not: a plan",
    "file is 4-10 KB, and every worker receives all of it.",
    "",
    "- The card already shows the title, goal and one line per step.",
    "- Details stay lazy: the board snapshot carries no context, acceptance or decisions.",
    "",
    "## Acceptance scenarios",
    "- The top bar shows Lead, Team, Cost, Graph and Plans; Plans opens the cards.",
    "- A closed Details control leaves the card exactly as it was.",
    "- Opening Details shows context, acceptance scenarios, decisions and the full steps.",
    "- board_plan_document refuses an unknown id without reading any file.",
    "",
    "## Steps",
    "- [x] 1. **Plans tab and route**: add the route, the top-bar button and a full-width view.",
    "- [ ] 2. **Expandable details**: lazy-read the document and render it as Markdown.",
    "- [ ] 3. **Preview scene**: plans shows several cards with one expanded.",
    "",
    "## Decisions",
    "- Reuse PlanTab's cards so the look stays identical; PlansView only adds width.",
    "- A card fetches once per open and re-reads on the board-changed event.",
    "",
    "## Interfaces",
    "- board_plan_document(plan_id: String) -> Result<String, String>",
    "- BoardTransport.document(planId: string): Promise<string>",
    "",
    "## Progress",
    "Step 1 is merged on wt/agent-plansTab; step 2 is in review.",
  ),
  "plan-preview-cost-cache": planMarkdown(
    "# Plan: Cost tab: prompt-cache savings",
    "",
    "## Goal",
    "Show prompt-cache tokens saved and their dollar value beside the other levers.",
    "",
    "## Context",
    "The Cost tab already prices usage per model. The cache rows exist in the report but",
    "the savings table ignores them, so the biggest lever reads as zero.",
    "",
    "## Acceptance scenarios",
    "- The savings table lists a Cache row with its tokens and dollars.",
    "- A cache row with no cached tokens is omitted.",
    "",
    "## Steps",
    "- [x] 1. **Cache report**: read cache tokens per model.",
    "- [ ] 2. **Savings lever**: render the row with its event count.",
    "",
    "## Decisions",
    "- Cache savings count toward the same total as the other levers.",
  ),
};

/** The plan inbox the Plans tab's Backlog section shows above the cards. */
const PREVIEW_BACKLOG: BacklogItem[] = [
  {
    id: "idea-preview-diff",
    text: "Show each worker's diff beside its thread, collapsed to the changed files first.",
    images: [],
    status: "pending",
    created_at: 0,
  },
  {
    id: "idea-preview-reopen",
    text: "Let the lead reopen a closed plan from the Plans tab and dispatch its remaining steps.",
    images: [],
    status: "pending",
    created_at: 0,
    deferred: true,
  },
  {
    id: "idea-preview-archive",
    text: "Archive the backlog's finished ideas into the project memory index.",
    images: [],
    status: "pending",
    created_at: 0,
  },
  {
    id: "idea-preview-embedded",
    text: "Let each worker's panel scroll its own log without moving the thread.",
    images: [],
    status: "in_flight",
    created_at: 0,
  },
  {
    id: "idea-preview-burndown",
    text: "Show the plan board's steps as a burn-down per worker.",
    images: [],
    status: "done",
    created_at: 0,
  },
];

/** A tiny in-memory backlog that mirrors the native transport's write semantics. */
function previewBacklog(initial: BacklogItem[]): BacklogTransport {
  let items = initial.map((item) => ({ ...item }));
  const copy = () => items.map((item) => ({ ...item }));
  return {
    items: async () => copy(),
    add: async (text) => {
      items = [
        ...items,
        { id: `idea-preview-${items.length + 1}`, text, images: [], status: "pending", created_at: 0 },
      ];
      return copy();
    },
    edit: async (id, text) => {
      items = items.map((item) => (item.id === id ? { ...item, text } : item));
      return copy();
    },
    remove: async (id) => {
      items = items.filter((item) => item.id !== id);
      return copy();
    },
    reorder: async (ids) => {
      const rank = new Map(ids.map((id, index) => [id, index]));
      items = [...items].sort(
        (a, b) => (rank.get(a.id) ?? ids.length) - (rank.get(b.id) ?? ids.length),
      );
      return copy();
    },
    setDeferred: async (id, deferred) => {
      items = items.map((item) => (item.id === id ? { ...item, deferred } : item));
      return copy();
    },
    setDone: async (id, done) => {
      items = items.map((item) => {
        if (item.id !== id) return item;
        if (done && item.status === "pending") return { ...item, status: "done" };
        if (!done && item.status !== "pending" && item.status !== "in_flight") {
          return { ...item, status: "pending" };
        }
        return item;
      });
      return copy();
    },
    clearFinished: async () => {
      items = items.filter((item) => item.status === "pending");
      return copy();
    },
    planThis: async (id) => {
      items = items.map((item) =>
        item.id === id
          ? { ...item, status: "in_flight", note: "handed to the lead" }
          : item,
      );
      return copy();
    },
    onChanged: async () => () => {},
  };
}

/** The Plans tab over the same project, with one card expanded to its document. */
function plansTab(): AppState {
  seedBoardPreview({
    snapshot: async () => ({ plans: PREVIEW_PLANS }),
    document: async (planId) => {
      const markdown = PREVIEW_PLAN_DOCUMENTS[planId];
      if (markdown === undefined) throw new Error(`no preview plan document for ${planId}`);
      return markdown;
    },
    onChanged: async () => () => {},
  });
  seedBacklogPreview(previewBacklog(PREVIEW_BACKLOG));
  return { ...baseState(), board: PREVIEW_PLANS, route: "plans" };
}

/**
 * One preview record: the list row plus everything the reading pane renders for
 * it. The `path` mirrors what the indexer stores in `data.rel_path`
 * (`<kind>/<created>-<slug>.md`).
 */
interface SampleMemoryRecord {
  row: MemoryRow;
  /** The knowledge file's body, front matter stripped. */
  markdown: string;
  /** The repo path of the knowledge file the row derives from. */
  path: string;
  /** The front matter's created date. */
  created: string;
  /** The records whose links point here. */
  backlinks: string[];
  /** The wiki-link targets the body carries. */
  links: string[];
}

/** Unix seconds for a `YYYY-MM-DD` knowledge-file date (09:00 UTC). */
const recordClock = (day: string): number => Math.floor(Date.parse(`${day}T09:00:00Z`) / 1000);

/** Join knowledge-file lines into the Markdown the preview's reader shows. */
const memoryBody = (...lines: string[]) => lines.join("\n");

/** One sample record, its row derived exactly like `MemoryRow::from_memory`. */
function sampleRecord(spec: {
  id: string;
  title: string;
  kind: string;
  slug: string;
  created: string;
  markdown: string;
  tier?: string;
  links?: string[];
  backlinks?: string[];
  accessCount?: number;
}): SampleMemoryRecord {
  const relPath = `${spec.kind}/${spec.created}-${spec.slug}.md`;
  return {
    row: {
      id: spec.id,
      title: spec.title,
      kind: spec.kind,
      tier: spec.tier ?? "semantic",
      preview: spec.markdown.replace(/\s+/g, " ").trim().slice(0, 180),
      path: `.coding/knowledge/${relPath}`,
      created_at: recordClock(spec.created),
      last_accessed_at: recordClock(spec.created) + 5_400,
      access_count: spec.accessCount ?? 4,
      strength: 0.87,
      superseded: false,
      quarantined: null,
    },
    markdown: spec.markdown,
    path: `.coding/knowledge/${relPath}`,
    created: spec.created,
    backlinks: spec.backlinks ?? [],
    links: spec.links ?? [],
  };
}

/** The Memory tab's records: 20 knowledge files across the four types. */
const SAMPLE_MEMORY_RECORDS: SampleMemoryRecord[] = [
  sampleRecord({
    id: "spec-memory-tab-reads-the-store",
    title: "SPEC: the Memory tab reads the store, not a copy",
    kind: "spec",
    slug: "memory-tab-reads-the-store",
    created: "2027-02-14",
    accessCount: 9,
    links: ["spec/2027-02-09-knowledge-as-files", "decision/2027-02-13-memory-tab-is-read-only"],
    backlinks: ["spec-knowledge-as-files", "decision-memory-tab-read-only"],
    markdown: memoryBody(
      "The Memory tab is a read-only window onto the project's memory store. It renders what the agent's own memory tools read, so the user never sees a second, drifting truth.",
      "",
      "## What the tab shows",
      "",
      "- One newest-first list of every knowledge record and memory, filtered by `SPEC` / `DECISION` / `BUG` / `HOW`.",
      "- The selected record as Markdown, with its file path, its date and its backlinks.",
      "- A collapsed Diagnostics card: embedder status, per-tier counts and the store's own recall ranking.",
      "",
      "## Why the store and not the files",
      "",
      "The files under `.coding/knowledge/` are the authored truth, but a record is only *reachable* once the indexer has derived its row — recall, backlinks and supersession all live in the store. Reading the files directly would show records the agent cannot recall yet, which is worse than a slightly staler view.",
      "",
      "```rust",
      "let store = engine.memory_store_handle()?;",
      "let page = store.list_filtered(&MemoryFilter::new().limit(40)).await?;",
      "```",
      "",
      "## Limits",
      "",
      "The tab never writes: superseding a record stays a knowledge-file edit the indexer picks up, so history and provenance cannot fork.",
    ),
  }),
  sampleRecord({
    id: "decision-memory-tab-read-only",
    title: "DECISION: the Memory tab is read-only",
    kind: "decision",
    slug: "memory-tab-is-read-only",
    created: "2027-02-13",
    accessCount: 6,
    links: ["spec/2027-02-14-memory-tab-reads-the-store"],
    markdown: memoryBody(
      "Browsing knowledge and editing memory are different jobs with different failure modes. A read surface cannot corrupt a corpus, so the tab ships without a write path and the agent keeps the pen.",
      "",
      "Editing stays in the knowledge files, where a diff review can catch a wrong supersession before it reaches a merge.",
    ),
  }),
  sampleRecord({
    id: "bug-indexer-skipped-predecessor",
    title: "BUG: the memory indexer skipped an unchanged predecessor",
    kind: "bug",
    slug: "indexer-skipped-unchanged-predecessor",
    created: "2027-02-11",
    accessCount: 12,
    markdown: memoryBody(
      "Symptom: a record superseded a file whose content had not changed, and the predecessor kept its old `superseded_by` — recall still served the dead record.",
      "",
      "Cause: the content-hash skip short-circuited the row rewrite, so the successor resolution never ran for the unchanged file.",
      "",
      "Fix: an unchanged file still gets its derived metadata rewritten, which is idempotent. Regression test: `superseded_predecessor_is_rewritten_without_a_content_change`.",
    ),
  }),
  sampleRecord({
    id: "spec-knowledge-as-files",
    title: "SPEC: knowledge as files",
    kind: "spec",
    slug: "knowledge-as-files",
    created: "2027-02-09",
    accessCount: 21,
    links: ["decision/2027-02-13-memory-tab-is-read-only"],
    markdown: memoryBody(
      "Knowledge lives as Markdown files under `.coding/knowledge/{spec,decision,bug,how}/<date>-<slug>.md`: front matter (title, status, supersedes, created) plus an unbounded body with `[[links]]`.",
      "",
      "The indexer derives budgeted digests with a UUIDv5 key, a pointer line and the front matter's supersession, so a git merge converges instances instead of forking a database.",
    ),
  }),
  sampleRecord({
    id: "how-rebuild-the-memory-index",
    title: "HOW: rebuild the memory index from Settings",
    kind: "how",
    slug: "rebuild-the-memory-index",
    created: "2027-02-07",
    tier: "procedural",
    accessCount: 7,
    markdown: memoryBody(
      "1. Open Settings → Memory.",
      "2. Press **Rebuild index from files**. The dialog reports the reconciliation while it runs.",
      "3. Re-open the Memory tab; the list re-reads on mount.",
      "",
      "The databases (`memory.db`, `codegraph.db`) are rebuildable caches and gitignored — deleting one is always safe, and this is the control that repopulates it.",
    ),
  }),
  sampleRecord({
    id: "decision-branch-topology",
    title: "DECISION: branch topology collapsed to wt/* → main",
    kind: "decision",
    slug: "branch-topology-collapsed-to-wt-main",
    created: "2027-02-05",
    accessCount: 15,
    markdown: memoryBody(
      "The `develop` integration tier is gone: one `wt/*` working branch per agent directory, merged into `main` through the protected-PR path. The two-tier topology cost a merge step on every landing and bought nothing the review gate did not already enforce.",
    ),
  }),
  sampleRecord({
    id: "bug-worker-row-kept-its-timer",
    title: "BUG: the worker row kept its turn timer after the turn ended",
    kind: "bug",
    slug: "worker-row-kept-the-turn-timer",
    created: "2027-02-04",
    accessCount: 5,
    markdown: memoryBody(
      "Symptom: a finished worker still showed a running timer, and the team header counted it as active.",
      "",
      "Cause: the row read `turnStartedAtMs` from the activity snapshot instead of the phase — a `waiting` worker has an anchored turn but must not tick.",
      "",
      "Fix: the tick is keyed on the phase list, and a `done` event clears the anchor.",
    ),
  }),
  sampleRecord({
    id: "spec-code-graph-is-a-query-surface",
    title: "SPEC: the code graph is a query surface",
    kind: "spec",
    slug: "code-graph-is-a-query-surface",
    created: "2027-02-02",
    accessCount: 11,
    markdown: memoryBody(
      "The graph exists to answer four questions fast — where is a symbol, who calls it, what breaks if it changes, what does its neighbourhood look like — never to draw a picture of the repository.",
      "",
      "Every answer comes from one in-memory snapshot, so the status line, the search and the impact query cannot disagree.",
    ),
  }),
  sampleRecord({
    id: "how-run-the-workspace-tests",
    title: "HOW: run the workspace tests",
    kind: "how",
    slug: "run-the-workspace-tests",
    created: "2027-01-31",
    tier: "procedural",
    accessCount: 33,
    markdown: memoryBody(
      "`cargo test --workspace` covers the engine, the app and both feature trees.",
      "",
      "A feature-gated dependency bump additionally needs `cargo test --features embeddings` (fastembed/ONNX) or `--features browser` (chromiumoxide): plain `cargo test` never compiles those paths.",
    ),
  }),
  sampleRecord({
    id: "decision-prompts-live-in-rust",
    title: "DECISION: prompts live in Rust, not the frontend",
    kind: "decision",
    slug: "prompts-live-in-rust",
    created: "2027-01-29",
    accessCount: 8,
    markdown: memoryBody(
      "The system prompts are compiled constants on the Rust side, so the headless engine and the desktop app send byte-identical instructions and a prompt change is reviewable in a diff.",
    ),
  }),
  sampleRecord({
    id: "spec-plan-files-are-the-board",
    title: "SPEC: plan files are the single source of board state",
    kind: "spec",
    slug: "plan-files-are-the-single-source-of-board-state",
    created: "2027-01-27",
    accessCount: 9,
    markdown: memoryBody(
      "`.coding/plans/<id>.md` is the plan; the board is a parsed view of those files. A step's checkbox, its worker assignment and its report path all round-trip through the file, so a hand edit and a tool edit produce the same board.",
    ),
  }),
  sampleRecord({
    id: "bug-plan-card-lost-its-scroll",
    title: "BUG: the plan card lost its scroll position on rebuild",
    kind: "bug",
    slug: "plan-card-lost-its-scroll",
    created: "2027-01-25",
    accessCount: 4,
    markdown: memoryBody(
      "Symptom: an expanded plan card jumped back to its first line whenever the board refreshed.",
      "",
      "Cause: the card was re-keyed on the document string, so every poll replaced the scrolling node.",
      "",
      "Fix: the card is keyed on the plan id and the document is re-read into the same node.",
    ),
  }),
  sampleRecord({
    id: "how-rebase-a-worker-branch",
    title: "HOW: rebase a worker branch onto the lead tip",
    kind: "how",
    slug: "rebase-a-worker-branch-onto-the-lead-tip",
    created: "2027-01-23",
    tier: "procedural",
    accessCount: 6,
    markdown: memoryBody(
      "Fetch, rebase onto the lead's branch tip, then re-run the check scope before the merge is offered again. A conflict halts the merge and routes the branch back to its worker; a stale base is never auto-rebased.",
    ),
  }),
  sampleRecord({
    id: "decision-one-engine-per-project",
    title: "DECISION: one engine per open project",
    kind: "decision",
    slug: "one-engine-per-open-project",
    created: "2027-01-21",
    accessCount: 10,
    markdown: memoryBody(
      "Mnemo2 opens exactly one engine for the project it has open. A second engine would mean two memory stores, two code-graph indexes and two cost ledgers racing on the same files — every one of them a silent corruption.",
    ),
  }),
  sampleRecord({
    id: "spec-cost-ledger-append-only",
    title: "SPEC: the cost ledger is append-only",
    kind: "spec",
    slug: "the-cost-ledger-is-append-only",
    created: "2027-01-19",
    accessCount: 7,
    markdown: memoryBody(
      "Every turn appends one row per request with its tokens and model; nothing rewrites an existing row. Prices change, so pricing is resolved at read time and an unpriced model is reported as *partial*, never as zero.",
    ),
  }),
  sampleRecord({
    id: "bug-backlog-id-was-integer",
    title: "BUG: backlog_status took an integer id, the ids are UUIDs",
    kind: "bug",
    slug: "backlog-status-took-an-integer-id",
    created: "2027-01-17",
    accessCount: 3,
    markdown: memoryBody(
      "Symptom: `backlog_status` rejected every id the list returned.",
      "",
      "Cause: the schema declared `id: integer` while the backlog writes UUID strings.",
      "",
      "Fix: the parameter is a `String` end to end, with a regression test that round-trips a UUID.",
    ),
  }),
  sampleRecord({
    id: "how-preview-a-scene",
    title: "HOW: preview a scene in the browser",
    kind: "how",
    slug: "preview-a-scene-in-the-browser",
    created: "2027-01-15",
    tier: "procedural",
    accessCount: 12,
    markdown: memoryBody(
      "Run the dev server on the `mnemo2` folder, open `preview.html?scene=<name>`, and pick a scene from the index. The scene builds an `AppState` and plants its transport through the dev-only seed seam, so a reviewer sees the real layout with sample data.",
    ),
  }),
  sampleRecord({
    id: "decision-settings-are-per-project",
    title: "DECISION: settings are per project, endpoints are install-wide",
    kind: "decision",
    slug: "settings-are-per-project-endpoints-are-install-wide",
    created: "2027-01-13",
    accessCount: 9,
    markdown: memoryBody(
      "A project owns its models, budget, embedding choice and team slots; the endpoints they point at belong to the install. One endpoint list edited once cannot silently fork per checkout.",
    ),
  }),
  sampleRecord({
    id: "spec-mcp-is-the-only-external-surface",
    title: "SPEC: MCP is the only external surface",
    kind: "spec",
    slug: "mcp-is-the-only-external-surface",
    created: "2027-01-11",
    accessCount: 14,
    markdown: memoryBody(
      "An external lead drives Mnemo2 over MCP and nothing else: plan, dispatch, review, merge. Every tool the UI can trigger is a tool the external lead can trigger, so the two paths cannot drift into different behaviour.",
    ),
  }),
  sampleRecord({
    id: "bug-primer-counted-superseded-rows",
    title: "BUG: the memory primer counted superseded rows",
    kind: "bug",
    slug: "the-memory-primer-counted-superseded-rows",
    created: "2027-01-09",
    accessCount: 2,
    markdown: memoryBody(
      "Symptom: the primer's per-tier counts included records that recall excludes, so the user's \"what the agent knows\" number was too high.",
      "",
      "Fix: the counts apply the same filter defaults recall uses, and a test pins the two together.",
    ),
  }),
];

/** A tiny in-memory answer service over the sample records, so the preview's chips, search, reads and recall test all work. */
function sampleMemoryTransport(): MemoryTransport {
  const byId = new Map(SAMPLE_MEMORY_RECORDS.map((record) => [record.row.id, record]));
  const rows = (): MemoryRow[] =>
    SAMPLE_MEMORY_RECORDS.map((record) => record.row).sort((a, b) => b.created_at - a.created_at);
  const inGroup = (row: MemoryRow, kind: string): boolean => {
    if (kind === "" || kind === "all") return true;
    if (kind === "other") return !["spec", "decision", "bug", "how"].includes(row.kind);
    return row.kind === kind;
  };
  const hitRow = (row: MemoryRow, score: number): MemoryHit => ({ ...row, score });
  const rowsFor = (ids: string[]): MemoryRow[] =>
    ids.flatMap((id) => {
      const record = byId.get(id);
      return record ? [record.row] : [];
    });
  return {
    list: async (query = {}) => {
      const matching = rows().filter((row) => inGroup(row, query.kind ?? "all"));
      const offset = query.offset ?? 0;
      const limit = query.limit ?? LIST_PAGE_SIZE;
      return {
        rows: matching.slice(offset, offset + limit),
        offset,
        has_more: offset + limit < matching.length,
      };
    },
    search: async (text, limit = 40) => {
      const needle = text.trim().toLowerCase();
      return rows()
        .filter((row) => `${row.title} ${row.preview}`.toLowerCase().includes(needle))
        .slice(0, limit)
        .map((row, index) => hitRow(row, Math.max(0.35, 0.94 - index * 0.08)));
    },
    read: async (id) => {
      const record = byId.get(id);
      if (!record) throw new Error(`no preview record ${id}`);
      return {
        id: record.row.id,
        title: record.row.title,
        kind: record.row.kind,
        tier: record.row.tier,
        markdown: record.markdown,
        path: record.path,
        created: record.created,
        status: "live",
        superseded_by: null,
        quarantined: null,
        created_at: record.row.created_at,
        last_accessed_at: record.row.last_accessed_at,
        access_count: record.row.access_count,
        strength: record.row.strength,
        links: record.links,
        evidence: null,
        unconfirmed: false,
        backlinks: rowsFor(record.backlinks),
      };
    },
    backlinks: async (id) => rowsFor(byId.get(id)?.backlinks ?? []),
    overview: async () => ({
      available: true,
      reason: "",
      model_id: "bge-small-en-v1.5",
      dim: 384,
      configured_model: "bge-small-en-v1.5",
      fingerprints: [{ model: "bge-small-en-v1.5", dim: 384 }],
      fingerprint_matches: true,
      counts: { working: 128, episodic: 41, semantic: 214, procedural: 18, total: 401 },
      index: {
        authored_count: 386,
        derived_count: 15,
        last_built_at: Math.floor(NOW_MS / 1000) - 12 * 60,
      },
      embedder: "ready",
    }),
    recallTest: async (text, limit = 8) => {
      const words = text
        .toLowerCase()
        .split(/\s+/)
        .filter((word) => word.length > 3);
      return rows()
        .map((row) => {
          const haystack = `${row.title} ${row.preview}`.toLowerCase();
          const overlap = words.filter((word) => haystack.includes(word)).length;
          return hitRow(row, 0.58 + overlap * 0.06);
        })
        .sort((a, b) => b.score - a.score)
        .slice(0, limit);
    },
  };
}

/** The panel's Memory tab over the same project, with the newest record open in the reader. */
function memoryTab(): AppState {
  seedMemoryPreview(sampleMemoryTransport());
  return { ...baseState(), workerPanelId: null, workbenchOpen: true, workbenchTab: "memory" };
}

/** The lead chat with the tool-row bodies: named-file headlines, the expanded
 *  edit row showing its coloured diff, a replayed multi_edit rebuilt from
 *  its arguments, a write's added lines, the read row's file-and-range list,
 *  and the shell's command with split output. */
function leadToolRows(): AppState {
  const row = (id: string, toolName: string, args: Record<string, unknown>, result: ToolEntry["result"]): ToolEntry => ({
    id,
    kind: "tool",
    time: "10:04",
    toolCallId: `call-${id}`,
    toolName,
    arguments: JSON.stringify(args),
    liveOutput: "",
    result,
  });
  const readOutput = [
    "=== mnemo2/src/lib/toolSummary.ts (lines 1-65 of 65) ===",
    "1  import { actionPhrase, argPaths, basename, dedupePaths, prettyToolName } from …",
    "",
    "=== mnemo2/src/lib/types.ts (lines 272-276 of 316) ===",
    "272  export interface LeadToolResult {",
    "273    success: boolean;",
    "274    output: string;",
    "275    data?: unknown;",
    "276  }",
    "",
  ].join("\n");
  return {
    ...baseState(),
    route: "lead",
    selectedWorkerId: null,
    workerPanelId: null,
    workbenchOpen: false,
    workerDeletion: null,
    leadEntries: [
      {
        id: "preview-lead-tools-intro",
        kind: "message",
        author: "lead",
        time: "10:03",
        body: "Expanding a tool row now shows what it did — the diff an edit applied, the content a write produced, the files and line ranges a read returned — instead of raw Arguments JSON. Every rich row keeps the raw call behind its Show raw call toggle.",
      },
      row(
        "preview-lead-tools-read",
        "read_files",
        {
          files: [
            { path: "mnemo2/src/lib/toolSummary.ts" },
            { path: "mnemo2/src/lib/types.ts", start_line: 272, max_lines: 5 },
          ],
        },
        { success: true, output: readOutput },
      ),
      row(
        "preview-lead-tools-edit",
        "file_edit",
        {
          path: "mnemo2/src/lib/toolSummary.ts",
          old_string: "export function toolSummary(",
          new_string: "export function toolHeadline(",
        },
        {
          success: true,
          output: "Edited mnemo2/src/lib/toolSummary.ts",
          data: {
            diff: [
              "--- a/mnemo2/src/lib/toolSummary.ts",
              "+++ b/mnemo2/src/lib/toolSummary.ts",
              "@@ -54,6 +54,7 @@",
              " /** A compact human headline derived from the actual call, never its outcome. */",
              "-export function toolSummary(",
              "+export function toolHeadline(",
              "   toolName: string,",
              "   args: string,",
              "   nameOf: (workerId: string) => string = (workerId) => workerId,",
              "-): string {",
              "+): { text: string; title: string } {",
            ].join("\n"),
          },
        },
      ),
      row(
        "preview-lead-tools-multi",
        "multi_edit",
        {
          files: [
            {
              path: "mnemo2/src/components/ToolActivity.svelte",
              ops: [
                {
                  old_string: "toolSummary(entry.toolName, entry.arguments, nameOf)",
                  new_string: "toolHeadline(entry.toolName, entry.arguments, nameOf)",
                },
              ],
            },
            { path: "mnemo2/src/components/ToolDiff.svelte" },
            { path: "mnemo2/src/components/ToolShellOutput.svelte" },
            { path: "mnemo2/src/lib/toolDiff.ts" },
            { path: "mnemo2/src/lib/toolDiff.test.ts" },
          ],
        },
        // A replayed transcript rebuilds results from the message text: no
        // structured diff, so the row reconstructs one from its arguments.
        { success: true, output: "Edited 1 file" },
      ),
      row(
        "preview-lead-tools-write",
        "file_write",
        {
          path: "mnemo2/src/components/ToolShellOutput.svelte",
          content: '<script lang="ts">\n  import TerminalText from "./TerminalText.svelte";\n</script>\n',
        },
        { success: true, output: "Wrote mnemo2/src/components/ToolShellOutput.svelte" },
      ),
      row(
        "preview-lead-tools-shell",
        "shell",
        { command: "npx vitest run src/lib src/components", purpose: "running tests", cwd: "mnemo2" },
        {
          success: true,
          output: "Test Files  1 passed (1)\n[stderr]\nnpm notice run mnemo@1.3.0\n[exit code: 0]",
        },
      ),
      row(
        "preview-lead-tools-git",
        "git_read",
        { op: "status" },
        { success: true, output: "## wt/agent-toolRows\nM mnemo2/src/components/ToolActivity.svelte\n" },
      ),
    ],
  };
}

/** Every scene the preview page renders, in index order. */
export const previewScenes: PreviewScene[] = [
  {
    name: "mission-control",
    title: "Mission control",
    description: "The team list with a live status row per running worker, and a selected worker whose thread has Markdown and tool rows.",
    build: missionControl,
  },
  {
    name: "graph",
    title: "Code graph",
    description: "The Graph tab: status and entry points, then a focused neighbourhood with callers, callees and change impact.",
    build: graphScene,
  },
  {
    name: "files",
    title: "Files",
    description: "The Files tab: the project tree, a path filter, and one file read-only with syntax highlighting.",
    build: filesTab,
  },
  {
    name: "git",
    title: "Git",
    description:
      "The Git tab: the current branch, every branch's distance from main, and the newest commits on one lane.",
    build: gitTab,
  },
  {
    name: "worker-thread",
    title: "Worker thread",
    description: "The same thread beside the lead chat, for a worker that stopped to ask.",
    build: workerThread,
  },
  {
    name: "lead-chat",
    title: "Lead chat",
    description: "The lead conversation: plan, dispatch, review, merge, and a tool row.",
    build: leadChat,
  },
  {
    name: "settings-models",
    title: "Settings: Models and Savings",
    description:
      "The settings dialog's Models page (per-task, vision, code indexing, budget) and Savings page (levers) against seeded data.",
    // The dialog opens from component-local state, so the scene mounts it
    // directly with the preview fakes seeded; `build` keeps the app shell
    // valid for the scenes test.
    build: missionControl,
    mount: (target) => {
      const restore = seedPreviewSettings();
      const dialog = svelteMount(SettingsDialog, { target, props: { open: true, onclose: () => {} } });
      return () => {
        unmount(dialog);
        restore();
      };
    },
  },
  {
    name: "settings-tiers",
    title: "Settings: Tiers",
    description:
      "The settings dialog's Tiers page: the top control (does Laya help pick the tier?) with its readiness line and Laya check, the five-rung ladder editor with fold hints and Laya labels, and three collapsed sections: what the lead sees, how Laya is doing, other things Laya can do.",
    // Same mount pattern as settings-models; the rail click opens the
    // Tiers page directly so the scene shows the page it names.
    build: missionControl,
    mount: (target) => {
      const restoreSettings = seedPreviewSettings();
      const restoreLaya = seedPreviewLaya();
      const dialog = svelteMount(SettingsDialog, { target, props: { open: true, onclose: () => {} } });
      const tiers = [...target.querySelectorAll<HTMLButtonElement>("nav button")].find(
        (button) => button.textContent === "Tiers",
      );
      tiers?.click();
      return () => {
        unmount(dialog);
        restoreLaya();
        restoreSettings();
      };
    },
  },
  {
    name: "settings-safety",
    title: "Settings: Safety",
    description:
      "The settings dialog's Safety page: the four approval modes with the no-prompt mode marked risky and the current one flagged, plus the open project's auto-approve rules with add, remove and Save.",
    // Same mount pattern as settings-laya; the rail click opens the Safety
    // page directly so the scene shows the page it names.
    build: missionControl,
    mount: (target) => {
      const restoreSettings = seedPreviewSettings();
      const restoreSafety = seedPreviewSafety();
      const dialog = svelteMount(SettingsDialog, { target, props: { open: true, onclose: () => {} } });
      const safety = [...target.querySelectorAll<HTMLButtonElement>("nav button")].find(
        (button) => button.textContent === "Safety",
      );
      safety?.click();
      return () => {
        unmount(dialog);
        restoreSafety();
        restoreSettings();
      };
    },
  },
  {
    name: "settings-mcp",
    title: "Settings: MCP servers",
    description:
      "The settings dialog's MCP page: a global stdio server (connected, with its live tool count), a project remote server and a disabled entry, each with Test/Edit/Remove, plus the add form and the note on when a save reaches the lead and its workers.",
    // Same mount pattern as settings-safety; the rail click opens the MCP
    // page directly so the scene shows the page it names.
    build: missionControl,
    mount: (target) => {
      const restoreSettings = seedPreviewSettings();
      const restoreMcp = seedPreviewMcp();
      const dialog = svelteMount(SettingsDialog, { target, props: { open: true, onclose: () => {} } });
      const mcp = [...target.querySelectorAll<HTMLButtonElement>("nav button")].find(
        (button) => button.textContent === "MCP",
      );
      mcp?.click();
      return () => {
        unmount(dialog);
        restoreMcp();
        restoreSettings();
      };
    },
  },
  {
    name: "settings-appearance",
    title: "Settings: Appearance & sounds",
    description:
      "The settings dialog's Appearance page: the three [ui] notification-sound toggles, each with a Test button that plays its tone, plus the Light / Dark / System theme choice that repaints the app at once.",
    // Same mount pattern as settings-mcp; the rail click opens the
    // Appearance page directly so the scene shows the page it names.
    build: missionControl,
    mount: (target) => {
      const restoreSettings = seedPreviewSettings();
      const restoreAppearance = seedPreviewAppearance();
      const dialog = svelteMount(SettingsDialog, { target, props: { open: true, onclose: () => {} } });
      const appearance = [...target.querySelectorAll<HTMLButtonElement>("nav button")].find(
        (button) => button.textContent === "Appearance",
      );
      appearance?.click();
      return () => {
        unmount(dialog);
        restoreAppearance();
        restoreSettings();
      };
    },
  },
  {
    name: "settings-project",
    title: "Settings: Project",
    description:
      "The settings dialog's Project page — the page the top bar's gear opens: the open project's worker slots, lead model and merge policy, under the rail's This project / All projects groups.",
    // Same mount pattern as settings-appearance, plus the app state the page
    // reads from: the project is seeded open, and the dialog is asked for the
    // Project page exactly as the gear asks for it.
    build: missionControl,
    mount: (target) => {
      app.set(missionControl());
      const restoreSettings = seedPreviewSettings();
      const dialog = svelteMount(SettingsDialog, { target, props: { open: true, onclose: () => {}, initialPage: "project" } });
      return () => {
        unmount(dialog);
        restoreSettings();
      };
    },
  },
  {
    name: "mission-control-dark",
    title: "Mission control (dark)",
    description:
      "The mission-control scene in the dark palette: the worker list with live status rows, and the selected worker's thread on the deep-green canvas.",
    build: missionControl,
    theme: "dark",
  },
  {
    name: "lead-chat-dark",
    title: "Lead chat (dark)",
    description:
      "The lead-chat scene in the dark palette: plan, dispatch, review and merge rows, a tool row and the composer on the dark surface.",
    build: leadChat,
    theme: "dark",
  },
  {
    name: "graph-dark",
    title: "Code graph (dark)",
    description:
      "The Graph tab in the dark palette: the module map and node canvas, their edges, and the symbol inspector with the dark tokens.",
    build: graphScene,
    theme: "dark",
  },
  {
    name: "lead-idle",
    title: "Lead idle",
    description: "The top bar with an idle Claude Code lead badge.",
    build: leadIdle,
  },
  {
    name: "delete-sheet",
    title: "Delete-old sheet",
    description: "The delete sheet with one superseded worker.",
    build: deleteSheet,
  },
  {
    name: "cost",
    title: "Cost",
    description: "The Cost tab: spend by model, savings per lever, prompt-cache hits and the last 14 days.",
    build: costTab,
  },
  {
    name: "plans",
    title: "Plans",
    description:
      "The top-level Plans tab: open and closed plan cards, the first one expanded to its context, acceptance scenarios, decisions and full steps.",
    build: plansTab,
    // Cards start closed by design, so the scene clicks the first Details
    // control after mounting to show the expanded document.
    mount: (target) => {
      app.set(plansTab());
      const shell = svelteMount(App, { target });
      target.querySelector<HTMLButtonElement>(".details-toggle")?.click();
      return () => unmount(shell);
    },
  },
  {
    name: "memory",
    title: "Memory",
    description:
      "The top-level Memory tab: the project's knowledge records newest first with the SPEC/DECISION/BUG/HOW chips, one record rendered as Markdown with its path, date and backlinks, and the Diagnostics card (embedder, per-tier counts, recall test).",
    build: memoryTab,
    // Diagnostics is collapsed by design, so the scene opens it and runs one
    // recall after mounting — otherwise the screenshot would show an empty
    // control row instead of the card the tab ships.
    mount: (target) => {
      app.set(memoryTab());
      const shell = svelteMount(App, { target });
      const diagnostics = target.querySelector<HTMLDetailsElement>(".diagnostics");
      if (diagnostics) diagnostics.open = true;
      const recallForm = target.querySelector<HTMLFormElement>(".recall-box");
      const recallInput = target.querySelector<HTMLInputElement>("#recall-query");
      if (recallForm && recallInput) {
        recallInput.value = "why does the memory tab read the store";
        recallInput.dispatchEvent(new Event("input", { bubbles: true }));
        setTimeout(() => recallForm.requestSubmit(), 0);
      }
      return () => unmount(shell);
    },
  },
  {
    name: "trace",
    title: "Trace",
    description:
      "The panel's Trace tab: the filter bar, the export/retention controls, and a list with a streaming lead call, a failed worker call, an unpriced call and retained metadata, plus the charts and the call detail.",
    build: traceScene,
  },
  {
    name: "trace-empty",
    title: "Trace (empty)",
    description: "The panel's Trace tab before any model call was recorded: the empty state, the filters and the data controls.",
    build: traceEmpty,
  },
  {
    name: "panel-team-diff",
    title: "Panel: Team with Diff",
    description: "The side panel open on its Diff tab beside Mission Control, resized to 460px.",
    build: panelTeamDiff,
  },
  {
    name: "panel-team-graph",
    title: "Panel: Team with Graph",
    description: "The side panel open on its Graph tab beside Mission Control: the code-graph explorer with its sample slice.",
    build: panelTeamGraph,
  },
  {
    name: "panel-cost-trace",
    title: "Panel: Cost with Trace",
    description: "The side panel open on its Trace tab beside the Cost tab: the filter bar and the list with a streaming lead call, a failed worker call, an unpriced call and retained metadata.",
    build: panelCostTrace,
  },
  {
    name: "panel-plans-memory",
    title: "Panel: Plans with Memory",
    description: "The side panel open on its Memory tab beside the Plans tab: the knowledge list and the newest record open in the reader.",
    build: panelPlansMemory,
  },
  {
    name: "lead-tool-rows",
    title: "Lead tool rows",
    description:
      "The lead chat's tool rows with file-named headlines: the expanded edit row shows its coloured diff, a replayed multi_edit reconstructs one from its arguments, a write shows its added lines, a read lists its files and line ranges, and the shell row keeps its command and split output.",
    build: leadToolRows,
    // The rows' bodies are component-local state, so the scene opens the edit
    // row after mounting — its diff is the body this scene is about; the
    // other rows stay collapsed for the index click-through.
    mount: (target) => {
      app.set(leadToolRows());
      const shell = svelteMount(App, { target });
      const editRow = target.querySelector<HTMLDetailsElement>(
        'article[aria-label^="Editing"] .tool-disclosure',
      );
      if (editRow !== null) editRow.open = true;
      return () => unmount(shell);
    },
  },
];


/** The scene an empty `?scene=` parameter falls back to. */
export const defaultPreviewScene = "mission-control";

/**
 * Resolve a `?scene=` value. An absent or empty value is the default scene; an
 * unknown name resolves to null so the page can show its index instead.
 */
export function previewScene(name: string | null | undefined): PreviewScene | null {
  const wanted = name === null || name === undefined || name.trim() === "" ? defaultPreviewScene : name.trim();
  return previewScenes.find((scene) => scene.name === wanted) ?? null;
}
