// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// TypeScript types mirroring the Rust IPC types (src-tauri/src/ipc/).

export type AgentId = number;

// ── Backlog types (mirror the Rust IPC types in src-tauri/src/ipc/backlog.rs) ──

/** Lifecycle status of a backlog item. */
export type BacklogStatus =
  | "pending"
  | "in_flight"
  | "done"
  | "failed"
  | "cant_resolve";

/** A single queued prompt in the backlog. */
export interface BacklogItem {
  /** UUIDv4 string id (unique across worktrees — union-merge safe). */
  id: string;
  text: string;
  images: string[];
  status: BacklogStatus;
  created_at: number;
  note: string | null;
  /**
   * Deferred (skip Run-All): the item stays pending and visible in the
   * Backlog tab but Run-All never selects or counts it — reserved for
   * manual in-app handling (the ▶ button still dispatches it) or later
   * re-inclusion. Orthogonal to status: the item keeps its status and
   * every existing transition keeps working. Absent = false (the backend
   * omits the field when unset).
   */
  deferred?: boolean;
  /**
   * The ROOT plan id dispatched for this item — the item↔plan linkage
   * (backlog 45dcf577) recorded when the workflow enters Executing (the
   * create_plan moment), refreshed whenever a fresh plan replaces an
   * abandoned one. The Backlog tab's plan chip shows its 8-char prefix
   * (the same short id the tool layer reports). Absent/null = not yet
   * stamped (the backend omits the field when None).
   */
  plan_id?: string | null;
  /**
   * The plan's TITLE at dispatch — recorded alongside plan_id (read from
   * the plan file's `# Plan: <title>` heading at the InFlight stamp,
   * backlog f45513b2) so the Backlog tab shows a human-friendly
   * identifier that survives plan completion. Absent/null = not yet
   * stamped (pre-change items) — the UI falls back to the short plan-id
   * chip.
   */
  plan_title?: string | null;
  /**
   * The run-all checkpoint sha parsed from the note head — the git
   * checkpoint taken before this item's turn, i.e. the manual
   * resume/rollback anchor (`git reset --hard <sha>`), machine-readable
   * instead of buried in the note text. Null when the item was never
   * checkpointed (manual adds, auto-feed). Mirrors the Rust
   * `BacklogItemView::checkpoint_sha` (always present from the backend).
   */
  checkpoint_sha?: string | null;
  /**
   * Soft-delete timestamp (secs since epoch), set when the item is deleted.
   * The backend omits the field for live items and filters soft-deleted items
   * out of every list; they are hard-purged 30 days after deletion.
   */
  deleted_at?: number | null;
}

/** Progress of the Run-All ("overnight") loop. */
export interface RunAllProgress {
  active: boolean;
  done: number;
  total: number;
  /** True while the run-all auto-compact (`auto_compact_on_plan_complete`)
   *  is compacting the main agent's context between items. */
  compacting: boolean;
  /** The run's dispatch concurrency (parallel run-all, plan ffd7a86f):
   *  1 = sequential (today's behavior), N > 1 = items beyond the first
   *  dispatch concurrently to spawned worktree agents. */
  concurrency: number;
  /** The concurrently dispatched items (parallel run-all, plan ffd7a86f) —
   *  one entry per spawned worktree agent. Empty on the sequential path. */
  spawned: SpawnedRunView[];
  /** The last run's completion note — "wrote N knowledge record(s) during
   *  this run — uncommitted in the main tree" when the run's agents wrote
   *  knowledge files; persists until the next run starts. */
  note: string | null;
}

/** One concurrently dispatched run-all item (parallel run-all, plan ffd7a86f). */
export interface SpawnedRunView {
  agent_id: number;
  item_id: string;
  branch: string;
}

/** Payload of the `backlog://changed` event (emitted on every backend mutation). */
export interface BacklogChangedPayload {
  items: BacklogItem[];
  auto_feed: boolean;
  /** Parallel run-all toggle (session-only, default off; plan ffd7a86f) —
   *  gates run-all concurrency only; auto-feed stays sequential. */
  parallel: boolean;
  run_all: RunAllProgress;
}

