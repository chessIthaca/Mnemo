import { get } from "svelte/store";
import { createTeamTransport, type TeamTransport } from "../teamEngine";
import { probeMnemo2Host } from "../runtime";
import { createStreamCoalescer } from "../streamCoalescer";
import {
  applyModelSwitch,
  applyWorkerLiveEvent,
  emptyWorkerLive,
  STREAM_ONLY_EVENTS,
  type WorkerLive,
} from "../workerThread";
import { applyWorkerEvent, applyWorkerStatus } from "../workerActivity";
import { notifySound, workerTransitionSound } from "../sounds";
import {
  projectUsageFrom,
  restoreTranscripts,
  type CachedTranscript,
  type TeamEvent,
  type TeamMessage,
  type TeamWorkerRecord,
} from "../teamState";
import type { Worker, WorkerActivity, WorkerStatus } from "../types";
import { app, leadError, withToast } from "./core";
import { buildWorker, mergeTeamActivity } from "./workerView";
import { scheduleBoardSync } from "./board";
import { scheduleDiffSync } from "./workbench";

let teamTransport: TeamTransport | null = null;
let teamUnlisten: (() => void) | null = null;
let teamToken = 0;
let teamSyncTimer: ReturnType<typeof setTimeout> | null = null;

export function requireTeam(): TeamTransport | null {
  if (!teamTransport) {
    app.update((state) => withToast(state, "The native team runtime is not connected.", "error"));
    return null;
  }
  return teamTransport;
}

export function noteTeamError(error: unknown): void {
  app.update((state) => withToast(state, leadError(error), "error"));
}

/** The connected team transport, or null while the team runtime is not connected. */
export function getTeamTransport(): TeamTransport | null {
  return teamTransport;
}

function workerFromRecord(
  record: TeamWorkerRecord,
  index: number,
  messages: TeamMessage[],
): Worker {
  const status = record.status as WorkerStatus;
  // Fold the status snapshot into the live activity: needs_you waits on the
  // human, dispatching has not streamed yet, and non-running statuses clear.
  const activity = applyWorkerStatus(workerActivity.get(record.id) ?? null, status, Date.now());
  if (activity) workerActivity.set(record.id, activity);
  else workerActivity.delete(record.id);
  return buildWorker(record, index, messages, status, activity);
}

/** Each worker's streamed activity since its transcript was last saved. */
const workerLive = new Map<string, WorkerLive>();
/** Each worker's saved thread and the record it came from. */
const workerSaved = new Map<string, { generation: number; length: number; thread: Worker["messages"] }>();
/** Each worker's transcript the last team snapshots delivered, keyed by worker id. */
const transcriptCache = new Map<string, CachedTranscript>();

/** A worker's thread: its saved conversation plus the live tail on top. */
function composeThread(workerId: string): Worker["messages"] | null {
  const saved = workerSaved.get(workerId);
  if (!saved) return null;
  const live = workerLive.get(workerId);
  if (!live || live.generation !== saved.generation || live.baseLength !== saved.length) {
    workerLive.delete(workerId);
    return saved.thread;
  }
  return [...saved.thread, ...live.entries];
}

/** Each worker's live activity: what its model is doing right now, for every running worker — not just open threads. */
const workerActivity = new Map<string, WorkerActivity>();

/** Fold one streamed worker event into its worker's live activity, for every worker in state. */
function streamWorkerActivity(event: TeamEvent): void {
  const workerId = event.worker_id;
  const current = workerActivity.get(workerId) ?? null;
  const next = applyWorkerEvent(current, event, Date.now());
  if (next === current) return;
  if (next) workerActivity.set(workerId, next);
  else workerActivity.delete(workerId);
  workerStream.push({ token: teamToken, workerId, thread: false });
}

/**
 * A worker whose activity or open thread changed in the maps above. The store
 * write is coalesced (see streamCoalescer.ts): a batch re-reads the maps, so
 * it carries the newest state and can never undo a later team sync.
 */
