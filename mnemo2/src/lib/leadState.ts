import type {
  AppState,
  LeadEntry,
  LeadEvent,
  LeadPendingInteraction,
  LeadRuntimeInfo,
  LeadSnapshot,
  LeadToolResult,
  LeadTranscript,
} from "./types";
import { keepActivityCards } from "./teamActivity";

export const TEAM_NOTICE_PREFIX = "[TeamManager notice — not the human] worker_id=";

/** The idle browser-mode lead state. */
export function initialLeadRuntime(): LeadRuntimeInfo {
  return {
    mode: "mock",
    status: "idle",
    generation: 0,
    busy: false,
    error: null,
    errorRetrying: false,
    model: null,
    modelId: null,
    reasoningEffort: null,
    models: [],
    defaultModel: null,
    pending: null,
    usedTokens: 0,
    maxTokens: 0,
    contextBreakdown: null,
    compacting: false,
    turnStartedAtMs: null,
    streamingEntryId: null,
  };
}

export function messageText(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((block) => {
        if (typeof block === "string") return block;
        if (block && typeof block === "object" && "text" in block) {
          const text = (block as { text?: unknown }).text;
          return typeof text === "string" ? text : "";
        }
        return "";
      })
      .join("");
  }
  if (content && typeof content === "object" && "text" in content) {
    const text = (content as { text?: unknown }).text;
    return typeof text === "string" ? text : "";
  }
  return "";
}

/**
 * Extract the image data URLs from a transcript message's content blocks.
 *
 * The headless transcript serializes user attachments as provider-neutral
 * `image_url` blocks; Anthropic-style base64 sources and legacy `data_url`
 * blocks are accepted too so a restored conversation keeps every image.
 * Only local `data:image/...` URLs are returned: a restored transcript is
 * untrusted input, and an `https://` block would otherwise make the renderer
 * fetch a remote URL the user never sent.
 */
export function messageImages(content: unknown): string[] {
  if (!Array.isArray(content)) return [];
  const local = (url: unknown): string | null =>
    typeof url === "string" && url.startsWith("data:image/") ? url : null;
  const images: string[] = [];
  for (const block of content) {
    if (!block || typeof block !== "object") continue;
    const record = block as Record<string, unknown>;
    if (record.type === "image_url") {
      const imageUrl = record.image_url;
      const url =
        typeof imageUrl === "string"
          ? imageUrl
          : imageUrl && typeof imageUrl === "object"
            ? (imageUrl as { url?: unknown }).url
            : record.url;
      const accepted = local(url);
      if (accepted) images.push(accepted);
      continue;
    }
    if (record.type === "data_url") {
      const accepted = local(record.data_url ?? record.url);
      if (accepted) images.push(accepted);
      continue;
    }
    if (record.type !== "image") continue;
    const direct = local(record.data_url ?? record.url);
    if (direct) {
      images.push(direct);
      continue;
    }
    const source = record.source;
    if (!source || typeof source !== "object") continue;
    const { type, media_type: mediaType, data } = source as {
      type?: unknown;
      media_type?: unknown;
      data?: unknown;
    };
    if (
      type === "base64" &&
      typeof mediaType === "string" &&
      mediaType.startsWith("image/") &&
      typeof data === "string"
    ) {
      images.push(`data:${mediaType};base64,${data}`);
    }
  }
  return images;
}

/** Live output kept per running tool; the final result carries the full text. */
const LIVE_OUTPUT_CAP = 16 * 1024;

/** Serialize a host tool's structured args for the compact arguments pane. */
function toolArgumentsText(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined || value === null) return "";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

/**
 * Normalize a live or transcript tool result. `fallbackSuccess` applies only
 * when the payload carries no boolean verdict (a transcript tool message with
 * `tool_is_error` absent reads as success).
 */
