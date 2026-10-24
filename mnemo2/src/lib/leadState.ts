import type {
  AppState,
  LeadEntry,
  LeadEvent,
  LeadPendingInteraction,
  LeadRuntimeInfo,
  LeadSnapshot,
  LeadTranscript,
} from "./types";

/** The idle browser-mode lead state. */
export function initialLeadRuntime(): LeadRuntimeInfo {
  return {
    mode: "mock",
    status: "idle",
    generation: 0,
    busy: false,
    error: null,
    model: null,
    modelId: null,
    models: [],
    defaultModel: null,
    pending: null,
    usedTokens: 0,
    maxTokens: 0,
    streamingEntryId: null,
  };
}

function messageText(content: unknown): string {
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

/** Map a persisted transcript to the lead conversation entries. */
export function leadEntriesFromTranscript(transcript: LeadTranscript): LeadEntry[] {
  const entries: LeadEntry[] = [];
  transcript.messages.forEach((message, index) => {
    if (message.role !== "user" && message.role !== "assistant") return;
    const body = messageText(message.content).trim();
    if (!body) return;
    entries.push({
      id: `history-${index}`,
      kind: "message",
      author: message.role === "user" ? "user" : "lead",
      body,
      time: "restored",
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
  const displayedModel = snapshot.model_id ?? snapshot.model;
  const currentPending = state.leadRuntime.pending;
  const snapshotKind = snapshot.pending?.kind;
  const pending = snapshot.pending
    ? {
        ...(currentPending && currentPending.kind === snapshotKind
          ? currentPending
          : {}),
        ...snapshot.pending,
      }
    : null;
  const next = withProjectModel(
    {
      ...state,
      leadEntries: leadEntriesFromTranscript(snapshot.transcript),
      leadRuntime: {
        ...state.leadRuntime,
        mode: "native",
        status: "ready",
        generation: snapshot.generation,
        busy: snapshot.busy,
        error: null,
        model: snapshot.model,
        modelId: snapshot.model_id,
        models: snapshot.models,
        defaultModel: snapshot.default_model,
        pending,
        usedTokens: snapshot.used_tokens,
        maxTokens: snapshot.max_tokens,
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

/** Apply one live native event, ignoring stale generations. */
export function applyLeadEvent(state: AppState, event: LeadEvent): AppState {
  const runtime = state.leadRuntime;
  if (runtime.mode !== "native" || event.generation !== runtime.generation) return state;
  const inner = event.event?.event;
  if (!inner) return state;

  switch (inner.kind) {
    case "started":
      return {
        ...state,
        leadRuntime: {
          ...runtime,
          busy: true,
          error: null,
          pending: null,
          streamingEntryId: null,
        },
      };

    case "text_delta":
      return appendLeadText(state, event.generation, inner.text ?? "");

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

    case "finished":
    case "exited":
    case "parked":
      return {
        ...state,
        leadRuntime: {
          ...runtime,
          busy: false,
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
          streamingEntryId: inner.retrying === true ? runtime.streamingEntryId : null,
        },
      };

    default:
      return state;
  }
}

/** Whether this event means the turn is over and a snapshot refresh is due. */
export function leadEventNeedsSnapshot(event: LeadEvent): boolean {
  return ["finished", "exited", "parked", "approval_request", "user_question"].includes(
    event.event?.event?.kind ?? "",
  );
}
