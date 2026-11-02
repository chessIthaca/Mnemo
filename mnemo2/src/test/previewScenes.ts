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
import type { AppState, LeadEntry, Worker, WorkerActivity, WorkerThreadEntry } from "../lib/types";

import { createLeadEntries, createWorkers, fixtureState } from "./fixtures";
import { seedCostPreview, type CostInsights } from "../lib/cost";

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

/** The open plan the dispatch bar shows above the worker list. */
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

const SAMPLE_SYMBOL_BY_ID = new Map(SAMPLE_SYMBOLS.map((symbol) => [symbol.id, symbol]));

const toSymbol = ({ id, name, kind, file, line }: SampleSymbol): GraphSymbol => ({ id, name, kind, file, line });

/** A tiny in-memory answer service over the sample symbols, so the preview's search, focus and refocus all work. */
function sampleGraphTransport(): GraphTransport {
  let indexing = false;
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

/** The Graph tab: the code-graph explorer seeded with a sample slice of this project. */
function graphScene(): AppState {
  seedGraphPreview(sampleGraphTransport());
  return {
    ...baseState(),
    route: "graph",
    selectedWorkerId: null,
    workerPanelId: null,
    workbenchOpen: false,
    workerDeletion: null,
  };
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
    workerDeletion: { previews: [SUPERSEDED_PREVIEW], busy: false },
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
    clearFinished: async () => {
      items = items.filter((item) => item.status === "pending");
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
      "The settings dialog's Models page (per-task, vision, code indexing, budget) and Savings page (levers, pricing) against seeded data.",
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
    name: "settings-laya",
    title: "Settings: Decisions",
    description:
      "The settings dialog's Decisions page: the Laya status block with Enable/Disable and install progress, the seven opt-in switches, and the routing lanes with the confidence threshold and enforce switch.",
    // Same mount pattern as settings-models; the rail click opens the
    // Decisions page directly so the scene shows the page it names.
    build: missionControl,
    mount: (target) => {
      const restoreSettings = seedPreviewSettings();
      const restoreLaya = seedPreviewLaya();
      const dialog = svelteMount(SettingsDialog, { target, props: { open: true, onclose: () => {} } });
      const decisions = [...target.querySelectorAll<HTMLButtonElement>("nav button")].find(
        (button) => button.textContent === "Decisions",
      );
      decisions?.click();
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