export type Approval = "approve" | "deny" | "deny_all";

export type WorkflowState = "planning" | "executing" | "reviewing" | "complete" | "skill" | "subagent";

export type SafetyMode =
  | "approve-each-action"
  | "auto-read-approve-writes"
  | "auto-approve-project"
  | "autonomous";

export type FinishReason =
  | "stop"
  | "tool_calls"
  | "length"
  | "content_filter"
  | { other: string };

/**
 * The phases of the agent's request loop, mirroring the backend `PhaseKind`
 * (snake_case wire form). `idle` is frontend-only: no turn is running.
 */
export type TurnPhaseKind =
  | "sending"
  | "compacting"
  | "waiting"
  | "reasoning"
  | "streaming"
  | "running_tools";
export type TurnPhase = "idle" | TurnPhaseKind;

export interface ToolResult {
  success: boolean;
  output: string;
  data?: unknown;
}

/**
 * The Rust `ApprovalPreview` (src/provider/mod.rs) as it reaches the UI,
 * discriminated on `kind`:
 * - `diff` — one file's unified diff (file_edit),
 * - `new_file` — a new file's full content (file_write),
 * - `multi_diff` — ONE combined diff covering every file a `multi_edit` call
 *   changes (each file contributing its own `--- path` / `+++ path`
 *   section), so no single `path` applies: the changed files are `paths`.
 */
export type ApprovalPreview =
  | { kind: "diff"; path: string; diff?: string; content?: string }
  | { kind: "new_file"; path: string; diff?: string; content?: string }
  | { kind: "multi_diff"; paths: string[]; diff: string };

export interface PlanStep {
  index: number;
  text: string;
  /** The short bold header extracted from the text (when the step starts with
   *  `**Header**`). Shown alone in the executing toolbar; the full `text` is
   *  shown expanded in PlanProgress. Absent for plain steps. */
  header?: string | null;
  done: boolean;
}

export interface PlanFile {
  title: string;
  goal: string;
  context: string;
  steps: PlanStep[];
}

export interface AgentEventPayload {
  agent_id: AgentId;
  event: SerializableAgentEvent;
}