export function normalizeToolResult(value: unknown, fallbackSuccess = false): LeadToolResult {
  const record =
    value && typeof value === "object"
      ? (value as { success?: unknown; output?: unknown; data?: unknown })
      : null;
  const success =
    typeof record?.success === "boolean" ? record.success : fallbackSuccess;
  const rawOutput = record ? record.output : value;
  let output = "";
  if (typeof rawOutput === "string") {
    output = rawOutput;
  } else if (rawOutput !== undefined && rawOutput !== null) {
    try {
      output = JSON.stringify(rawOutput);
    } catch {
      output = String(rawOutput);
    }
  }
  const result: LeadToolResult = { success, output };
  if (record && record.data !== undefined) result.data = record.data;
  return result;
}

/** Bound a running tool's live tail and avoid a split surrogate at the cut. */
function appendLiveOutputTail(previous: string, text: string): string {
  const joined = previous + text;
  if (joined.length <= LIVE_OUTPUT_CAP) return joined;
  const kept = joined.slice(-LIVE_OUTPUT_CAP);
  const first = kept.charCodeAt(0);
  return first >= 0xdc00 && first <= 0xdfff ? kept.slice(1) : kept;
}

/** The interaction id, whichever field the event or snapshot used. */
function pendingId(pending: LeadPendingInteraction): unknown {
  return pending.question_id ?? pending.request_id ?? pending.call_id ?? pending.hook_id ?? pending.tool_call_id;
}

/** Whether two pending records describe the same live interaction. */
function samePendingInteraction(a: LeadPendingInteraction, b: LeadPendingInteraction): boolean {
  const id = pendingId(b);
  return a.kind === b.kind && id !== undefined && pendingId(a) === id;
}

/** Clear only the approval/host-tool pending state owned by this call. */
function clearPendingForCall(
  pending: LeadPendingInteraction | null,
  callId: string,
): LeadPendingInteraction | null {
  if (!pending) return null;
  if (
    (pending.kind === "approval" || pending.kind === "host_tool_call") &&
    (pending.request_id === callId || pending.tool_call_id === callId)
  ) {
    return null;
  }
  return pending;
}

/** Start or refresh a real tool entry, keeping its position in the thread. */
function startToolEntry(
  state: AppState,
  generation: number,
  call: {
    id: string;
    name: string;
    streamIndex?: number;
    argumentsText?: string;
  },
): AppState {
  let found = false;
  const leadEntries = state.leadEntries.map((entry) => {
    if (entry.kind !== "tool" || entry.toolCallId !== call.id) return entry;
    found = true;
    if (entry.result !== null) {
      return { ...entry, toolName: call.name || entry.toolName };
    }
    return {
      ...entry,
      toolName: call.name || entry.toolName,
      arguments: call.argumentsText ?? entry.arguments,
      startedAtMs: entry.startedAtMs ?? Date.now(),
      ...(call.streamIndex !== undefined ? { streamIndex: call.streamIndex } : {}),
    };
  });
  if (!found) {
    leadEntries.push({
      id: `lead-tool-${generation}-${call.id}`,
      kind: "tool",
      time: "now",
      toolCallId: call.id,
      toolName: call.name || "tool",
      arguments: call.argumentsText ?? "",
      liveOutput: "",
      startedAtMs: Date.now(),
      ...(call.streamIndex !== undefined ? { streamIndex: call.streamIndex } : {}),
      result: null,
    });
  }
  return {
    ...state,
    leadEntries,
    leadRuntime: { ...state.leadRuntime, streamingEntryId: null },
  };
}

/** Append one streamed argument fragment to the matching running call. */
function appendToolArguments(state: AppState, index: number, fragment: string): AppState {
  if (fragment.length === 0) return state;
  let changed = false;
  const leadEntries = state.leadEntries.map((entry) => {
    if (entry.kind !== "tool" || entry.result !== null || entry.streamIndex !== index) {
      return entry;
    }
    changed = true;
    return { ...entry, arguments: entry.arguments + fragment };
  });
  return changed ? { ...state, leadEntries } : state;
}

