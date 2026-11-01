/**
 * Sample data for the dev-only preview page (`preview.html?scene=<name>`).
 *
 * This is test-side code: it builds on `fixtures.ts` (which the production
 * entry never imports) and is imported only by `src/preview/main.ts` and its
 * test. `index.html` never reaches it, so `vite build` bundles none of it and
 * `npm run check:no-mock` keeps passing.
 */

import type { BoardPlan } from "../lib/boardState";
import type { WorkerDeletionPreview } from "../lib/teamState";
import type { AppState, LeadEntry, Worker, WorkerActivity, WorkerThreadEntry } from "../lib/types";
import { createLeadEntries, createWorkers, fixtureState } from "./fixtures";

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

/** Every scene the preview page renders, in index order. */
export const previewScenes: PreviewScene[] = [
  {
    name: "mission-control",
    title: "Mission control",
    description: "The team list with a live status row per running worker, and a selected worker whose thread has Markdown and tool rows.",
    build: missionControl,
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