export type SerializableAgentEvent =
  | { kind: "started" }
  | { kind: "text_delta"; text: string }
  | { kind: "reasoning_delta"; text: string }
  | { kind: "tool_call_start"; index: number; id: string; name: string }
  | { kind: "tool_call_arg_delta"; index: number; fragment: string }
  | {
      /** Live output from a RUNNING tool (today only `shell`), one chunk per
       *  throttle window (>= 60 ms or >= 8 KiB, so <= ~16 chunks/s per
       *  stream). DISPLAY-ONLY: the complete text still arrives on the call's
       *  `tool_result`, which is what the model consumes — the reducer
       *  therefore drops a chunk whose call already has a result (a
       *  timed-out/cancelled reader can emit one after the fact). */
      kind: "tool_output_delta";
      tool_call_id: string;
      stream: "stdout" | "stderr";
      text: string;
    }
  | {
      kind: "approval_request";
      tool_call_id: string;
      tool_name: string;
      args: unknown;
      preview: ApprovalPreview | null;
      /** True for core operations (git merge/push) that always prompt
       * regardless of mode/rules. The UI hides the no-op "Mark Safe" /
       * "Allow for project" buttons when true. */
      core_operation: boolean;
    }
  | { kind: "tool_result"; tool_call_id: string; result: ToolResult }
  | {
      kind: "usage";
      prompt_tokens: number;
      completion_tokens: number;
      reasoning_tokens: number;
      cached_tokens: number;
      ttft_ms: number | null;
      generation_ms: number | null;
    }
  | {
      kind: "context_usage";
      used: number;
      max: number;
      breakdown: { system: number; user: number; assistant: number; tool: number };
    }
  | {
      /** Context compaction completed — token count before/after. Emitted on
       * a completed manual (/compact, popup button) or automatic threshold
       * compaction so the transcript can confirm the reduction — or, when
       * nothing was compacted, a no-change note (every `compact_started`
       * gets a paired end). */
      kind: "compacted";
      before: number;
      after: number;
    }
  | {
      /** Context compaction started — the summarization call is running.
       * Emitted on every compaction path (manual /compact, popup button,
       * mid-turn, auto) so the transcript can announce "Compacting
       * context…"; paired with `compacted` (completion) or `error`
       * (failure). */
      kind: "compact_started";
    }
  | {
      /** A vision-model image description is starting — the text-only
       * provider fallback describes each pasted image via the vision model
       * BEFORE the turn starts (Started fires later, inside run_turn), so
       * this is what the transcript shows instead of a silent pause.
       * Paired with exactly one `vision_described`. */
      kind: "vision_describe";
      /** 1-based position of this image among the prompt's attachments. */
      index: number;
      /** Total number of image attachments in the prompt. */
      total: number;
      /** The prompt text sent to the vision model. */
      query: string;
    }
  | {
      /** A vision-model image description finished — the counterpart to
       * `vision_describe`. On success `description` holds the vision model's
       * answer (the text folded into the prompt); on failure it holds the
       * error text and `success` is false. */
      kind: "vision_described";
      /** 1-based position of this image among the prompt's attachments. */
      index: number;
      /** Total number of image attachments in the prompt. */
      total: number;
      /** Whether the vision call succeeded. */
      success: boolean;
      /** The vision model's answer, or the error text when `success` is
       * false. */
      description: string;
    }
  | {
      kind: "workflow_state_changed";
      state: WorkflowState;
      /** Id of the ROOT plan (bottom of the stack), null when no plan. Constant
       *  across sub-plan push/pop and skill transitions; changes only when a
       *  fresh top-level plan starts. The Diff-tab changed-file list resets
       *  when this id changes. */
      top_plan_id: string | null;
    }
  /** A plan step was completed. `step_index` is the 1-indexed step number
   *  (1 = the first step — matches the plan document's numbering). */
  | { kind: "step_completed"; step_index: number }
  | { kind: "suggestion_injected"; text: string; images: string[] }
  | { kind: "prompt_dispatched"; text: string; images: string[] }
  | { kind: "skill_started"; name: string; prompt: string }
  | {
      kind: "model_changed";
      model: string;
      provider?: string | null;
      /** The DISPLAY-space effective reasoning effort of the model now
       *  serving ("off" | "low" | "medium" | "high" | "max"), absent when
       *  unknown (mock-backed loops) — the UI falls back to the toolbar
       *  echo / endpoint default. Same resolution the request builder uses,
       *  in UI vocabulary (backlog 51dab4da). */
      reasoning_effort?: string | null;
    }
  | { kind: "memory_recalled"; hits: { tier: string; title: string }[] }
  | {
      kind: "user_question";
      question_id: string;
      question: string;
      options: { label: string; description?: string | null }[];
    }
  | { kind: "finished"; reason: FinishReason }
  | {
      /** The turn moved into a new request-loop phase — drives the inflight
       * bar's live status (sending / waiting / reasoning / answering /
       * running tools). */
      kind: "phase";
      phase: TurnPhaseKind;
    }
  | { kind: "exited" }
  /**
   * A tool-spawned background agent finished its task (synthesized by the
   * event forwarder when a child with a recorded parent reaches `finished` /
   * a final `error`). `child_id` is the background agent's id; `success` is
   * false when the task ended in an error. The sidebar marks that agent done.
   */
  | {
      kind: "child_finished";
      child_id: number;
      name: string;
      success: boolean;
    }
  | { kind: "error"; error: string; retrying: boolean }
  | {
      /** The agent parked (went idle awaiting input) instead of
       * auto-continuing — pre-stall evidence. `reason` distinguishes a user
       * stop (`interrupted`) and an exhausted auto-continue budget
       * (`budget_exhausted`) — both need manual input and surface as a
       * banner — from the by-design waits (`waiting_for_descendants`,
       * `no_work_expected`). */
      kind: "parked";
      reason:
        | "interrupted"
        | "budget_exhausted"
        | "waiting_for_descendants"
        | "no_work_expected";
      /** The workflow state at the park (evidence label, e.g. "Reviewing"). */
      workflow_state: string;
      /** Whether spawned descendants were still running. */
      descendants_running: boolean;
      /** The auto-continue streak at the park. */
      auto_continue_streak: number;
    };