/** Append live output to the matching RUNNING call; late chunks are ignored. */
function appendToolOutput(state: AppState, callId: string, text: string): AppState {
  if (!callId || text.length === 0) return state;
  let changed = false;
  const leadEntries = state.leadEntries.map((entry) => {
    if (entry.kind !== "tool" || entry.toolCallId !== callId || entry.result !== null) {
      return entry;
    }
    changed = true;
    return { ...entry, liveOutput: appendLiveOutputTail(entry.liveOutput, text) };
  });
  return changed ? { ...state, leadEntries } : state;
}

/**
 * Restart the matching RUNNING call's clock at its actual execution start.
 *
 * A batch of calls is announced while the model streams, but the engine runs
 * them one at a time: only now does this call's own clock begin, so a later
 * card no longer inherits the queue time of the calls before it. A completed
 * call keeps its measured span — a trailing event must never reopen it — and
 * an unknown call id is ignored because the announcement owns card creation.
 */
function startToolExecution(state: AppState, callId: string): AppState {
  let changed = false;
  const leadEntries = state.leadEntries.map((entry) => {
    if (entry.kind !== "tool" || entry.toolCallId !== callId || entry.result !== null) {
      return entry;
    }
    changed = true;
    return { ...entry, startedAtMs: Date.now() };
  });
  return changed ? { ...state, leadEntries } : state;
}

/** Finalize a real call; an unseen call still gets a result-only entry. */
function finishToolEntry(
  state: AppState,
  generation: number,
  callId: string,
  result: LeadToolResult,
  toolName: string,
): AppState {
  let found = false;
  const leadEntries = state.leadEntries.map((entry) => {
    if (entry.kind !== "tool" || entry.toolCallId !== callId) return entry;
    found = true;
    return {
      ...entry,
      result,
      liveOutput: "",
      endedAtMs: entry.endedAtMs ?? Date.now(),
    };
  });
  if (!found) {
    leadEntries.push({
      id: `lead-tool-result-${generation}-${callId}`,
      kind: "tool",
      time: "now",
      toolCallId: callId,
      toolName,
      arguments: "",
      liveOutput: "",
      result,
    });
  }
  return {
    ...state,
    leadEntries,
    leadRuntime: {
      ...state.leadRuntime,
      streamingEntryId: null,
      pending: clearPendingForCall(state.leadRuntime.pending, callId),
    },
  };
}

/** Map a persisted transcript to the lead conversation entries. */
export function leadEntriesFromTranscript(
  transcript: LeadTranscript,
  previousEntries: LeadEntry[] = [],
): LeadEntry[] {
  const entries: LeadEntry[] = [];
  const previousTools = new Map(
    previousEntries.flatMap((entry) =>
      entry.kind === "tool" ? [[entry.toolCallId, entry] as const] : [],
    ),
  );
  const toolIndex = new Map<string, number>();
  transcript.messages.forEach((message, index) => {
    if (message.role === "user" || message.role === "assistant") {
      const body = messageText(message.content).trim();
      if (message.role === "user" && body.startsWith(TEAM_NOTICE_PREFIX)) {
        const firstLine = body.indexOf("\n");
        const workerId = body.slice(TEAM_NOTICE_PREFIX.length, firstLine === -1 ? body.length : firstLine);
        entries.push({
          id: `history-team-${index}`,
          kind: "team-message",
          workerId,
          body: firstLine === -1 ? "" : body.slice(firstLine + 1),
          time: "restored",
        });
        return;
      }
      const images = messageImages(message.content);
      // Keep a message when it has text OR attachments: an image-only user
      // turn must survive the snapshot round-trip instead of disappearing.
      if (body || images.length > 0) {
        entries.push({
          id: `history-${index}`,
          kind: "message",
          author: message.role === "user" ? "user" : "lead",
          body,
          ...(images.length > 0 ? { images } : {}),
          time: "restored",
        });
      }
      if (message.role !== "assistant" || !Array.isArray(message.tool_calls)) return;
      message.tool_calls.forEach((call, callIndex) => {
        const callId =
          typeof call?.id === "string" && call.id
            ? call.id
            : `history-call-${index}-${callIndex}`;
        const previous = previousTools.get(callId);
        const entry: LeadEntry = {
          id: `history-tool-${index}-${callIndex}`,
          kind: "tool",
          time: "restored",
          toolCallId: callId,
          toolName:
            typeof call?.name === "string" && call.name ? call.name : "tool",
          arguments: typeof call?.arguments === "string" ? call.arguments : "",
          liveOutput: "",
          startedAtMs: previous?.startedAtMs,
          result: null,
        };
        entries.push(entry);
        toolIndex.set(callId, entries.length - 1);
      });
      return;
    }
    if (message.role !== "tool") return;
    const callId =
      typeof message.tool_call_id === "string" ? message.tool_call_id : "";
    const result = normalizeToolResult(
      messageText(message.content),
      message.tool_is_error !== true,
    );
    const previous = previousTools.get(callId);
    const existingIndex = callId ? toolIndex.get(callId) : undefined;
    if (existingIndex !== undefined) {
      const existing = entries[existingIndex];
      if (existing?.kind === "tool") {
        entries[existingIndex] = {
          ...existing,
          result,
          liveOutput: "",
          endedAtMs: previous?.endedAtMs,
        };
      }
      return;
    }
    entries.push({
      id: `history-tool-result-${index}`,
      kind: "tool",
      time: "restored",
      toolCallId: callId,
      toolName: typeof message.name === "string" && message.name ? message.name : "tool",
      arguments: "",
      liveOutput: "",
      startedAtMs: previous?.startedAtMs,
      endedAtMs: previous?.endedAtMs,
      result,
    });
  });
  return entries;
}