interface WorkerStreamMark {
  token: number;
  workerId: string;
  thread: boolean;
}

const workerStream = createStreamCoalescer<WorkerStreamMark>(applyWorkerStreamMarks);

function applyWorkerStreamMarks(marks: WorkerStreamMark[]): void {
  const threads = new Set<string>();
  const activities = new Set<string>();
  for (const mark of marks) {
    if (mark.token !== teamToken) continue;
    (mark.thread ? threads : activities).add(mark.workerId);
  }
  const composed = new Map<string, Worker["messages"]>();
  for (const workerId of threads) {
    const thread = composeThread(workerId);
    if (thread) composed.set(workerId, thread);
  }
  if (activities.size === 0 && composed.size === 0) return;
  app.update((state) => ({
    ...state,
    workers: state.workers.map((worker) => {
      const activity = activities.has(worker.id);
      const thread = composed.get(worker.id);
      if (!activity && !thread) return worker;
      return {
        ...worker,
        ...(activity ? { activity: workerActivity.get(worker.id) ?? null } : {}),
        ...(thread ? { messages: thread } : {}),
      };
    }),
  }));
}

/** Stream one worker event into that worker's live activity and open thread. */
function streamWorkerEvent(event: TeamEvent): void {
  streamWorkerActivity(event);
  const saved = workerSaved.get(event.worker_id);
  if (!saved || event.generation < saved.generation) return;
  let live = workerLive.get(event.worker_id);
  if (!live || live.generation !== event.generation || live.baseLength !== saved.length) {
    live = emptyWorkerLive(event.generation, saved.length);
  }
  const next = applyWorkerLiveEvent(live, event);
  if (next === live && workerLive.get(event.worker_id) === live) return;
  workerLive.set(event.worker_id, next);
  if (event.generation !== saved.generation) return;
  workerStream.push({ token: teamToken, workerId: event.worker_id, thread: true });
}

/**
 * Note a live model switch in the worker's open thread tail. The note rides
 * the live entries (the saved transcript never holds it), stays in step with
 * the current saved generation, and flips to "Now running on …" once the
 * kernel's model_changed event reports the new model serving.
 */
export function noteWorkerModelSwitch(workerId: string, model: string): void {
  const saved = workerSaved.get(workerId);
  if (!saved) return;
  let live = workerLive.get(workerId);
  if (!live || live.generation !== saved.generation || live.baseLength !== saved.length) {
    live = emptyWorkerLive(saved.generation, saved.length);
  }
  workerLive.set(workerId, applyModelSwitch(live, model));
  const thread = composeThread(workerId);
  if (!thread) return;
  app.update((state) => ({
    ...state,
    workers: state.workers.map((worker) =>
      worker.id === workerId ? { ...worker, messages: thread } : worker,
    ),
  }));
}