export interface AgentInfo {
  id: AgentId;
  name: string;
  running: boolean;
  /** Id of the agent that spawned this one via the spawn_agent tool; null for
   *  UI-spawned agents and the main agent (the parentless agent with the
   *  smallest id). */
  parent_id: AgentId | null;
  /** The model id this agent is currently running (read from its provider).
   *  Absent when the agent's loop was removed (it exited). The frontend shows
   *  it as a second line in the agent tab. */
  model?: string | null;
  /** The `endpoints.toml` endpoint name serving `model` — reported by the
   *  backend so the UI never mislabels the provider when the same model id
   *  is listed under two endpoints (first-match resolution over the endpoint
   *  list cannot disambiguate those). Absent when unknown (mock-backed
   *  agents); the UI then resolves the model id against endpoints.toml. */
  provider?: string | null;
  /** The DISPLAY-space effective reasoning effort of the model serving this
   *  agent's most recent turn ("off" | "low" | "medium" | "high" | "max") —
   *  resolved exactly as the request builder resolves it (per-context
   *  ModelRef.reasoning_effort override, else the model's default chain
   *  ModelSpec.reasoning_effort → endpoint default → "max", gated +
   *  clamped). Absent when unknown (mock-backed loops); the UI then falls
   *  back to the toolbar echo / endpoint default (backlog 51dab4da: the
   *  status bar must show the effort of the model actually in use). */
  reasoning_effort?: string | null;
}

export interface ActiveSkillInfo {
  /** The skill name (selects the registry entry). */
  name: string;
  /** The goal the agent drives toward while the skill is active. */
  prompt: string;
  /** Where the workflow lands after `skill_end`. */
  target_state: WorkflowState;
}

