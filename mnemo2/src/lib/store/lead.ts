import { get } from "svelte/store";
import { createLeadTransport, type LeadApprovalDecision, type LeadTransport } from "../leadEngine";
import {
  applyLeadEvent,
  applyLeadSnapshot,
  initialLeadRuntime,
  leadEventAffectsUsage,
  leadEventNeedsSnapshot,
} from "../leadState";
import { leadEventSound, notifySound } from "../sounds";
import { createStreamCoalescer } from "../streamCoalescer";
import type { LeadAnswerInput, LeadEvent, Project } from "../types";
import { app, leadError, nextId, withToast } from "./core";
import { scheduleTeamSync } from "./team";

let leadTransport: LeadTransport | null = null;
let leadUnlisten: (() => void) | null = null;
let leadSessionToken = 0;

export async function stopNativeLead(): Promise<void> {
  const unlisten = leadUnlisten;
  leadUnlisten = null;
  unlisten?.();
  const transport = leadTransport;
  leadTransport = null;
  if (transport) {
    try {
      await transport.close();
    } catch {
      // Closing a replaced lead is best effort; the next open owns the state.
    }
  }
}

async function refreshNativeLead(token: number): Promise<void> {
  const transport = leadTransport;
  if (!transport) return;
  try {
    const snapshot = await transport.snapshot();
    if (token === leadSessionToken) {
      leadStream.flush();
      app.update((state) => applyLeadSnapshot(state, snapshot));
    }
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => ({
        ...state,
        leadRuntime: { ...state.leadRuntime, error: leadError(error) },
      }));
    }
  }
}

/**
 * Lead events pass through a coalescer: while the window paints they apply one
 * by one, but a backlog from a hidden or throttled window lands in one update
 * (see streamCoalescer.ts). Flush it before any other write to lead state so
 * queued events never apply on top of a newer snapshot.
 */
const leadStream = createStreamCoalescer<{ token: number; event: LeadEvent }>(applyNativeLeadEvents);

function handleNativeLeadEvent(token: number, event: LeadEvent): void {
  if (token !== leadSessionToken) return;
  leadStream.push({ token, event });
}

function applyNativeLeadEvents(items: { token: number; event: LeadEvent }[]): void {
  const token = leadSessionToken;
  const events = items.filter((item) => item.token === token).map((item) => item.event);
  if (events.length === 0) return;
  app.update((state) => events.reduce(applyLeadEvent, state));
  // The lead's own turn-finished / needs-you / stopped-with-errors events ring
  // the matching notification sound (gated by the [ui] toggles, per-sound
  // debounce, more urgent sounds always through).
  const runtime = get(app).leadRuntime;
  let affectsUsage = false;
  let needsSnapshot = false;
  for (const event of events) {
    if (runtime.mode === "native" && runtime.generation === event.generation) {
      const sound = leadEventSound(event.event?.event);
      if (sound) notifySound(sound);
    }
    affectsUsage ||= leadEventAffectsUsage(event);
    needsSnapshot ||= leadEventNeedsSnapshot(event);
  }
  if (affectsUsage) {
    // The durable per-project accumulator changed; re-read the team snapshot
    // (debounced and project-guarded) so the header cost follows the lead too.
    scheduleTeamSync();
  }
  if (needsSnapshot) {
    setTimeout(() => {
      if (token === leadSessionToken) void refreshNativeLead(token);
    }, 25);
  }
}

export async function connectNativeLead(project: Project, allowConflict = false): Promise<void> {
  const token = ++leadSessionToken;
  const transport = createLeadTransport();
  if (!transport) return;
  const available = await transport.probe().catch(() => false);
  if (!available || token !== leadSessionToken) return;
  await stopNativeLead();
  if (token !== leadSessionToken) return;
  leadTransport = transport;
  app.update((state) => ({
    ...state,
    leadRuntime: {
      ...initialLeadRuntime(),
      mode: "native",
      status: "opening",
    },
  }));
  try {
    const unlisten = await transport.onEvent((event) =>
      handleNativeLeadEvent(token, event),
    );
    if (token !== leadSessionToken) {
      unlisten();
      return;
    }
    leadUnlisten = unlisten;
    const snapshot = await transport.open({
      workspaceRoot: project.path,
      ...(allowConflict ? { allowInstanceConflict: true } : {}),
    });
    if (token !== leadSessionToken) return;
    leadStream.flush();
    app.update((state) => applyLeadSnapshot(state, snapshot));
  } catch (error) {
    if (token !== leadSessionToken) return;
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        status: "error",
        error: leadError(error),
      },
    }));
  }
}

