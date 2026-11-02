import { shortModelLabel } from "./cost";
import { messageText, normalizeToolResult } from "./leadState";
import type { TeamEvent, TeamMessage, TeamWorkerRecord } from "./teamState";
import type {
  LeadEntry,
  LeadTranscriptMessage,
  WorkerModelSwitchEntry,
  WorkerThreadEntry,
} from "./types";

type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

/** The engine's cache-stable request footer (`CONTEXT_FOOTER`). */
export const CONTEXT_FOOTER = "<context footer — cache-stable sentinel, ignore>";

/** The worker's report-to-lead host tool; its text renders as a message. */
const MESSAGE_PARENT = "message_parent";

/** Live output kept per running tool, like the lead's reducer. */
const LIVE_OUTPUT_CAP = 16 * 1024;

function isFooter(message: LeadTranscriptMessage | undefined): boolean {
  return message !== undefined && messageText(message.content).trim() === CONTEXT_FOOTER;
}

/**
 * Whether `messages[index]` is request scaffolding an older engine leaked
 * into a saved transcript: the context footer or the volatile tail before it.
 */
export function isRequestScaffolding(messages: LeadTranscriptMessage[], index: number): boolean {
  return isFooter(messages[index]) || (index > 0 && isFooter(messages[index + 1]));
}

function messageParentText(argumentsText: string): string | null {
  try {
    const parsed: unknown = JSON.parse(argumentsText);
    if (parsed && typeof parsed === "object" && typeof (parsed as { text?: unknown }).text === "string") {
      return (parsed as { text: string }).text;
    }
  } catch {
    // Arguments still streaming or malformed: not a finished report yet.
  }
  return null;
}

function authorOf(role: string, body: string): "lead" | "worker" | "harness" {
  if (role === "assistant") return "worker";
  return body.startsWith("[harness") ? "harness" : "lead";
}

/**
 * A worker's saved conversation as a thread: the lead's brief and replies,
 * the worker's Markdown, its reports to the lead, and every tool it ran.
 * System prompts and leaked request scaffolding are left out.
 */
export function workerThreadFromRecord(
  record: TeamWorkerRecord,
  inbox: TeamMessage[] = [],
): WorkerThreadEntry[] {
  const messages = record.transcript.messages;
  const entries: WorkerThreadEntry[] = [];
  const tools = new Map<string, number>();
  const reports = new Set<string>();
  messages.forEach((message, index) => {
    if (isRequestScaffolding(messages, index)) return;
    if (message.role === "user" || message.role === "assistant") {
      const body = messageText(message.content).trim();
      if (body) {
        entries.push({
          id: `worker-${record.id}-${index}`,
          from: authorOf(message.role, body),
          body,
          time: "restored",
        });
      }
      if (message.role !== "assistant") return;
      (message.tool_calls ?? []).forEach((call, callIndex) => {
        const callId = call?.id || `worker-${record.id}-${index}-call-${callIndex}`;
        const argumentsText = typeof call?.arguments === "string" ? call.arguments : "";
        if (call?.name === MESSAGE_PARENT) {
          const text = messageParentText(argumentsText);
          reports.add(callId);
          if (text) {
            entries.push({ id: `worker-${record.id}-report-${callId}`, from: "worker", body: text, time: "restored" });
          }
          return;
        }
        entries.push({
          id: `worker-${record.id}-tool-${index}-${callIndex}`,
          kind: "tool",
          time: "restored",
          toolCallId: callId,
          toolName: call?.name || "tool",
          arguments: argumentsText,
          liveOutput: "",
          result: null,
        });
        tools.set(callId, entries.length - 1);
      });
      return;
    }
    if (message.role !== "tool") return;
    const callId = typeof message.tool_call_id === "string" ? message.tool_call_id : "";
    if (reports.has(callId)) return;
    const at = tools.get(callId);
    const entry = at === undefined ? undefined : entries[at];
    if (entry?.kind === "tool") {
      entries[at!] = {
        ...entry,
        result: normalizeToolResult(messageText(message.content), message.tool_is_error !== true),
      };
    }
  });
  // Reports kept only in the live inbox (the transcript is saved per turn).
  const shown = new Set(entries.flatMap((entry) => (entry.kind === "tool" ? [] : [entry.body.trim()])));
  for (const message of inbox) {
    if (message.worker_id !== record.id || shown.has(message.text.trim())) continue;
    entries.push({
      id: `team-${message.worker_id}-${message.at_ms}`,
      from: "worker",
      body: message.text,
      time: new Date(message.at_ms).toLocaleTimeString(),
    });
  }
  return entries;
}

/** A worker's in-flight activity since its transcript was last saved. */
export interface WorkerLive {
  generation: number;
  /** The saved transcript length these live entries extend. */
  baseLength: number;
  entries: WorkerThreadEntry[];
  /** The worker message currently receiving streamed text. */
  streamingId: string | null;
  sequence: number;
}