function withProjectModel(state: AppState, model: string | null): AppState {
  if (!model) return state;
  return {
    ...state,
    projects: state.projects.map((project) =>
      project.id === state.projectId ? { ...project, leadModel: model } : project,
    ),
  };
}

/** Apply an authoritative native snapshot. */
export function applyLeadSnapshot(state: AppState, snapshot: LeadSnapshot): AppState {
  let foundActive = false;
  const planItems = (snapshot.plan?.steps ?? []).map((step) => {
    const active = !step.done && !foundActive;
    if (active) foundActive = true;
    const header = step.header?.trim();
    const marker = header ? `**${header}**` : "";
    const detail = marker && step.text.startsWith(marker)
      ? step.text.slice(marker.length).replace(/^\s*(?:—|–|-|:)?\s*/, "")
      : step.text;
    return {
      id: `${snapshot.plan?.id ?? "plan"}-${step.index}`,
      title: header || `Step ${step.index + 1}`,
      detail,
      owner: "lead",
      status: step.done ? "done" as const : active ? "active" as const : "queued" as const,
    };
  });
  const displayedModel = snapshot.model_id ?? snapshot.model;
  const currentPending = state.leadRuntime.pending;
  // Keep the live event's extra fields only for the SAME interaction; a
  // different id is a different question or approval whose text must not
  // inherit the previous one's.
  const pending = snapshot.pending
    ? {
        ...(currentPending && samePendingInteraction(currentPending, snapshot.pending)
          ? currentPending
          : {}),
        ...snapshot.pending,
      }
    : null;
  const next = withProjectModel(
    {
      ...state,
      plan: snapshot.plan ?? null,
      planItems,
      // Review/landed cards live outside the transcript: keep them in place.
      leadEntries: keepActivityCards(
        leadEntriesFromTranscript(snapshot.transcript, state.leadEntries),
        state.leadEntries,
      ),
      leadRuntime: {
        ...state.leadRuntime,
        mode: "native",
        status:
          state.leadRuntime.status === "error" && snapshot.generation === state.leadRuntime.generation
            ? "error"
            : "ready",
        generation: snapshot.generation,
        busy: snapshot.busy,
        error:
          snapshot.generation === state.leadRuntime.generation
            ? state.leadRuntime.error
            : null,
        errorRetrying:
          snapshot.busy && snapshot.generation === state.leadRuntime.generation
            ? state.leadRuntime.errorRetrying
            : false,
        model: snapshot.model,
        modelId: snapshot.model_id,
        reasoningEffort: snapshot.reasoning_effort ?? null,
        models: snapshot.models,
        defaultModel: snapshot.default_model,
        pending,
        usedTokens: snapshot.used_tokens,
        maxTokens: snapshot.max_tokens,
        turnStartedAtMs: snapshot.busy ? state.leadRuntime.turnStartedAtMs : null,
        streamingEntryId: null,
      } satisfies LeadRuntimeInfo,
    },
    displayedModel,
  );
  return next;
}