/**
 * Invalidate every lead promise and event still in flight: each project open,
 * close, reset, or retry bumps the session token. The token itself stays
 * private to this module; outside callers use this bump instead.
 */
export function bumpLeadSession(): number {
  return ++leadSessionToken;
}

/**
 * Send one submission to the native lead.
 *
 * Resolves `true` only when the host accepted it. A rejected steer keeps the
 * turn's running state (busy, elapsed clock, streaming entry) intact — the
 * engine may still be working — and a rejected idle prompt becomes an error
 * instead of a fabricated idle turn. Either failure rolls back the optimistic
 * transcript entry and surfaces a toast.
 */
async function sendNativeLeadMessage(message: string, images: string[]): Promise<boolean> {
  const transport = leadTransport;
  if (!transport) return false;
  const token = leadSessionToken;
  // The optimistic entry goes after every event the host already sent.
  leadStream.flush();
  const before = get(app).leadRuntime;
  // A submission on a busy turn steers the running turn instead of starting
  // a new one: the elapsed clock and the streaming entry must survive.
  const steering = before.busy;
  const entryId = nextId("user");
  app.update((state) => ({
    ...state,
    leadEntries: [
      ...state.leadEntries,
      {
        id: entryId,
        kind: "message",
        author: "user",
        body: message,
        ...(images.length > 0 ? { images } : {}),
        time: "now",
      },
    ],
    leadRuntime: {
      ...state.leadRuntime,
      status: "ready",
      busy: true,
      error: null,
      errorRetrying: false,
      turnStartedAtMs: steering ? state.leadRuntime.turnStartedAtMs : Date.now(),
      streamingEntryId: steering ? state.leadRuntime.streamingEntryId : null,
    },
  }));
  try {
    await transport.prompt(message, images);
    return true;
  } catch (error) {
    if (token === leadSessionToken) {
      const failure = leadError(error);
      app.update((state) => {
        // Roll back the optimistic entry unless a later turn update followed
        // it, so a rejected submission is never shown as delivered.
        const entries = state.leadEntries.at(-1)?.id === entryId
          ? state.leadEntries.slice(0, -1)
          : state.leadEntries;
        const runtime = steering
          ? // The turn was already running: never fake idle after a rejected
            // steer, and leave busy/clock/streaming exactly as the engine set
            // them (a `finished` event during the await still wins).
            { ...state.leadRuntime, error: null, errorRetrying: false }
          : {
              ...state.leadRuntime,
              status: "error" as const,
              busy: false,
              error: failure,
              errorRetrying: false,
              turnStartedAtMs: before.turnStartedAtMs,
              streamingEntryId: before.streamingEntryId,
            };
        return withToast({ ...state, leadEntries: entries, leadRuntime: runtime }, failure, "error");
      });
    }
    return false;
  }
}

/**
 * Submit text (and optional image data URLs) to the lead.
 *
 * Resolves `true` when the submission was accepted (a prompt when idle, a
 * steer while busy) and `false` when it was refused. The unsent draft is
 * owned by the caller, so a refusal never discards it.
 */
export async function sendLeadMessage(body: string, images: string[] = []): Promise<boolean> {
  const message = body.trim();
  const attachments = images.filter(
    (image) => typeof image === "string" && image.length > 0,
  );
  // An image-only submission is still a real message for a vision model.
  if (!message && attachments.length === 0) return false;
  const state = get(app);
  if (state.leadRuntime.mode === "native") {
    if (!leadTransport) {
      app.update((current) => withToast(current, "The native lead is not connected.", "error"));
      return false;
    }
    if (state.leadRuntime.status !== "ready") {
      app.update((current) => withToast(current, "The lead is still connecting."));
      return false;
    }
    // A pending question or approval owns the turn; answer it first. This is
    // the race-safe gate: a pending interaction that appeared after the
    // composer's own check still refuses the send and keeps the draft.
    if (state.leadRuntime.pending) {
      app.update((current) =>
        withToast(current, "Answer the lead's question before sending."),
      );
      return false;
    }
    return sendNativeLeadMessage(message, attachments);
  }
  // Browser-only Vite sessions have no engine behind them. Keep the typed
  // message, but never fabricate a lead reply.
  app.update((current) => ({
    ...withToast(
      current,
      "This build has no native shell; open Mnemo2's desktop app to run the lead.",
      "error",
    ),
    leadEntries: [
      ...current.leadEntries,
      {
        id: nextId("user"),
        kind: "message",
        author: "user",
        body: message,
        ...(attachments.length > 0 ? { images: attachments } : {}),
        time: "now",
      },
    ],
  }));
  return true;
}

