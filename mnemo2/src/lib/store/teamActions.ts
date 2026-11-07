import { get } from "svelte/store";
import { type TeamTransport, type WorkerDeletionProgress } from "../teamEngine";
import {
  deletionDiscardsWork,
  deletionPreviewFromWire,
  findingToWire,
  projectUsageFrom,
  reviewEvidenceToWire,
  workerDisplayName,
} from "../teamState";
import type { Finding, ReviewEvidence, Worker, WorkerStatus } from "../types";
import { app, leadError, withToast } from "./core";
import { noteTeamError, noteWorkerModelSwitch, requireTeam, syncTeam } from "./team";

/**
 * Send one message to a worker. Resolves `true` only when the backend accepted
 * it, so the caller keeps the text when the send is refused.
 *
 * `requireTeam` runs before the first await: a missing runtime reports its
 * toast synchronously, exactly as the previous fire-and-forget call did.
 */
export async function replyWorker(workerId: string, body: string): Promise<boolean> {
  const message = body.trim();
  if (!message) return false;
  const transport = requireTeam();
  if (!transport) return false;
  try {
    await transport.message(workerId, message);
  } catch (error) {
    noteTeamError(error);
    return false;
  }
  void syncTeam().catch(noteTeamError);
  return true;
}

export function stopWorker(workerId: string) {
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .stop(workerId)
    .then(() => syncTeam())
    .catch(noteTeamError);
}

/** Statuses of workers whose task is over — what "Delete old workers" offers. */
export const OLD_WORKER_STATUSES: WorkerStatus[] = ["merged", "cancelled", "stopped", "interrupted", "failed"];

/**
 * Whether "Delete old workers" offers this worker: its task is over. That
 * is any terminal status, plus finished audit workers — read-only reviewers
 * are never merged, so they end in `completed`. A `completed` implementation
 * worker still awaits review and is never offered here.
 */
export function isOldWorker(worker: Pick<Worker, "status" | "kind">): boolean {
  return OLD_WORKER_STATUSES.includes(worker.status) ||
    (worker.kind === "audit" && worker.status === "completed");
}

/** A token that invalidates promises and events of superseded deletion
 * flows: each open request bumps it, so a canceled or reopened sheet never
 * applies a stale result. */
let deletionFlow = 0;

/** The streamed per-worker deletion progress listener; null while none is
 * subscribed. One listener serves the open request and is removed when
 * the sheet closes. */
let deletionUnlisten: (() => void) | null = null;

/** Merge one streamed deletion update into the open sheet's rows. */
function applyDeletionProgress(event: WorkerDeletionProgress): void {
  app.update((state) => {
    const request = state.workerDeletion;
    if (!request) return state;
    return {
      ...state,
      workerDeletion: {
        ...request,
        rows: request.rows.map((row) => {
          if (row.workerId !== event.worker_id) return row;
          return {
            ...row,
            name: event.name || row.name,
            preview: event.preview
              ? deletionPreviewFromWire(event.preview)
              : row.preview,
            error: event.error ?? row.error,
            deleted: event.phase === "deleting" && event.error === null,
          };
        }),
      },
    };
  });
}

/** Close the sheet, invalidating anything its flow still has in flight. */
function closeDeletionSheet(): void {
  deletionFlow += 1;
  deletionUnlisten?.();
  deletionUnlisten = null;
  app.update((state) => (state.workerDeletion ? { ...state, workerDeletion: null } : state));
}

/**
 * Open the deletion sheet at once: rows start in a checking state and fill
 * in per worker as the host streams finished previews
 * (`mnemo2://worker-deletion`), so slow Git checks never delay the
 * dialog. A request that is already open is ignored — the button cannot
 * open the sheet twice.
 */