export interface WorkflowStateInfo {
  state: WorkflowState;
  plan: PlanFile | null;
  /** How many plans deep the stack is (0 = Planning, 1 = a single root plan). */
  depth: number;
  /** The ancestor plans below the active one, root first (empty unless the
   *  active plan is a sub-plan). Each carries an `id` (so the staircase can
   *  click an ancestor to view it via `getPlan`) + `title`. */
  parents: { id: string; title: string }[];
  /** The active skill overlay, when the workflow is in the Skill state.
   *  `undefined` otherwise. */
  skill?: ActiveSkillInfo;
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

/**
 * A single safety rule: a tool name + a regex pattern. The pattern is matched
 * against the tool call's signature (`"<tool>:<key_argument>"`). If it matches,
 * the call is auto-approved (the approval prompt is skipped).
 */
export interface SafetyRule {
  tool: string;
  pattern: string;
}

// A single tool invocation within a (possibly merged) tool transcript entry.
export interface ToolInvocation {
  id: string;
  index: number; // correlates arg-delta fragments during streaming
  args: string;
  result: ToolResult | null; // null = still running
  /**
   * When the call was issued (ms since epoch) — stamped by the reducer on
   * `tool_call`. With `endedAt` it drives the card's duration + wall-clock
   * timing display. Absent on invocations restored from legacy saved
   * conversations (they simply show no timing).
   */
  startedAt?: number;
  /**
   * When the result landed (ms since epoch) — stamped by the reducer on
   * `tool_result`. Absent while the call is still running.
   */
  endedAt?: number;
  /**
   * Live output tail while the call is running — appended by
   * `tool_output_delta` chunks (the reducer keeps only the last
   * `LIVE_OUTPUT_CAP` chars) and cleared the moment `result` lands. Rendered
   * as the card's live preview; the expanded body still shows the full result.
   */
  liveOutput?: string;
}

// Transcript entry types (for the chat view).
export type TranscriptEntry = (
  | {
      kind: "user";
      text: string;
      images?: string[];
      /**
       * Count of image attachments dropped from this entry by the
       * transcript image byte budget (agentState.ts `capTranscriptImages`,
       * mem-perf review LOW 4): when the retained image payloads exceed
       * `MAX_TRANSCRIPT_IMAGE_CHARS`, the OLDEST entries' `images` are
       * replaced by this count and Message renders a placeholder chip.
       * Mutually exclusive with `images` (eviction removes the payloads).
       */
      imagesEvicted?: number;
    }
  | { kind: "assistant"; text: string }
  | {
      kind: "tool";
      name: string;
      calls: ToolInvocation[]; // consecutive calls to the same tool, merged
    }
  | { kind: "error"; text: string }
  | {
      kind: "steer";
      text: string;
      /** The steer's image attachments (base64 data URLs), rendered as
       *  thumbnails like a user prompt's images. */
      images?: string[];
      /** Count of image attachments dropped from this entry by the
       * transcript image byte budget (same as user entries). */
      imagesEvicted?: number;
    }
  | { kind: "skill"; name: string; prompt: string }
  | { kind: "qa"; question: string; answer: string }
  | {
      kind: "memory";
      name: string;
      /** The owning tool call's id — the correlation key that lets
       *  `tool_result` finalize THIS entry (and only this entry) instead of
       *  the first running memory card it finds. Undefined for the instant
       *  `auto-recall` entries (created already finished). */
      id?: string;
      /** The owning tool call's index — correlates streaming
       *  `tool_call_arg_delta` fragments into `args` while running. */
      index?: number;
      /** The accumulated raw JSON args of the owning tool call — parsed for
       *  the "what was searched" chip on memory_search cards. */
      args?: string;
      /** For memory_search: how many memories the result reported matched
       *  (the "N memories matched" header; 0 for "no memories matched").
       *  Undefined for other memory tools and while still running. */
      matched?: number;
      tier?: string;
      title?: string;
      snippet?: string;
      /** The full recalled-hit list (auto-recall entries) or the parsed
       *  per-hit list of a memory_search result (backlog 41f39672) — the
       *  card expands to show these. */
      hits?: { tier: string; title: string; score?: number }[];
      success: boolean;
      running: boolean;
    }
  | {
      /** A vision-model image description ("image parsing" card) — pushed by
       *  `vision_describe` (running) and completed by `vision_described`.
       *  The card collapses to a one-line "image parsing (i/n)" status and
       *  expands to show the query sent to the vision model and the response
       *  it returned (user request: "I would love to see what the query and
       *  response is when I uncollapse"). */
      kind: "vision";
      /** 1-based position of this image among the prompt's attachments. */
      index: number;
      /** Total number of image attachments in the prompt. */
      total: number;
      /** The prompt text sent to the vision model. */
      query: string;
      /** The vision model's answer, or the error text when done. Null while
       *  the call is still in flight. */
      description: string | null;
      /** Whether the vision call succeeded (meaningful once done). */
      success: boolean;
      /** True between `vision_describe` and its paired `vision_described`. */
      running: boolean;
    }
) & {
  /** Creation time (ms since epoch) — stamped when the entry first appears
   *  (applyAgentEvent, identity-based) and used for the hover-timestamp
   *  tooltip. Absent on entries restored from legacy saved conversations
   *  (they simply show no tooltip). */
  ts?: number;
  /**
   * Stable monotonic id for React keying (mem-perf review HIGH 3) — stamped
   * at creation by the applyAgentEvent identity pass (or the direct-push
   * sites: InputBar pushes, qa answers, /load restores), so Conversation's
   * turn/run/entry keys survive the transcript cap's index shifts and
   * entry replacements. Survives /save + /load round-trips. Named
   * `entryId` (not `id`) because the `memory` variant already carries
   * `id` — the owning tool call's string correlation key.
   */
  entryId?: number;
};

// ── Headless-browser types (mirror src/browser/mod.rs + ipc/browser.rs) ──

/** A snapshot of one open headless-browser page. */
export interface BrowserPage {
  /** The manager-assigned page id (used by every browser command). */
  id: string;
  /** The page's current URL. */
  url: string;
  /** The page's current title. */
  title: string;
  /** Whether this is the active page (the one id-less operations act on). */
  active: boolean;
}

/** One buffered console message / JS exception from a page. */
export interface BrowserConsoleEntry {
  /** The severity level (`log`, `warn`, `error`, `exception`, ...). */
  level: string;
  /** The rendered message text. */
  text: string;
}

/** Payload of the `browser://console` event (live console push). */
export interface BrowserConsoleEventPayload {
  /** The page that logged this entry. */
  page_id: string;
  /** The severity level. */
  level: string;
  /** The rendered message text. */
  text: string;
}

// ── Steering-effectiveness metrics (mirror src/agent/steering_stats.rs) ──

/** Per-marker steering counts for one nudge kind. */
export interface SteeringMarkerCounts {
  /** Stable kebab label (`search-nudge`, `shell-tip`, `graph-miss`,
   * `recall-rider`, `read-nudge`). */
  marker: string;
  /** How many tool results carried this marker. */
  fired: number;
  /** How often the next call from the same agent named a target tool. */
  switched: number;
  /** How many next attempts the gate actually intercepted for this kind
   *  (backlog e8b39d72 H4 — the early emission-decay signal; zero for
   *  kinds without a gate). */
  gated: number;
}

/** Snapshot of the steering counters (the `get_steering_stats` command). */
export interface SteeringStatsSnapshot {
  /** Total nudges observed (all marker kinds). */
  fired: number;
  /** Total switches — a marker fired and the next call named a target. */
  switched: number;
  /** Per-marker breakdown (all kinds, even at zero). */
  by_marker: SteeringMarkerCounts[];
  /** File mutations via the file tools this session (successful
   *  file_edit/file_write/file_append calls — plan be16ea36 step 9). */
  file_tool_mutations: number;
  /** File mutations attempted via the shell this session (commands matching
   *  a mutation pattern: Set-Content, Out-File, Add-Content, >>, sed -i,
   *  tee, python*). */
  shell_mutations: number;
}

// ── LLM trace types (mirror the Rust types in src/provider/trace.rs) ──

/** Token usage for one request — the fields that matter for cost/cache analysis. */
export interface LlmUsage {
  /** Input (prompt) tokens. */
  prompt: number;
  /** Output (completion) tokens. */
  completion: number;
  /** Tokens spent on reasoning (completion_tokens_details.reasoning_tokens). */
  reasoning: number;
  /** Prompt tokens served from the provider's cache (OpenAI prompt_tokens_details/input_tokens_details cached_tokens; Anthropic cache_read_input_tokens). */
  cached: number;
}

/**
 * One recorded request/response pair (the full detail — includes the big
 * JSON payloads). Mirror of the Rust `LlmRequestRecord` Serialize output:
 * snake_case fields, `Option` fields nullable.
 */
export interface LlmRequestDetail {
  /** Monotonic id — ids increase with request order. */
  id: number;
  /** Unix epoch milliseconds at capture time. */
  ts_ms: number;
  /** The model id the request targeted. */
  model: string;
  /** The endpoint base URL (host only for display; no credentials). */
  base_url: string;
  /** The endpoint/provider name from endpoints.toml (e.g. "openai"). */
  provider: string;
  /** The exact JSON body POSTed to `/chat/completions`. */
  request_json: Record<string, unknown>;
  /** The raw response text (SSE lines / JSON error body). Null before any bytes arrive. */
  response_raw: string | null;
  /** The tool calls AS DELIVERED by the model for this response — the raw
   *  (id, name, arguments) triple per call, captured at stream completion
   *  BEFORE any harness normalization/execution (backlog e8b39d72 H1: the
   *  generation-vs-harness discriminator; budget-exempt — survives
   *  response_raw eviction). Null until the stream completes. */
  raw_tool_calls:
    | { id: string; name: string; arguments: string; truncated: boolean }[]
    | null;
  /** The client-side stream-guard cut, when one fired: the matched stop
   *  boundary, cut position, and cut size. Null on natural stops — this
   *  distinguishes guard-cut turns from clean empty completions. */
  guard_cut: GuardCutDetails | null;
  /** HTTP status of the response. Null before the request completes. */
  http_status: number | null;
  /** Parsed token usage from the stream's final chunk, if any. */
  usage: LlmUsage | null;
  /** The finish reason string (e.g. "stop", "tool_calls", "length"). */
  finish_reason: string | null;
  /** Time-to-first-token in ms (POST sent → first streamed chunk — the
   *  real first-token wait: upload + server prefill/queue; the
   *  record-created → headers window is connect_ms). */
  ttft_ms: number | null;
  /** Generation time in ms (first chunk → usage/final chunk). */
  generation_ms: number | null;
  /** Reasoning time in ms (first chunk → first answer/tool delta);
   *  null when the request streamed no reasoning. */
  reasoning_ms: number | null;
  /** Connect time in ms (record created → the POST response headers
   *  arrived): TCP/TLS connect + request upload + server ack — the POST
   *  in flight. Pure network waiting; includes the full round trip and
   *  the reasoning_effort fallback retry when one fires. */
  connect_ms: number | null;
  /** Tools time in ms (stream end → the tool batch this response triggered
   *  finished executing). */
  tools_ms: number | null;
  /** Local prompt-prep time in ms (turn-loop iteration start → record
   *  created: prompt build, request-body serialization, trace-log start;
   *  includes compact_ms when auto-compaction fired in that window). */
  prep_ms: number | null;
  /** Auto-compaction time in ms (the summarization LLM call) when it fired
   *  before this request. */
  compact_ms: number | null;
  /** Retry-backoff sleep in ms that preceded this attempt (the 1s/2s sleeps
   *  between failed attempts) — waiting, not local work. */
  backoff_ms: number | null;
  /** Mid-stream stall time in ms — byte-silence gaps > 2s inside the
   *  generation window (a subset of generation_ms). */
  stall_ms: number | null;
  /** Error text for failed requests (HTTP error body or stream error). */
  error: string | null;
  /** True when the consumer dropped the stream mid-flight (a user
   *  interrupt/cancel/compact/clear) — not a provider failure. */
  cancelled: boolean;
  /** True when the raw response exceeded the 2 MiB cap and was cut. */
  response_truncated: boolean;
  /** True when the request body exceeded the configured cap and its longest
   *  string leaves were truncated (JSON structure preserved). */
  request_truncated: boolean;
  /** True when the raw response was EVICTED by the global memory budget
   *  (payload gone; the row's metadata survives). */
  response_evicted: boolean;
  /** Whether the request reached a terminal state (finish_reason or error).
   *  The Trace tab stops re-polling a complete record. */
  is_complete: boolean;
  /** Mutation counter (perf review L5, 2027-01-09): bumped on every change to
   *  the record — every mutation and every raw-budget payload eviction. The
   *  Trace tab's cheap list poll carries it, so the detail poll can skip
   *  re-fetching the full record (up to ~2 MiB) while it is unchanged. */
  version: number;
}

/** Details of a client-side stream-guard termination: recorded when the
 *  stream guard finds a stop boundary in a text delta and cuts the turn. */
export interface GuardCutDetails {
  /** The stop sequence that matched. */
  boundary: string;
  /** Byte index of the match within the triggering text delta. */
  byte_idx: number;
  /** Total byte length of the triggering delta. */
  delta_len: number;
  /** Answer characters streamed before the triggering delta's batch
   *  (unbounded counter — accurate however long the answer). */
  chars_before: number;
  /** True when nothing of the triggering delta survived — the turn can
   *  end with no visible text at all. */
  prefix_empty: boolean;
}

/** The lightweight list-row wire type: everything a row needs, minus payloads. */
export interface LlmRequestSummary {
  id: number;
  ts_ms: number;
  model: string;
  base_url: string;
  /** The endpoint/provider name from endpoints.toml (e.g. "openai"). */
  provider: string;
  http_status: number | null;
  usage: LlmUsage | null;
  finish_reason: string | null;
  /** True once the record is terminal (finish_reason or error) — in-flight
   *  (streaming) rows carry live estimate usage, not the final numbers. */
  is_complete: boolean;
  /** Mutation counter (see LlmRequestDetail.version) — carried by the cheap
   *  list poll so the detail poll can skip re-fetching an unchanged record. */
  version: number;
  ttft_ms: number | null;
  generation_ms: number | null;
  /** Reasoning time in ms (first chunk → first answer/tool delta). */
  reasoning_ms: number | null;
  /** Connect time in ms (record created → the POST response headers
   *  arrived): TCP/TLS connect + request upload + server ack — the POST
   *  in flight. Pure network waiting; includes the full round trip and
   *  the reasoning_effort fallback retry when one fires. */
  connect_ms: number | null;
  /** Tools time in ms (stream end → the tool batch this response triggered
   *  finished executing). */
  tools_ms: number | null;
  /** Local prompt-prep time in ms (see LlmRequestDetail.prep_ms). */
  prep_ms: number | null;
  /** Auto-compaction time in ms (see LlmRequestDetail.compact_ms). */
  compact_ms: number | null;
  /** Retry-backoff sleep in ms (see LlmRequestDetail.backoff_ms). */
  backoff_ms: number | null;
  /** Mid-stream stall time in ms (see LlmRequestDetail.stall_ms). */
  stall_ms: number | null;
  error: string | null;
  /** True when the consumer dropped the stream mid-flight (a user
   *  interrupt/cancel/compact/clear) — not a provider failure: no error
   *  text, the HTTP response was healthy, and provider-errors.jsonl is
   *  never written for these rows. */
  cancelled: boolean;
  response_truncated: boolean;
  /** True when the raw response was evicted by the memory budget. */
  response_evicted: boolean;
  /** The client-side stream-guard cut, when one fired (see
   *  LlmRequestDetail.guard_cut). */
  guard_cut: GuardCutDetails | null;
}

/** One configured MCP server (an `mcp.toml` `[[server]]` entry). Secrets
 *  never appear in this shape by design: `env` lists environment variable
 *  NAMES and `headers_env` maps header names to env-var names — values are
 *  resolved from the environment at connect time. */
export interface McpServer {
  name: string;
  enabled: boolean;
  command?: string | null;
  args?: string[];
  env?: string[];
  url?: string | null;
  headers_env?: Record<string, string>;
  /** Which file the server lives in: "global" (~/.mnemo/mcp.toml) or
   *  "project" (<project>/.coding/mcp.toml — wins by name over global).
   *  Defaults to "global" when absent (legacy payloads). */
  source?: "global" | "project";
  /** Auth scheme for remote servers: "oauth" enables the OAuth 2.1
   *  authorization-code flow (tokens in keys.toml, automatic 401
   *  refresh). Absent/null = no auth beyond headers_env. */
  auth?: string | null;
  /** Pre-registered OAuth client id (required when auth = "oauth"). */
  client_id?: string | null;
  /** OAuth scope(s) requested during authorization (space-joined). */
  scopes?: string[];
  /** Per-server auto-approve trust: trusted servers' tools run without
   *  per-call approval prompts (approval-only — the plan-first state
   *  gates never widen). Default false. */
  trusted?: boolean;
  /** Optional connection idle timeout in seconds: a cached connection
   *  idle longer than this is dropped and re-established lazily on the
   *  next call. Absent/null = keep connections until they fail. */
  idle_timeout_secs?: number | null;
}

/** Live per-server status from the Settings page (mcp_status). */
export interface McpServerStatus {
  name: string;
  connected: boolean;
  tool_count: number;
  last_error: string | null;
  restarts: number;
  idle_timeout_secs: number | null;
}

/** The outcome of starting an OAuth flow: the authorization URL to open
 *  in the user's browser plus a note for the UI. */
export interface McpOauthStart {
  authorize_url: string;
  note: string;
}

/** The outcome of a Settings "Test" click: connect + initialize handshake +
 *  tools/list for one MCP server. */
export interface McpTestResult {
  ok: boolean;
  tool_count: number;
  tool_names: string[];
  error: string | null;
}