export async function syncTeam(): Promise<void> {
  const transport = teamTransport;
  if (!transport) return;
  const token = teamToken;
  const projectId = get(app).projectId;
  // Tell the host which transcript revisions the window already holds: it
  // leaves those workers' transcripts out and the cache puts them back below.
  const known: Record<string, number> = {};
  for (const [id, cached] of transcriptCache) known[id] = cached.rev;
  try {
    const [raw, messages] = await Promise.all([
      transport.snapshot(known),
      transport.messages(),
    ]);
    if (token !== teamToken) return;
    const snapshot = restoreTranscripts(raw, transcriptCache);
    if (!snapshot) {
      // The host holds a thread the window cannot rebuild: drop the cache and
      // ask again for everything.
      transcriptCache.clear();
      scheduleTeamSync();
      return;
    }
    // Merges tick steps and workers move through statuses; the board follows.
    const before = new Map(get(app).workers.map((worker) => [worker.id, worker]));
    const boardStale =
      snapshot.workers.length !== before.size ||
      snapshot.workers.some((record) => {
        const previous = before.get(record.id);
        return !previous ||
          previous.status !== record.status ||
          (previous.planId ?? null) !== (record.plan_id ?? null) ||
          (previous.planStep ?? null) !== (record.plan_step ?? null);
      });
    if (boardStale) scheduleBoardSync();
    app.update((state) => {
      // A snapshot belongs to the project that requested it: a slow response
      // must never repaint (or reprice) a project the human has since opened.
      if (state.projectId !== projectId) return state;
      const diffWorkerId = state.diffWorkerId ?? state.selectedWorkerId;
      // A worker that left the snapshot must not keep a live activity record.
      for (const id of workerActivity.keys()) {
        if (!snapshot.workers.some((record) => record.id === id)) workerActivity.delete(id);
      }
      return {
        ...state,
        workers: snapshot.workers.map((record, index) => {
          const next = workerFromRecord(record, index, messages);
          workerSaved.set(record.id, {
            generation: record.generation,
            length: record.transcript.messages.length,
            thread: next.messages,
          });
          next.messages = composeThread(record.id) ?? next.messages;
          const previous = state.workers.find((worker) => worker.id === record.id);
          return record.merge ? next : {
            ...next,
            files: previous?.files ?? [],
            diff: previous?.diff ?? next.diff,
          };
        }),
        leadEntries: mergeTeamActivity(state.leadEntries, messages, snapshot.workers),
        teamSlots: snapshot.slots,
        teamActive: snapshot.active,
        projectUsage: projectUsageFrom(snapshot.usage),
        selectedWorkerId: snapshot.workers.some((worker) => worker.id === state.selectedWorkerId)
          ? state.selectedWorkerId
          : snapshot.workers[0]?.id ?? null,
        diffWorkerId: snapshot.workers.some((worker) => worker.id === diffWorkerId)
          ? diffWorkerId
          : snapshot.workers[0]?.id ?? null,
        workerPanelId: snapshot.workers.some((worker) => worker.id === state.workerPanelId)
          ? state.workerPanelId
          : null,
      };
    });
    scheduleDiffSync();
    // A worker that just finished, started waiting for the human, or failed
    // rings the matching sound; a burst across the team collapses to one via
    // the module's debounce window. A snapshot for an already-closed project
    // stays silent (the update above skipped it).
    if (get(app).projectId === projectId) {
      for (const record of snapshot.workers) {
        const sound = workerTransitionSound(before.get(record.id)?.status, record.status);
        if (sound) notifySound(sound);
      }
    }
  } catch (error) {
    if (token === teamToken) noteTeamError(error);
  }
}

export function scheduleTeamSync(): void {
  if (teamSyncTimer) return;
  teamSyncTimer = setTimeout(() => {
    teamSyncTimer = null;
    void syncTeam();
  }, 120);
}

export async function disconnectTeam(): Promise<void> {
  teamToken += 1;
  workerLive.clear();
  workerSaved.clear();
  transcriptCache.clear();
  workerActivity.clear();
  if (teamSyncTimer) {
    clearTimeout(teamSyncTimer);
    teamSyncTimer = null;
  }
  const unlisten = teamUnlisten;
  teamUnlisten = null;
  unlisten?.();
  teamTransport = null;
}

export async function connectTeam(): Promise<void> {
  const transport = createTeamTransport();
  if (!transport) return;
  // Same foreign-host guard as the project path: only the real Mnemo2 host
  // answers the handshake.
  if (!(await probeMnemo2Host())) return;
  await disconnectTeam();
  const token = teamToken;
  teamTransport = transport;
  try {
    const unlisten = await transport.onEvent((event) => {
      if (token !== teamToken) return;
      streamWorkerEvent(event);
      // Streamed content changes no record; only lifecycle events resync.
      if (!STREAM_ONLY_EVENTS.has(String(event.event?.event?.kind ?? ""))) scheduleTeamSync();
    });
    if (token !== teamToken) {
      unlisten();
      return;
    }
    teamUnlisten = unlisten;
    await syncTeam();
  } catch (error) {
    if (token === teamToken) noteTeamError(error);
  }
}