export async function requestDeleteWorkers(workerIds: string[]): Promise<void> {
  const transport = requireTeam();
  if (!transport || workerIds.length === 0) return;
  if (get(app).workerDeletion) return;
  const ids = [...new Set(workerIds)];
  const workers = get(app).workers;
  app.update((state) => ({
    ...state,
    workerDeletion: {
      phase: "checking",
      rows: ids.map((workerId) => ({
        workerId,
        name: workers.find((worker) => worker.id === workerId)?.name ?? workerId,
        preview: null,
        error: null,
        deleted: false,
      })),
    },
  }));
  const flow = ++deletionFlow;
  deletionUnlisten?.();
  deletionUnlisten = transport.onDeletionEvent
    ? await transport.onDeletionEvent(applyDeletionProgress)
    : null;
  try {
    const previews = (await transport.deletePreview(ids)).map(deletionPreviewFromWire);
    if (flow !== deletionFlow) return;
    app.update((state) => {
      const request = state.workerDeletion;
      if (!request) return state;
      return {
        ...state,
        workerDeletion: {
          ...request,
          rows: request.rows.map((row) => ({
            ...row,
            preview:
              previews.find((preview) => preview.worker_id === row.workerId) ?? row.preview,
          })),
          phase: "ready",
        },
      };
    });
  } catch (error) {
    if (flow !== deletionFlow) return;
    closeDeletionSheet();
    noteTeamError(error);
  }
}

/** Offer to delete every worker whose task is over. */
export async function requestDeleteOldWorkers(): Promise<void> {
  const ids = get(app).workers.filter(isOldWorker).map((worker) => worker.id);
  if (ids.length === 0) {
    app.update((state) => withToast(state, "There are no old workers to delete."));
    return;
  }
  await requestDeleteWorkers(ids);
}

export function cancelDeleteWorkers(): void {
  const request = get(app).workerDeletion;
  if (!request || request.phase === "deleting") return;
  closeDeletionSheet();
}

/**
 * Delete the confirmed workers; blocked ones are left in place. Results
 * stream in per worker; when some fail the sheet stays open with a retry
 * for exactly those workers, and closes only once nothing failed.
 */
export async function confirmDeleteWorkers(): Promise<void> {
  const transport = requireTeam();
  const request = get(app).workerDeletion;
  if (!transport || !request || request.phase !== "ready") return;
  // After a failed round, retry only the workers that failed.
  const retrying = request.rows.some((row) => row.error !== null && !row.deleted);
  const rows = retrying
    ? request.rows.filter((row) => row.error !== null && !row.deleted)
    : request.rows.filter((row) => row.preview && row.preview.blocked === null);
  if (rows.length === 0) return;
  const discardWork = rows.some((row) => row.preview && deletionDiscardsWork(row.preview));
  const flow = deletionFlow;
  app.update((state) =>
    state.workerDeletion
      ? { ...state, workerDeletion: { ...state.workerDeletion, phase: "deleting" } }
      : state,
  );
  try {
    const outcome = await transport.deleteWorkers(
      rows.map((row) => row.workerId),
      discardWork,
    );
    if (flow !== deletionFlow) return;
    app.update((state) => {
      const current = state.workerDeletion;
      if (!current || current.phase !== "deleting") return state;
      const nextRows = current.rows.map((row) => {
        const result = outcome.results.find((entry) => entry.worker_id === row.workerId);
        if (!result) return row;
        return {
          ...row,
          name: result.name || row.name,
          error: result.error ?? row.error,
          deleted: result.error === null,
        };
      });
      const failed = nextRows.filter((row) => row.error !== null && !row.deleted);
      if (failed.length === 0) {
        return withToast(
          { ...state, workerDeletion: null },
          `Deleted ${rows.length} worker${rows.length === 1 ? "" : "s"} and ${rows.length === 1 ? "its" : "their"} history.`,
        );
      }
      // Keep the sheet open: the failed rows show their errors, with a
      // retry for exactly those workers.
      return { ...state, workerDeletion: { phase: "ready", rows: nextRows } };
    });
  } catch (error) {
    if (flow !== deletionFlow) return;
    // The whole command failed: keep the sheet so the user can retry.
    app.update((state) =>
      withToast(
        state.workerDeletion
          ? { ...state, workerDeletion: { ...state.workerDeletion, phase: "ready" } }
          : state,
        `Delete failed: ${leadError(error)}`,
        "error",
      ),
    );
  }
  await syncTeam();
  if (!get(app).workerDeletion) closeDeletionSheet();
}

export function pauseWorker(workerId: string) {
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .pause(workerId)
    .then(() => syncTeam())
    .catch(noteTeamError);
}

export function resumeWorker(workerId: string) {
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .resume(workerId)
    .then(() => syncTeam())
    .catch(noteTeamError);
}

/**
 * Move a worker to another model. A worker mid-run (working, dispatching,
 * fixing) keeps its session running: the switch applies to its next request
 * and its thread notes the change; every other worker comes back idle
 * with its transcript, as before.
 */
