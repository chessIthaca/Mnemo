import { messageText, normalizeToolResult } from "./leadState";
import type { TeamEvent, TeamMessage, TeamWorkerRecord } from "./teamState";
import type { LeadEntry, LeadTranscriptMessage, WorkerThreadEntry } from "./types";

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
  if (!entries.some((entry) => entry.kind !== "tool" && entry.from === "lead") && record.brief.trim()) {
    // The brief reaches the transcript only when the first turn is saved.
    entries.unshift({ id: `worker-${record.id}-brief`, from: "lead", body: record.brief, time: "dispatched" });
  }
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

/** Fold one worker event into its live thread tail. */
export function applyWorkerLiveEvent(live: WorkerLive, event: TeamEvent): WorkerLive {
  const payload = event.event?.event as Record<string, unknown> | undefined;
  if (!payload) return live;
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
]);