/** Replace the unsent composer text, keeping any attached images. */
export function setLeadDraftText(text: string): void {
  app.update((state) =>
    state.leadDraft.text === text
      ? state
      : { ...state, leadDraft: { ...state.leadDraft, text } },
  );
}

/** Attach a downscaled image data URL to the unsent draft. */
export function addLeadDraftImage(dataUrl: string): void {
  if (!dataUrl) return;
  app.update((state) => ({
    ...state,
    leadDraft: {
      ...state.leadDraft,
      images: [...state.leadDraft.images, dataUrl],
    },
  }));
}

/** Detach one attached image by its position in the draft. */
export function removeLeadDraftImage(index: number): void {
  app.update((state) => {
    if (index < 0 || index >= state.leadDraft.images.length) return state;
    return {
      ...state,
      leadDraft: {
        ...state.leadDraft,
        images: state.leadDraft.images.filter((_, position) => position !== index),
      },
    };
  });
}

/** Drop the whole unsent draft (after a send or on a project switch). */
export function clearLeadDraft(): void {
  app.update((state) =>
    state.leadDraft.text === "" && state.leadDraft.images.length === 0
      ? state
      : { ...state, leadDraft: { text: "", images: [] } },
  );
}

export async function interruptLead(): Promise<void> {
  const transport = leadTransport;
  if (!transport) {
    const error = new Error("The native lead is not connected, so it cannot be interrupted.");
    app.update((state) => withToast(state, error.message, "error"));
    throw error;
  }
  const token = leadSessionToken;
  try {
    await transport.interrupt();
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error), "error"));
    }
    throw error;
  }
}

/** Compact the lead's context window between turns; a failure is toasted. */
export async function compactLead(): Promise<void> {
  const transport = leadTransport;
  if (!transport) {
    app.update((state) =>
      withToast(
        state,
        "The native lead is not connected, so its context cannot be compacted.",
        "error",
      ),
    );
    return;
  }
  const token = leadSessionToken;
  try {
    await transport.compact();
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error), "error"));
    }
  }
}

export function approveLead(requestId: string, decision: LeadApprovalDecision) {
  const transport = leadTransport;
  if (!transport) return;
  const token = leadSessionToken;
  void transport
    .approve(requestId, decision)
    .then(() => refreshNativeLead(token))
    .catch((error) => {
      if (token === leadSessionToken) {
        app.update((state) => withToast(state, leadError(error), "error"));
      }
    });
}

/**
 * Answer the pending interaction.
 *
 * Resolves `true` once the host accepted (and the answer landed), `false`
 * when it was refused. The caller owns the composer draft, so a refusal is
 * toasted here and must leave the typed answer intact.
 */
export async function answerLead(
  questionId: string,
  answer: LeadAnswerInput,
): Promise<boolean> {
  const transport = leadTransport;
  if (!transport) return false;
  const token = leadSessionToken;
  try {
    await transport.answer(questionId, answer);
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error), "error"));
    }
    return false;
  }
  // The answer landed; refresh the accepted snapshot in the background.
  void refreshNativeLead(token);
  return true;
}

export function setLeadModel(spec: string, reasoningEffort: string | null = null) {
  const transport = leadTransport;
  if (!transport) return;
  const state = get(app);
  if (state.leadRuntime.busy) {
    app.update((current) =>
      withToast(current, "Stop the current turn before changing models."),
    );
    return;
  }
  const token = leadSessionToken;
  void transport
    .setModel(spec, reasoningEffort)
    .then((snapshot) => {
      if (token === leadSessionToken) {
        leadStream.flush();
        app.update((current) => applyLeadSnapshot(current, snapshot));
      }
    })
    .catch((error) => {
      if (token === leadSessionToken) {
        app.update((current) => withToast(current, leadError(error), "error"));
      }
    });
}

export function retryLead() {
  const state = get(app);
  const project = state.projects.find((item) => item.id === state.projectId);
  if (!project) return;
  leadSessionToken += 1;
  void stopNativeLead();
  void connectNativeLead(project);
}