export function emptyWorkerLive(generation: number, baseLength: number): WorkerLive {
  return { generation, baseLength, entries: [], streamingId: null, sequence: 0 };
}

function updateTool(live: WorkerLive, match: (entry: ToolEntry) => boolean, update: (entry: ToolEntry) => WorkerThreadEntry): WorkerLive {
  let index = -1;
  for (let at = live.entries.length - 1; at >= 0; at -= 1) {
    const entry = live.entries[at];
    if (entry.kind === "tool" && match(entry)) {
      index = at;
      break;
    }
  }
  if (index === -1) return live;
  const entries = [...live.entries];
  entries[index] = update(entries[index] as ToolEntry);
  return { ...live, entries };
}

/** The error text a provider or runtime failure reported, with the lead's wording. */
function errorTextOf(payload: Record<string, unknown>): string {
  if (typeof payload.error === "string" && payload.error) return payload.error;
  if (typeof payload.message === "string" && payload.message) return payload.message;
  return "The worker stopped with an error.";
}

/** Drop the transient retrying-error note: with the stream resumed it no longer applies. */
function dropRetryNotes(live: WorkerLive): WorkerLive {
  if (!live.entries.some((entry) => entry.kind === "error" && entry.retrying)) return live;
  return {
    ...live,
    entries: live.entries.filter((entry) => !(entry.kind === "error" && entry.retrying)),
  };
}

/** The muted note a user's live model switch leaves in the worker's thread. */
export function modelSwitchNoteBody(model: string): string {
  return `Model changed to ${shortModelLabel(model)} by you; it takes effect on the next call`;
}

/** The note's text once the switched model actually serves a request. */
export function modelSwitchedBody(model: string): string {
  return `Now running on ${shortModelLabel(model)}`;
}

/**
 * The muted note a completed deferred switch leaves in the worker's thread: the
 * OLD model summarized the conversation so it fits the new, smaller window.
 * Informational — the switch succeeded and the turn continues (bug 2026-11-02,
 * plan 1238e0de).
 */
export function modelSwapSummarizedBody(model: string): string {
  return `Summarized the conversation to fit ${shortModelLabel(model)}`;
}

/**
 * Record a live model switch as a muted note at the end of the worker's
 * thread. The request in flight still serves on the old model, so the note
 * stays pending until the kernel's model_changed event reports the new one
 * serving; a newer switch replaces any still-pending note.
 */
export function applyModelSwitch(live: WorkerLive, model: string): WorkerLive {
  const entries = live.entries.filter((entry) => !(entry.kind === "model_switch" && !entry.applied));
  return {
    ...live,
    sequence: live.sequence + 1,
    entries: [
      ...entries,
      {
        id: `model-switch-${live.sequence}`,
        kind: "model_switch",
        time: "now",
        model,
        body: modelSwitchNoteBody(model),
        applied: false,
      },
    ],
  };
}