export async function setWorkerModel(workerId: string, model: string, reasoningEffort: string | null): Promise<void> {
  const transport = requireTeam();
  if (!transport) return;
  let switched = false;
  try {
    const record = await transport.setModel(workerId, model, reasoningEffort);
    // A still-running record means the switch was applied live — the request
    // in flight keeps serving on the old model.
    switched = ["working", "dispatching", "fixing"].includes(record.status);
    app.update((state) => withToast(
      state,
      switched
        ? `${workerDisplayName(record.id, record.name)} keeps working; it runs on ${model} from its next call.`
        : `${workerDisplayName(record.id, record.name)} now runs on ${model}. Message it to continue.`,
    ));
  } catch (error) {
    noteTeamError(error);
  }
  await syncTeam();
  if (switched) noteWorkerModelSwitch(workerId, model);
}

export async function compactWorker(workerId: string): Promise<void> {
  await performTeam((transport) => transport.compact(workerId));
}

async function performTeam(
  operation: (transport: TeamTransport) => Promise<unknown>,
): Promise<void> {
  const transport = requireTeam();
  if (!transport) return;
  try {
    await operation(transport);
    await syncTeam();
  } catch (error) {
    noteTeamError(error);
  }
}

export async function reviewWorker(
  workerId: string,
  summary: string,
  findings: Finding[],
  evidence: ReviewEvidence,
): Promise<void> {
  await performTeam((transport) =>
    transport.review(workerId, summary, findings.map(findingToWire), reviewEvidenceToWire(evidence)),
  );
}

export async function recordWorkerCheck(
  workerId: string,
  name: string,
  status: "passed" | "failed",
  detail?: string | null,
): Promise<void> {
  await performTeam((transport) =>
    transport.recordCheck(workerId, name, status, detail ?? null),
  );
}

export async function resolveFinding(
  workerId: string,
  findingId: string,
  status: "fixed" | "wontfix",
  evidence?: string | null,
  justification?: string | null,
): Promise<void> {
  await performTeam((transport) =>
    transport.resolveFinding(
      workerId,
      findingId,
      status,
      evidence ?? null,
      justification ?? null,
    ),
  );
}

export async function fixFinding(
  workerId: string,
  findingId: string,
  oldString: string,
  newString: string,
): Promise<void> {
  const finding = get(app).workers
    .find((worker) => worker.id === workerId)
    ?.findings.find((item) => item.id === findingId);
  if (!finding) {
    app.update((state) => withToast(state, `Unknown finding '${findingId}'.`, "error"));
    return;
  }
  await performTeam((transport) =>
    transport.reviewFix(workerId, findingId, finding.file, oldString, newString),
  );
}

export async function spawnFixer(workerId: string, findingId: string): Promise<void> {
  await performTeam((transport) => transport.routeFix(workerId, findingId));
}

export async function wontfixFinding(
  workerId: string,
  findingId: string,
  justification: string,
): Promise<void> {
  await resolveFinding(workerId, findingId, "wontfix", null, justification);
}

export async function mergeWorker(workerId: string): Promise<void> {
  await performTeam((transport) => transport.merge(workerId));
}

export async function revertMerge(workerId: string): Promise<void> {
  await performTeam((transport) => transport.revertMerge(workerId));
}

export async function stopAll(): Promise<void> {
  await performTeam((transport) => transport.stopAll());
}

export function setWorkerSlots(workerSlots: number): void {
  if (!Number.isInteger(workerSlots) || workerSlots < 1 || workerSlots > 32) {
    app.update((state) => withToast(state, "Worker slots must be a whole number from 1 to 32.", "error"));
    return;
  }
  const transport = requireTeam();
  if (!transport) return;
  const projectId = get(app).projectId;
  void transport.setSlots(workerSlots).then((snapshot) => {
    app.update((state) => {
      if (state.projectId !== projectId) return state;
      return {
        ...state,
        teamSlots: snapshot.slots,
        teamActive: snapshot.active,
        projectUsage: projectUsageFrom(snapshot.usage),
        projects: state.projects.map((project) => project.id === state.projectId
          ? { ...project, workerSlots: snapshot.slots }
          : project),
      };
    });
  }).catch(noteTeamError);
}