function appendLeadText(state: AppState, generation: number, text: string): AppState {
  const id =
    state.leadRuntime.streamingEntryId ??
    `lead-stream-${generation}-${state.leadEntries.length}`;
  const existing = state.leadEntries.find((entry) => entry.id === id);
  const leadEntries = existing
    ? state.leadEntries.map((entry) =>
        entry.id === id && entry.kind === "message"
          ? { ...entry, body: entry.body + text }
          : entry,
      )
    : [
        ...state.leadEntries,
        {
          id,
          kind: "message" as const,
          author: "lead" as const,
          body: text,
          time: "now",
        },
      ];
  return {
    ...state,
    leadEntries,
    leadRuntime: { ...state.leadRuntime, streamingEntryId: id },
  };
}

/** Narrow a `context_usage` event's per-role breakdown payload. */
function roleBreakdown(value: unknown): LeadRuntimeInfo["contextBreakdown"] {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  const counts = ["system", "user", "assistant", "tool"].map((role) => record[role]);
  return counts.every((count) => typeof count === "number")
    ? {
        system: counts[0] as number,
        user: counts[1] as number,
        assistant: counts[2] as number,
        tool: counts[3] as number,
      }
    : null;
}

/** Apply one live native event, ignoring stale generations. */
export function applyLeadEvent(state: AppState, event: LeadEvent): AppState {
  if (state.leadRuntime.mode !== "native" || event.generation !== state.leadRuntime.generation) {
    return state;
  }
  const inner = event.event?.event;
  if (!inner) return state;
  const runtime = state.leadRuntime.errorRetrying &&
    !["error", "finished", "exited", "parked"].includes(inner.kind)
    ? { ...state.leadRuntime, error: null, errorRetrying: false }
    : state.leadRuntime;
  if (runtime !== state.leadRuntime) {
    state = { ...state, leadRuntime: runtime };
  }

  switch (inner.kind) {
    case "started":
      return {
        ...state,
        leadEntries: state.leadEntries.map((entry) =>
          entry.kind === "tool" && entry.streamIndex !== undefined
            ? { ...entry, streamIndex: undefined }
            : entry,
        ),
        leadRuntime: {
          ...runtime,
          status: "ready",
          busy: true,
          error: null,
          errorRetrying: false,
          turnStartedAtMs: runtime.turnStartedAtMs ?? Date.now(),
          pending: null,
          streamingEntryId: null,
        },
      };

    case "text_delta":
      return appendLeadText(state, event.generation, inner.text ?? "");

    case "tool_call_start": {
      const callId = typeof inner.id === "string" ? inner.id : "";
      if (!callId) return state;
      const toolName =
        typeof inner.name === "string"
          ? inner.name
          : typeof inner.tool_name === "string"
            ? inner.tool_name
            : "tool";
      return startToolEntry(state, event.generation, {
        id: callId,
        name: toolName,
        streamIndex: typeof inner.index === "number" ? inner.index : undefined,
      });
    }

    case "tool_execution_start": {
      const callId =
        typeof inner.tool_call_id === "string" ? inner.tool_call_id : "";
      if (!callId) return state;
      return startToolExecution(state, callId);
    }

    case "tool_call_arg_delta":
      if (typeof inner.index !== "number" || typeof inner.fragment !== "string") {
        return state;
      }
      return appendToolArguments(state, inner.index, inner.fragment);

    case "tool_output_delta": {
      const callId =
        typeof inner.tool_call_id === "string" ? inner.tool_call_id : "";
      if (!callId || typeof inner.text !== "string") return state;
      return appendToolOutput(state, callId, inner.text);
    }

    case "tool_result": {
      const callId =
        typeof inner.tool_call_id === "string" ? inner.tool_call_id : "";
      if (!callId) return state;
      return finishToolEntry(
        state,
        event.generation,
        callId,
        normalizeToolResult(inner.result),
        typeof inner.tool_name === "string" ? inner.tool_name : "tool",
      );
    }

    case "host_tool_call": {
      const callId =
        typeof inner.call_id === "string"
          ? inner.call_id
          : typeof inner.tool_call_id === "string"
            ? inner.tool_call_id
            : "";
      if (!callId) return state;
      const toolName =
        typeof inner.name === "string"
          ? inner.name
          : typeof inner.tool_name === "string"
            ? inner.tool_name
            : "tool";
      const next = startToolEntry(state, event.generation, {
        id: callId,
        name: toolName,
        argumentsText: toolArgumentsText(inner.args),
      });
      const pending: LeadPendingInteraction = {
        ...inner,
        kind: "host_tool_call",
        request_id: callId,
        tool_call_id: callId,
        tool_name: toolName,
        args: inner.args,
      };
      return {
        ...next,
        leadRuntime: { ...next.leadRuntime, busy: true, pending },
      };
    }

    case "model_changed": {
      const modelId = typeof inner.model === "string" ? inner.model : runtime.modelId;
      return withProjectModel(
        {
          ...state,
          leadRuntime: { ...runtime, modelId },
        },
        modelId,
      );
    }

    case "approval_request":
    case "user_question": {
      const requestId =
        typeof inner.tool_call_id === "string"
          ? inner.tool_call_id
          : typeof inner.request_id === "string"
            ? inner.request_id
            : undefined;
      const pending: LeadPendingInteraction =
        inner.kind === "approval_request"
          ? {
              ...inner,
              kind: "approval",
              request_id: requestId,
            }
          : {
              ...inner,
              kind: "question",
              question_id: inner.question_id,
            };
      return {
        ...state,
        leadRuntime: {
          ...runtime,
          busy: true,
          pending,
        },
      };
    }

    case "context_usage": {
      const breakdown = roleBreakdown(inner.breakdown);
      return {
        ...state,
        leadRuntime: {
          ...runtime,
          usedTokens: typeof inner.used === "number" ? inner.used : runtime.usedTokens,
          maxTokens: typeof inner.max === "number" ? inner.max : runtime.maxTokens,
          contextBreakdown: breakdown,
        },
      };
    }

    case "compact_started":
      return { ...state, leadRuntime: { ...runtime, compacting: true } };

    case "compacted": {
      const usedTokens = typeof inner.after === "number" ? inner.after : runtime.usedTokens;
      return { ...state, leadRuntime: { ...runtime, compacting: false, usedTokens } };
    }

    case "finished":
    case "exited":
    case "parked":
      return {
        ...state,
        leadRuntime: {
          ...runtime,
          busy: false,
          errorRetrying: false,
          turnStartedAtMs: null,
          pending: null,
          streamingEntryId: null,
        },
      };

    case "error":
      return {
        ...state,
        leadRuntime: {
          ...runtime,
          status: inner.retrying === true ? runtime.status : "error",
          busy: inner.retrying === true ? runtime.busy : false,
          error:
            inner.error ?? inner.message ?? "The lead stopped with an error.",
          errorRetrying: inner.retrying === true,
          streamingEntryId: inner.retrying === true ? runtime.streamingEntryId : null,
        },
      };

    default:
      return state;
  }
}

/** Whether this event means the turn is over (or a compaction rewrote the
 * conversation) and a snapshot refresh is due. */
export function leadEventNeedsSnapshot(event: LeadEvent): boolean {
  return ["finished", "exited", "parked", "approval_request", "user_question", "compacted"].includes(
    event.event?.event?.kind ?? "",
  );
}

/**
 * Whether this event records project cost and token usage.
 *
 * A lead `usage` event changes the durable per-project accumulator, so the
 * header's cost metric must re-read the team snapshot (which carries the
 * aggregate) rather than trusting the lead runtime counters.
 */
export function leadEventAffectsUsage(event: LeadEvent): boolean {
  return event.event?.event?.kind === "usage";
}