/** Fold one worker event into its live thread tail. */
export function applyWorkerLiveEvent(live: WorkerLive, event: TeamEvent): WorkerLive {
  const payload = event.event?.event as Record<string, unknown> | undefined;
  if (!payload) return live;
  // A retrying provider error holds a transient note at the thread's end; any
  // other event — streamed text, a tool, the turn ending — means the stream
  // resumed, so the note goes away before the event itself is folded in.
  if (payload.kind !== "error") live = dropRetryNotes(live);
  switch (payload.kind) {
    case "started":
    case "finished":
    case "exited":
      return { ...live, streamingId: null };
    case "text_delta": {
      const text = typeof payload.text === "string" ? payload.text : "";
      if (!text) return live;
      const streaming = live.streamingId
        ? live.entries.find((entry) => entry.id === live.streamingId)
        : undefined;
      if (streaming && streaming.kind !== "tool") {
        return {
          ...live,
          entries: live.entries.map((entry) =>
            entry.id === live.streamingId && entry.kind !== "tool" ? { ...entry, body: entry.body + text } : entry,
          ),
        };
      }
      const id = `live-${event.worker_id}-${event.generation}-${live.sequence}`;
      return {
        ...live,
        sequence: live.sequence + 1,
        streamingId: id,
        entries: [...live.entries, { id, from: "worker", body: text, time: "now" }],
      };
    }
    case "tool_call_start": {
      const callId = typeof payload.id === "string" ? payload.id : "";
      if (!callId) return live;
      return {
        ...live,
        streamingId: null,
        sequence: live.sequence + 1,
        entries: [
          ...live.entries,
          {
            id: `live-tool-${event.worker_id}-${event.generation}-${live.sequence}`,
            kind: "tool",
            time: "now",
            toolCallId: callId,
            toolName: typeof payload.name === "string" ? payload.name : "tool",
            arguments: "",
            liveOutput: "",
            streamIndex: typeof payload.index === "number" ? payload.index : undefined,
            startedAtMs: Date.now(),
            result: null,
          },
        ],
      };
    }
    case "tool_call_arg_delta": {
      if (typeof payload.index !== "number" || typeof payload.fragment !== "string") return live;
      const fragment = payload.fragment;
      return updateTool(
        live,
        (entry) => entry.streamIndex === payload.index && entry.result === null,
        (entry) => ({ ...entry, arguments: entry.arguments + fragment }),
      );
    }
    case "tool_execution_start": {
      const callId = typeof payload.tool_call_id === "string" ? payload.tool_call_id : "";
      if (!callId) return live;
      return updateTool(
        live,
        (entry) => entry.toolCallId === callId && entry.result === null,
        (entry) => ({ ...entry, startedAtMs: Date.now() }),
      );
    }
    case "tool_output_delta": {
      const callId = typeof payload.tool_call_id === "string" ? payload.tool_call_id : "";
      if (!callId || typeof payload.text !== "string") return live;
      const text = payload.text;
      return updateTool(
        live,
        (entry) => entry.toolCallId === callId,
        (entry) => ({ ...entry, liveOutput: (entry.liveOutput + text).slice(-LIVE_OUTPUT_CAP) }),
      );
    }
    case "host_tool_call": {
      const callId = typeof payload.call_id === "string" ? payload.call_id : "";
      if (payload.name !== MESSAGE_PARENT || !callId) return live;
      const args = payload.args as { text?: unknown } | undefined;
      const text = typeof args?.text === "string" ? args.text : "";
      // A report to the lead reads as the worker's message, not a tool card.
      return updateTool(
        live,
        (entry) => entry.toolCallId === callId,
        (entry) => ({ id: entry.id, from: "worker", body: text, time: "now" }),
      );
    }
    case "tool_result": {
      const callId = typeof payload.tool_call_id === "string" ? payload.tool_call_id : "";
      if (!callId) return live;
      return updateTool(
        live,
        (entry) => entry.toolCallId === callId,
        (entry) => ({
          ...entry,
          liveOutput: "",
          endedAtMs: Date.now(),
          result: normalizeToolResult(payload.result),
        }),
      );
    }
    case "error": {
      const retrying = payload.retrying === true;
      // A newer provider error replaces the transient retrying note — the
      // thread never holds two notes at once.
      const base = dropRetryNotes(live);
      return {
        ...base,
        sequence: base.sequence + 1,
        // Retrying keeps the half-written message open so resumed text
        // continues it; a final error closes the stream for good.
        streamingId: retrying ? live.streamingId : null,
        entries: [
          ...base.entries,
          {
            id: `live-error-${event.worker_id}-${event.generation}-${base.sequence}`,
            kind: "error",
            time: "now",
            body: errorTextOf(payload),
            retrying,
          },
        ],
      };
    }
    case "model_changed": {
      const model = typeof payload.model === "string" ? payload.model : "";
      if (!model) return live;
      // The switched model serves its first request: the pending note's
      // wait is over, so from here on it reads "Now running on …".
      const pending = live.entries.find(
        (entry): entry is WorkerModelSwitchEntry =>
          entry.kind === "model_switch" &&
          !entry.applied &&
          shortModelLabel(entry.model) === shortModelLabel(model),
      );
      if (!pending) return live;
      return {
        ...live,
        entries: live.entries.map((entry) =>
          entry === pending ? { ...pending, applied: true, body: modelSwitchedBody(pending.model) } : entry,
        ),
      };
    }
    case "model_swap_summarized": {
      // The deferred compact-then-switch completed: the conversation was
      // summarized to fit the new, smaller window and the swap landed. A muted
      // note beside the model-switch note — never an error entry (bug
      // 2026-11-02, plan 1238e0de). `applied: true` keeps it out of the
      // pending-switch matcher above: it reports a completed summarization, not
      // a switch still waiting to serve.
      const model = typeof payload.model === "string" ? payload.model : "";
      if (!model) return live;
      return {
        ...live,
        sequence: live.sequence + 1,
        entries: [
          ...live.entries,
          {
            id: `model-swap-summarized-${event.worker_id}-${event.generation}-${live.sequence}`,
            kind: "model_switch",
            time: "now",
            model,
            body: modelSwapSummarizedBody(model),
            applied: true,
          },
        ],
      };
    }
    default:
      return live;
  }
}

/** Event kinds that only stream content; they never change a worker record. */
export const STREAM_ONLY_EVENTS = new Set([
  "text_delta",
  "reasoning_delta",
  "tool_call_arg_delta",
  "tool_execution_start",
  "tool_output_delta",
  "model_changed",
  "model_swap_summarized",
]);
