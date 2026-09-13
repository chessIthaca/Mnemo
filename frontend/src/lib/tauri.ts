// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Typed wrappers around Tauri invoke() + listen().

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentId,
  AgentInfo,
  Approval,
  BrowserConsoleEntry,
  BrowserConsoleEventPayload,
  BrowserPage,
  FileEntry,
  PlanFile,
  WorkflowStateInfo,
  AgentEventPayload,
  SafetyMode,
  BacklogItem,
  BacklogChangedPayload,
  LlmRequestDetail,
  LlmRequestSummary,
  SteeringStatsSnapshot,
  McpServer,
  McpOauthStart,
  McpServerStatus,
  McpTestResult,
} from "./types";

const AGENT_EVENT_CHANNEL = "agent://event";
const BACKLOG_CHANGED_CHANNEL = "backlog://changed";
const BROWSER_REVEAL_CHANNEL = "browser://reveal";

// Commands (frontend → Rust).

/**
 * Send a prompt to an agent. `images` is an optional list of base64 data URLs
 * (`data:image/png;base64,...`) for pasted image attachments. When non-empty,
 * the agent builds a multipart user message (text + image_url blocks).
 */
export async function sendPrompt(
  agentId: AgentId,
  text: string,
  images: string[] = []
): Promise<void> {
  await invoke("send_prompt", { agentId, text, images });
}

/**
 * Send a suggestion (steer) to an agent — mid-work guidance queued and
 * injected at the next turn boundary. `images` is a list of base64 data URLs
 * (`data:image/png;base64,...`) for pasted image attachments; they ride the
 * steer payload end-to-end and are injected as image blocks exactly like a
 * normal prompt's images (multimodal path / vision-model fallback).
 */
export async function sendSuggestion(
  agentId: AgentId,
  text: string,
  images: string[] = []
): Promise<void> {
  await invoke("send_suggestion", { agentId, text, images });
}

/**
 * Cancel a queued suggestion (steer) — the "x" on a pending steer in the
 * backlog. Sends `cancel_suggestion` so the backend drops the matching
 * queued steer before it's injected (handled by `StopReason::fold` and the
 * pre-inject drain in `run_turn_with_retry`). Fire-and-forget at call sites
 * — the agent processes the cancel asynchronously. A no-op if the steer was
 * already injected or none is queued.
 */
export async function cancelSuggestion(agentId: AgentId, text: string): Promise<void> {
  await invoke("cancel_suggestion", { agentId, text });
}

/**
 * Interrupt an agent's current LLM generation. Sends `AgentCommand::Interrupt`
 * to the agent's inbox; the streaming `select!` in the agent loop breaks the
 * stream and emits `Finished`, leaving the agent alive and ready for the next
 * prompt. Used by the Stop button and by `/clear` (to stop repopulation of a
 * cleared conversation). Fire-and-forget at call sites — the agent processes
 * the interrupt asynchronously.
 */
export async function interrupt(agentId: AgentId): Promise<void> {
  await invoke("interrupt", { agentId });
}

export async function cancel(agentId: AgentId): Promise<void> {
  await invoke("cancel", { agentId });
}

/**
 * Manually compact an agent's context — summarize old messages into a summary
 * system message, keeping the system prompt + recent messages. Backed by the
 * `/compact` slash command + the context-popup Compact button. If the agent is
 * mid-turn, the turn ends first so the compaction runs on a quiescent
 * conversation. Fire-and-forget at call sites — the agent processes the
 * compaction asynchronously.
 */
export async function compact(agentId: AgentId): Promise<void> {
  await invoke("compact", { agentId });
}

/**
 * Clear an agent's conversation history entirely — a fresh start. Backed by
 * the `/new` slash command. If the agent is mid-turn, the turn ends first.
 * Fire-and-forget at call sites.
 */
export async function clearConversation(agentId: AgentId): Promise<void> {
  await invoke("clear_conversation", { agentId });
}

export async function approve(toolCallId: string, approval: Approval): Promise<boolean> {
  return await invoke("approve", { toolCallId, approval });
}

/**
 * The user's answer to an `ask_user` question. `choice` carries the 0-indexed
 * position of the clicked option; `freeform` carries the typed text (the
 * always-present "Let's talk about it" input).
 */
export type UserAnswer =
  | { kind: "choice"; index: number }
  | { kind: "freeform"; text: string };

/**
 * Extract a human-readable message from a thrown IPC error.
 *
 * Backend commands now reject with a structured `{ kind, message }` DTO
 * (`IpcError`), but older code paths and non-IPC throws may still produce a
 * bare string or an `Error`. This normalizes all three shapes to a string so
 * call sites can do `errMsg(e)` instead of `String(e)` (which would render the
 * DTO as `[object Object]`).
 */
export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && "message" in e) {
    const m = (e as { message: unknown }).message;
    if (typeof m === "string") return m;
  }
  return String(e);
}

/**
 * Answer a pending `ask_user` question. Resolves the oneshot the agent is
 * blocked on, so the agent resumes with the answer. Returns true when a
 * matching pending question was found + resolved.
 */
export async function answerQuestion(
  questionId: string,
  answer: UserAnswer,
): Promise<boolean> {
  return await invoke("answer_question", { questionId, answer });
}

export async function setSafetyMode(mode: SafetyMode): Promise<void> {
  await invoke("set_safety_mode", { mode });
}

export async function getSafetyMode(): Promise<SafetyMode> {
  return await invoke("get_safety_mode");
}

/**
 * Read the raw `safety.toml` contents (for the Safety tab editor).
 * Returns an empty string if the file doesn't exist yet.
 */
export async function getSafetyRules(): Promise<string> {
  return await invoke("get_safety_rules");
}

/**
 * Save raw `safety.toml` contents (from the Safety tab editor).
 * The text is validated as TOML before writing; invalid TOML returns an error.
 */
export async function saveSafetyRules(content: string): Promise<void> {
  await invoke("save_safety_rules", { content });
}

/**
 * Add a safety rule for a tool call (the "Mark Safe" action).
 * `args` is the tool call's arguments as a JSON string. Returns the pattern
 * string that was saved.
 */
export async function addSafetyRule(
  tool: string,
  args: string
): Promise<string> {
  return await invoke("add_safety_rule", { tool, args });
}

/**
 * Add a *broad* safety rule that auto-approves any call of the given tool
 * (the "Allow for project" action). Unlike `setSafetyMode`, this is additive
 * and persistent — it adds a `^<tool>:` rule to `safety.toml` without
 * changing the global safety mode. Returns the pattern string that was saved.
 */
export async function addSafetyRuleBroad(tool: string): Promise<string> {
  return await invoke("add_safety_rule_broad", { tool });
}

/**
 * Add a `command_class` safety rule for a shell command (the "Mark Safe (same
 * operation)" action). Classifies the command into a normalized safety class
 * (e.g. `cargo test`) and saves a rule that auto-approves any shell call with
 * the same class — ignoring cosmetic output filtering (`Select-String`,
 * `2>&1`) but still prompting for chained (`&&`, `;`) or unknown commands.
 * Returns the class string that was saved, or throws if the command can't be
 * classified.
 */
export async function addSafetyRuleClass(
  tool: string,
  args: string
): Promise<string> {
  return await invoke("add_safety_rule_class", { tool, args });
}

export async function getStartupError(): Promise<string | null> {
  return await invoke("get_startup_error");
}

export async function listAgents(): Promise<AgentInfo[]> {
  return await invoke("list_agents");
}

/**
 * Per-agent context-window max (in tokens), seeded at startup right after
 * listAgents so each agent's ctx bar shows "0 / <max>" before its first
 * turn. Returns only the max — the used count flows exclusively through the
 * `context_usage` agent event, so a late call can never clobber a live used
 * value with a stale 0.
 */
export async function getContextCaps(): Promise<[number, number][]> {
  return await invoke("context_caps");
}

/**
 * The startup snapshot: everything the frontend needs on mount in one call
 * (agents, context caps, ALL workflow states, backlog, embedder status).
 * Replaces 5 sequential invoke round-trips and closes the
 * stale-non-active-workflowStates gap (the old startup fetched only the
 * active agent's state).
 */
export interface StartupSnapshot {
  agents: AgentInfo[];
  context_caps: [number, number][];
  workflow_states: [number, string][];
  backlog: BacklogItem[];
  embedder_status: string;
  /**
   * Same-project instance conflict: set when another LIVE mnemo instance
   * already holds this project — the app asks before opening it (a second
   * instance is otherwise fine now, thanks to per-instance WebView2
   * profiles). Absent/null on the first instance, and absent on backends
   * that predate the field.
   */
  instance_conflict?: InstanceConflict | null;
}

/**
 * The incumbent claim of another mnemo instance on this project (from
 * `<project>/.coding/instance.json`, resolved at startup before this
 * instance overwrote it).
 */
export interface InstanceConflict {
  /** The pid of the incumbent instance (the one already on this project). */
  pid: number;
  /** Unix epoch seconds when the incumbent instance launched. */
  started_at: number;
}

/**
 * Fetch the startup snapshot in one call. Seeds agents, context caps, ALL
 * workflow states, backlog, and embedder status on mount.
 */
export async function getStartupSnapshot(): Promise<StartupSnapshot> {
  return await invoke("startup_snapshot");
}

/**
 * Spawn a new agent with the given name. Returns the new agent's id + name.
 * The new agent gets its own workflow + tool registry (via the factory) while
 * sharing the provider, memory store, sandbox, and safety rules.
 */
export async function spawnAgent(name: string): Promise<AgentInfo> {
  return await invoke("spawn_agent", { name });
}

/**
 * Get the workflow state + plan for a specific agent. Each agent owns its own
 * workflow, so pass the agent id whose plan you want (typically the active
 * agent in the sidebar).
 */
export async function getWorkflowState(agentId: AgentId): Promise<WorkflowStateInfo> {
  return await invoke("get_workflow_state", { agentId });
}

/**
 * Read a plan by its on-disk id (the file stem of `.coding/plans/<id>.md`).
 * Used by the clickable plan staircase: clicking an ancestor fetches + displays
 * it read-only. Returns the parsed plan or throws on a missing/invalid id.
 */
export async function getPlan(agentId: AgentId, planId: string): Promise<PlanFile> {
  return await invoke("get_plan", { agentId, planId });
}

export interface EndpointInfo {
  name: string;
  kind: string;
  base_url: string;
  models: string[];
  /**
   * Per-model config (caps + reasoning-effort lists), same order as `models`.
   * Absent when no model has per-model config (legacy endpoints).
   */
  model_configs?: ModelConfigInfo[];
  /** Max context window in tokens (null = provider-kind default). */
  max_context?: number | null;
  /** Max output tokens per request (null = max_context / 2). */
  max_output_tokens?: number | null;
  /** Whether models at this endpoint accept image inputs. */
  multimodal?: boolean;
  /**
   * Whether models accept the `reasoning_effort` request field.
   * Default true when absent. When false the field is never sent.
   */
  supports_reasoning_effort?: boolean;
  /** Configured reasoning effort from endpoints.toml (null/absent = default "max"). */
  reasoning_effort?: string | null;
  /**
   * Optional Anthropic workspace id (anthropic endpoints only, from
   * endpoints.toml). Absent on endpoints without one.
   */
  workspace_id?: string | null;
}

/** Per-model config on the wire (the `endpoints[].model_configs[]` element). */
export interface ModelConfigInfo {
  id: string;
  /** Per-model max-context override (null = the endpoint's value). */
  max_context?: number | null;
  /** Per-model max-output override (null = the endpoint's value). */
  max_output_tokens?: number | null;
  /** Per-model reasoning-effort allow-list (empty = the endpoint's list). */
  reasoning_efforts: string[];
  /** Per-model reasoning-effort default (null = the endpoint's value). */
  reasoning_effort?: string | null;
  /** Per-model multimodal (vision) override (null = the endpoint's flag). */
  multimodal?: boolean | null;
}

/**
 * A model id paired with the metadata the provider reports for it: whether
 * it's vision-capable (accepts image inputs) and its token caps (context
 * window + per-request output limit). From the `list_models` /
 * `list_vision_models` IPC commands, which fetch the endpoint's live
 * `/models` list. The caps are `null` when the provider exposes no such
 * field (vanilla OpenAI / z.ai) — discovery is best-effort.
 */
export interface VisionModelInfo {
  id: string;
  vision_capable: boolean;
  /** Context window in tokens as reported by the provider; null = unreported. */
  context_length?: number | null;
  /** Per-request output-token cap as reported by the provider; null = unreported. */
  max_output_tokens?: number | null;
}

/** Per-model pricing ($ per 1M tokens). From the [[pricing]] table in endpoints.toml. */
export interface PricingEntry {
  model: string;
  input_per_1m: number;
  output_per_1m: number;
  cached_per_1m: number;
}

/** Vision fallback model from config.toml [general.vision_model]. */
export interface VisionModelConfig {
  endpoint: string;
  model: string;
}

/** Embedding model from config.toml [general.embedding_model]. */
export interface EmbeddingModelConfig {
  endpoint: string;
  model: string;
}

/** A per-context model override (one entry of the `[models]` section). */
export interface ModelRefConfig {
  endpoint: string;
  model: string;
  /** Optional per-context reasoning-effort override (null/absent = the
   *  model's own default applies). */
  reasoning_effort?: string | null;
}

/** The `[models]` section — per-context model overrides. */
export interface ModelsConfig {
  /** Model override for the Planning state (null = use default). */
  planning: ModelRefConfig | null;
  /** Model override for the Executing state (null = use default). */
  executing: ModelRefConfig | null;
  /** Model + reasoning effort for the active plan's bug-fixing lifecycle:
   *  while the active plan's kind is bug_fixing and the workflow is in the
   *  Executing or Reviewing state, this slot wins over `executing` (null =
   *  inherit the executing override, then default). */
  bug_fixing: ModelRefConfig | null;
  /** Model override for the Reviewing state (null = use default; falls back
   *  to the executing override at resolution time). */
  reviewing: ModelRefConfig | null;
  /** Model override for the Complete state (null = use default). */
  complete: ModelRefConfig | null;
  /** Model override for subagents (null = use default). */
  subagent: ModelRefConfig | null;
  /** Model for compaction summaries — the auto-compaction summary call and
   *  the run-all between-items compact (null = ride the turn's model). */
  summarize: ModelRefConfig | null;
  /** Per-skill overrides, keyed by skill name. */
  skill: Record<string, ModelRefConfig>;
}

/** Full Settings payload from `get_settings` (no secrets). */
export interface AppSettings {
  config_dir: string;
  general: {
    default_provider?: string | null;
    default_model?: string | null;
    safety: string;
    vision_model?: VisionModelConfig | null;
    embedding_model?: EmbeddingModelConfig | null;
    bundled_embedding_model?: string | null;
    /** Whether the agent's `browser_*` browser-inspection tools are enabled
     *  (exposes an unauthenticated localhost CDP port — opt-in, off by
     *  default; debug builds always expose it regardless). */
    enable_browser_inspection: boolean;
    /** Whether a Run-All loop auto-compacts the main agent's context
     *  between items (after each completed plan, before the next item is
     *  dispatched). Run-All only — interactive completions never trigger
     *  it. Opt-in, off by default. */
    auto_compact_on_plan_complete: boolean;
  };
  context: {
    summarize_at_fill_rate: number;
    proxy_cache_ceiling_tokens: number | null;
  };
  ui: {
    theme: string;
    show_token_usage: boolean;
    /** Inline images from image commands in the agent chat. */
    show_tool_images: boolean;
    /** Agent-activity cards (tool/memory/vision/skill) in the chat
     *  transcript — GUI-only display filter, default on (tool results show
     *  by default; set false to hide). */
    show_tool_activity: boolean;
    /** Knowledge-access activity cards (graph_* tool calls, memory tool
     *  calls, auto-recall entries) in the chat transcript — GUI-only
     *  display filter, default on. */
    show_knowledge_activity: boolean;
    /** The AUTO-DELEGATED steering line in search/search_read
     *  tool results — GUI-only display filter, default off. */
    show_delegation_notes: boolean;
    /** Vertical thread line along consecutive activity cards in the chat
     *  transcript — GUI-only display filter, default on. */
    chat_thread_line: boolean;
    /** Cap assistant prose at ~100 columns (code blocks and tool outputs
     *  stay full width) — GUI-only display filter, default on. */
    chat_prose_cap: boolean;
    /** Alternating faint background band per conversation turn — GUI-only
     *  display filter, default on. */
    chat_turn_tint: boolean;
    /** Transcript entry creation time as a hover tooltip — GUI-only
     *  display filter, default on. */
    chat_hover_timestamps: boolean;
    /** Ding when an agent's plan reaches Complete. */
    sound_complete: boolean;
    /** Ping when an agent needs user input (approval/question). */
    sound_input_needed: boolean;
    /** Doom tone when repeated errors stop an agent. */
    sound_stopped_errors: boolean;
  };
  /** Per-context model overrides (`[models]` section). */
  models: ModelsConfig;
  /** Markdown viewer settings (`[markdown]` section). */
  markdown: { skip_dirs: string[] };
  /** Git settings (`[git]` section) — subcommands that always require approval. */
  git: { core_operations: string[] };
  /** Trace-log memory limits (`[trace]` section) — Advanced settings. */
  trace: { memory_budget_mb: number; request_body_cap_kb: number };
  /** Memory retrieval/indexing scale knobs (`[memory]` section, Phase 2). */
  memory: {
    /** Recency half-life (days) for the strength decay in recall ranking. */
    decay_half_life_days: number;
    /** Default per-query result cap (an explicit filter limit always wins). */
    per_query_cap: number;
    /** Max derived records of one record type per recall result. */
    derived_per_class_cap: number;
    /** Digest auto-truncation lengths (chars) for derived/captured digests. */
    plan_budget: number;
    bug_budget: number;
    spec_budget: number;
    decision_budget: number;
    review_budget: number;
  };
  endpoints: EndpointInfo[];
  pricing: PricingEntry[];
  projects: { name: string; path: string }[];
}

/** Patch for `save_settings` — only set fields you want to change. */
export interface SettingsSavePatch {
  default_provider?: string;
  default_model?: string;
  clear_default_provider?: boolean;
  clear_default_model?: boolean;
  safety?: string;
  vision_model?: VisionModelConfig;
  clear_vision_model?: boolean;
  embedding_model?: EmbeddingModelConfig;
  clear_embedding_model?: boolean;
  bundled_embedding_model?: string;
  clear_bundled_embedding_model?: boolean;
  summarize_at_fill_rate?: number;
  proxy_cache_ceiling_tokens?: number | null;
  theme?: string;
  show_token_usage?: boolean;
  show_tool_images?: boolean;
  show_tool_activity?: boolean;
  show_knowledge_activity?: boolean;
  show_delegation_notes?: boolean;
  chat_thread_line?: boolean;
  chat_prose_cap?: boolean;
  chat_turn_tint?: boolean;
  chat_hover_timestamps?: boolean;
  sound_complete?: boolean;
  sound_input_needed?: boolean;
  sound_stopped_errors?: boolean;
  /**
   * Per-context model overrides patch. Each field is `null` to clear the
   * override, an object to set it, or omitted (absent key) to keep the
   * existing value. `skill`, when present, fully replaces the per-skill map.
   */
  models?: {
    planning?: ModelRefConfig | null;
    executing?: ModelRefConfig | null;
    bug_fixing?: ModelRefConfig | null;
    reviewing?: ModelRefConfig | null;
    complete?: ModelRefConfig | null;
    subagent?: ModelRefConfig | null;
    summarize?: ModelRefConfig | null;
    skill?: Record<string, ModelRefConfig>;
  };
  pricing?: PricingEntry[];
  /** When present, fully replaces `[markdown].skip_dirs`. */
  skip_dirs?: string[];
  /** When present, fully replaces `[git].core_operations` (git subcommands
   *  that always require approval, even in Autonomous mode / under a safety
   *  rule). Entries are trimmed + lowercased by the backend. */
  core_operations?: string[];
  /** Patch the `[general].enable_browser_inspection` flag (whether the
   *  agent's `browser_*` browser-inspection tools expose the CDP port). The
   *  change takes effect on the next app restart (the env var is read at
   *  WebView2 creation time). */
  enable_browser_inspection?: boolean;
  /** Patch `[general].auto_compact_on_plan_complete` — the Run-All
   *  between-items auto-compact (compact the main agent's context after
   *  each completed plan, before the next item is dispatched). Takes
   *  effect on the next item resolution; Run-All loops only. */
  auto_compact_on_plan_complete?: boolean;
  /** Patch `[trace].memory_budget_mb` — total raw-response byte budget
   *  across the in-memory trace ring, MiB (1–512). Applied to the LIVE
   *  trace log immediately; oldest payloads evicted first. */
  trace_memory_budget_mb?: number;
  /** Patch `[trace].request_body_cap_kb` — per-record request-body cap, KiB
   *  (16–8192). Applies to requests recorded after the save. */
  trace_request_body_cap_kb?: number;
  /** Patch the `[memory]` retrieval/indexing knobs (Phase 2) — each field
   *  optional, omitted fields keep their values. Applied to the live store
   *  snapshot on save (no restart). */
  memory?: {
    decay_half_life_days?: number;
    per_query_cap?: number;
    derived_per_class_cap?: number;
    plan_budget?: number;
    bug_budget?: number;
    spec_budget?: number;
    decision_budget?: number;
    review_budget?: number;
  };
}

/** Load full Settings (general/context/memory/ui/vision/pricing/paths). */
export async function getSettings(): Promise<AppSettings> {
  return await invoke("get_settings");
}

/**
 * Persist non-endpoint Settings sections. Reloads config on success and
 * updates runtime safety mode when `safety` is included.
 */
export async function saveSettings(
  patch: SettingsSavePatch,
): Promise<{ ok: boolean; safety: string | null }> {
  return await invoke("save_settings", { patch });
}

/**
 * Get the live embedder status ("ready" / "checking" / "fallback" /
 * "pulling" / "failed"). Polled on startup; updated via the
 * `embedder://status` event so the UI can show a banner when semantic recall
 * has degraded to keyword-only.
 */
export async function getEmbedderStatus(): Promise<string> {
  return await invoke("get_embedder_status");
}

/** Subscribe to embedder status changes (startup probe / circuit transitions). */
export function onEmbedderStatus(
  handler: (status: string) => void,
): Promise<UnlistenFn> {
  return listen<string>("embedder://status", (event) => {
    handler(event.payload);
  });
}

// ── Bundled embedding models (fastembed, in-process) ───────────────────────

/** A catalog entry for a bundled embedding model. */
export interface BundledModelInfo {
  id: string;
  name: string;
  dim: number;
  size_mb: number;
  /** Whether the model is already downloaded on this machine. */
  installed: boolean;
}

/** List the curated bundled embedding models, each flagged `installed`. */
export async function listBundledEmbeddingModels(): Promise<BundledModelInfo[]> {
  return await invoke("list_bundled_embedding_models");
}

/**
 * Download a bundled model in the background. Emits `embedder://status`
 * events with a `downloading` payload (carrying model + progress 0–1) as it
 * goes, then `ready` on completion (or `failed` on error).
 */
export async function downloadBundledModel(model: string): Promise<void> {
  await invoke("download_bundled_model", { model });
}

// ── Memory debug (the right-panel "Memory" tab) ─────────────────────────────

/** A memory row as sent to the Memory debug tab (content truncated to 500 chars). */
export interface MemoryDebugWire {
  id: string;
  tier: string;
  title: string;
  content: string;
  strength: number;
  access_count: number;
  created_at: number;
  last_accessed_at: number;
  source_session_ids: string[];
  data: unknown;
}

/** A scored memory result from a recall test (the memory + its relevance score). */
export interface ScoredMemoryDebugWire {
  id: string;
  tier: string;
  title: string;
  content: string;
  strength: number;
  access_count: number;
  created_at: number;
  last_accessed_at: number;
  source_session_ids: string[];
  data: unknown;
  score: number;
}

/** Per-tier memory counts. */
export interface MemoryCounts {
  working: number;
  episodic: number;
  semantic: number;
  procedural: number;
  total: number;
}

/**
 * The overview shown at the top of the Memory debug tab: the embedder status,
 * the active model + dimension, the configured model, the stored vector
 * fingerprints (for mismatch detection), and per-tier counts.
 */
export interface MemoryDebugOverview {
  /** The embedder status wire form (a string for unit variants, or an object
   *  for the downloading variant: `{"downloading": {model, progress}}`). */
  status: string | Record<string, unknown>;
  /** The active embedder's model id (e.g. `"all-MiniLM-L6-v2"`, or `"hash"`
   *  for the offline fallback). `"<none>"` when no memory store is wired. */
  model_id: string;
  /** The active embedder's vector dimension. */
  dim: number;
  /** The configured bundled model id from config (`null` = none / hash mode). */
  configured_model: string | null;
  /** The set of `[model_id, dim]` fingerprints present across stored memories. */
  fingerprints: [string, number][];
  /** Whether the active embedder's `(model_id, dim)` is present in the stored
   *  fingerprints. `false` = stored vectors are in a different space (recall
   *  degraded; a re-embed is pending or needed). */
  fingerprint_matches: boolean;
  /** Per-tier memory counts. */
  counts: MemoryCounts;
}

/**
 * The overview shown at the top of the Memory debug tab: embedder status +
 * model, stored vector fingerprints (mismatch detection), and per-tier counts.
 * Returns `model_id = "<none>"` + zeroed counts when no memory store is wired.
 */
export async function memoryDebugOverview(): Promise<MemoryDebugOverview> {
  return await invoke("memory_debug_overview");
}

/**
 * List memories, optionally filtered by tier, with content truncated to 500
 * chars. `tier = null` lists across all four tiers. `limit` caps the result
 * count (applied per-tier when `tier` is null). Throws when no memory store is
 * wired.
 */
export async function memoryDebugList(
  tier: string | null,
  limit?: number,
): Promise<MemoryDebugWire[]> {
  return await invoke("memory_debug_list", { tier, limit: limit ?? null });
}

/**
 * Run a recall test against the memory store and return the scored results
 * (memory + relevance score). Includes working-tier memories (unlike the
 * production auto-recall) so the debug view can surface raw tool-event
 * snapshots too. Throws when no memory store is wired.
 */
export async function memoryDebugRecall(
  query: string,
  limit?: number,
): Promise<ScoredMemoryDebugWire[]> {
  return await invoke("memory_debug_recall", { query, limit: limit ?? null });
}

/** A resolved `[[wiki-link]]` target — what a link points at in the current
 *  index (kind, path, the target record's id/title/snippet when resolved). */
export interface ResolvedLink {
  /** `"knowledge"` / `"plan"` / `"review"` / `"file"` / `"other"`. */
  kind: string;
  /** The original target, verbatim. */
  target: string;
  /** The repo-relative path of the target's file, when meaningful. */
  rel_path: string | null;
  /** The derived memory id of the target record, when one exists. */
  memory_id: string | null;
  /** The target record's title, when resolved. */
  title: string | null;
  /** The target record's content (truncated), when resolved. */
  snippet: string | null;
}

/** Resolve one `[[wiki-link]]` target to its current record. Returns `null`
 *  for malformed targets; a missing record resolves to a path-only entry. */
export async function memoryDebugResolveLink(
  target: string,
): Promise<ResolvedLink | null> {
  return await invoke("memory_debug_resolve_link", { target });
}

/** Every live memory whose `data.links` points at `rel` (the "referenced by"
 *  reverse surface for the knowledge UI). */
export async function memoryDebugBacklinks(
  rel: string,
): Promise<MemoryDebugWire[]> {
  return await invoke("memory_debug_backlinks", { rel });
}

/** One entry of the rolling memory-access log (the Graph tab's "Memory
 *  access" section): a read or write that went through the memory store.
 *  Mirrors the Rust `MemoryAccessEntry` wire shape. */
export interface MemoryAccessEntry {
  /** Unix timestamp (seconds) of the access. */
  at: number;
  /** `"read"` (recall / session primer) or `"write"`. */
  op: string;
  /** The tier a write targeted / a read filtered on (`null` = unfiltered). */
  tier: string | null;
  /** What was searched for or written (query / title), bounded to one line. */
  detail: string;
  /** How many memories a read returned (`null` for writes). */
  hits: number | null;
}

/**
 * The rolling last-100 memory reads + writes (newest first), for the Graph
 * tab's "Memory access" section. Returns an empty array when no memory store
 * is wired (degrades to an empty section, never throws for that reason).
 */
export async function memoryAccessLog(): Promise<MemoryAccessEntry[]> {
  return await invoke("memory_access_log");
}

// ── Memory maintenance (Settings → Memory) ──────────────────────────────────

/** Which maintenance operation an event belongs to. */
export type MaintenanceOp = "cleanup" | "rebuild" | "index";

/** An event on the `memory://maintenance` stream (`"type"`-tagged, kebab-case
 *  — pinned by the backend's `maintenance_event_wire_shape` test). `progress`
 *  carries `total = 0` for label-only phases with no measurable units
 *  ("reindexing", "vacuuming"). */
export type MaintenanceEvent =
  | { type: "started"; op: MaintenanceOp }
  | {
      type: "progress";
      op: MaintenanceOp;
      done: number;
      total: number;
      phase: string;
    }
  | { type: "done"; op: MaintenanceOp; summary: string }
  | { type: "failed"; op: MaintenanceOp; error: string };

/**
 * Start a memory cleanup in the background: consolidates every non-live
 * session that still holds raw working-tier events into episodic summaries
 * (with semantic/procedural extraction when a default LLM endpoint is
 * configured), deletes the raw rows, and compacts the database. Progress +
 * the terminal summary arrive via `onMaintenanceEvent`; the command itself
 * returns immediately (throws when no memory store is wired or another
 * maintenance operation is already running).
 */
export async function maintenanceCleanup(): Promise<void> {
  await invoke("memory_cleanup");
}

/**
 * Rebuild the semantic search index in the background: re-embeds every
 * memory with the active embedding model, rebuilds the FTS5 full-text index,
 * and compacts the database — the recovery path for stale/mixed embedding
 * fingerprints or a drifted index. Progress + the terminal summary arrive via
 * `onMaintenanceEvent`; the command itself returns immediately.
 */
export async function maintenanceRebuildSearch(): Promise<void> {
  await invoke("memory_rebuild_search");
}

/**
 * Wipe + re-scan the derived index in the background (budgeted digests of
 * `.coding/plans`, `.coding/reviews`, and the pending backlog items) —
 * authored memories are never touched. Progress + the terminal summary
 * arrive via `onMaintenanceEvent` with op `"index"`; the command itself
 * returns immediately (throws when no memory store is wired or another
 * maintenance operation is already running).
 */
export async function maintenanceRebuildIndex(): Promise<void> {
  await invoke("memory_rebuild_index");
}

/** The derived-index status behind the Settings → Memory card. */
export interface MemoryIndexStatus {
  /** Authored (agent/user-written) memory count. */
  authored_count: number;
  /** Derived (indexer-built digest) memory count. */
  derived_count: number;
  /** Unix seconds of the most recent index run; `null` when never indexed. */
  last_built_at: number | null;
}

/** Read the derived-index status (counts by class + last-built timestamp). */
export async function memoryIndexStatus(): Promise<MemoryIndexStatus> {
  return await invoke("memory_index_status");
}

/** Subscribe to memory-maintenance progress events (Settings → Memory). */
export function onMaintenanceEvent(
  handler: (event: MaintenanceEvent) => void,
): Promise<UnlistenFn> {
  return listen<MaintenanceEvent>("memory://maintenance", (event) => {
    handler(event.payload);
  });
}

// ── Startup reconciliation (memory DB ↔ on-disk truth) ─────────────────────

/** An event on the `memory://reconcile` stream (`"type"`-tagged) — emitted
 *  only when the startup derived-index reconciliation actually runs (the
 *  corpus drifted: a git merge landed, a record was edited, or memory.db was
 *  deleted and is being rebuilt from the files). When the corpus is in sync
 *  NO events fire and no dialog appears. */
export type ReconcileEvent =
  | { type: "started" }
  | { type: "progress"; done: number; total: number }
  | { type: "done"; summary: string }
  | { type: "failed"; error: string };

/** Subscribe to startup-reconciliation events (the wait dialog's stream). */
export function onReconcileEvent(
  handler: (event: ReconcileEvent) => void,
): Promise<UnlistenFn> {
  return listen<ReconcileEvent>("memory://reconcile", (event) => {
    handler(event.payload);
  });
}

// ── CodeGraph (the right-panel Graph tab) ────────────────────────────────────

/** One symbol in the code knowledge graph (the `Symbol` wire form). */
export interface GraphSymbol {
  /** Stable id: `{relpath}::{name}::{start_line}`. */
  id: string;
  /** The short declared name. */
  name: string;
  /** The symbol kind (snake_case: `function`, `method`, `struct`, …). */
  kind: string;
  /** Project-relative path with `/` separators. */
  file: string;
  /** 1-based line where the definition starts. */
  start_line: number;
  /** 1-based line where the definition ends. */
  end_line: number;
}

/** One directed edge between two symbols (the `EdgeRow` wire form). */
export interface GraphEdge {
  /** Source symbol id. */
  from_id: string;
  /** Target symbol id. */
  to_id: string;
  /** Edge kind: `calls` | `imports` | `contains`. */
  kind: string;
}

/** The Graph tab's status line: availability, counts, indexing flag. */
export interface CodegraphStatus {
  /** False when codegraph is disabled / failed to open / brain failed. */
  available: boolean;
  /** True while an indexing pass is running (startup or a refresh). */
  indexing: boolean;
  /** Number of indexed files — every searchable file (source + content-only). */
  files: number;
  /** Number of stored symbols. */
  symbols: number;
  /** Number of stored edges. */
  edges: number;
  /** Unix seconds of the most recent index pass, if any. */
  last_indexed_at: number | null;
}

/** A visualization subgraph from `codegraph_graph`. */
export interface CodegraphGraph {
  /** The selected symbols, id-sorted. */
  nodes: GraphSymbol[];
  /** Edges between the selected symbols only. */
  edges: GraphEdge[];
}

/** The Graph tab's status: availability, counts, indexing flag. Never errors. */
export async function codegraphStatus(): Promise<CodegraphStatus> {
  return await invoke("codegraph_status");
}

/**
 * Trigger a background re-index; returns immediately with the current status
 * (poll `codegraphStatus` to observe the indexing flag). Throws when the
 * graph is unavailable.
 */
export async function codegraphRefresh(): Promise<CodegraphStatus> {
  return await invoke("codegraph_refresh");
}

/** An event on the `codegraph://maintenance` stream — the same
 *  `"type"`-tagged shape as the memory-maintenance events, minus the `op`
 *  router (one operation lives on this channel). Pinned by the backend's
 *  `reindex_event_wire_shape` test; folded by the shared
 *  `applyMaintenanceEvent` mapping. */
export type ReindexEvent =
  | { type: "started" }
  | { type: "progress"; done: number; total: number; phase: string }
  | { type: "done"; summary: string }
  | { type: "failed"; error: string };

/**
 * Rebuild the code content index (symbols + FTS search rows) in the
 * background — the Settings → Memory & Search "Rebuild code search index"
 * action. Progress + the terminal summary arrive via `onReindexEvent`; the
 * command itself returns immediately (throws when the graph is unavailable
 * or an indexing pass is already running).
 */
export async function codegraphRebuildIndex(): Promise<void> {
  await invoke("codegraph_rebuild_index");
}

/** Subscribe to code-index rebuild progress (Settings → Memory & Search). */
export function onReindexEvent(
  handler: (event: ReindexEvent) => void,
): Promise<UnlistenFn> {
  return listen<ReindexEvent>("codegraph://maintenance", (event) => {
    handler(event.payload);
  });
}

/** An event on the `codegraph://index-progress` stream — the indexing
 *  progress behind the open-project overlay (startup pass + create-project
 *  seed pass). Same `"type"`-tagged shape as the reconcile events, with a
 *  phase-less `progress` (a code index has exactly one phase). Pinned by the
 *  backend's `index_progress_event_wire_shape` test; folded by the pure
 *  `applyIndexProgress` mapping in IndexingOverlay.tsx. */
export type IndexProgressEvent =
  | { type: "started" }
  | { type: "progress"; done: number; total: number }
  | { type: "done"; summary: string }
  | { type: "failed"; error: string };

/** Subscribe to the open-project indexing progress
 *  (`codegraph://index-progress`) — drives the IndexingOverlay's progress
 *  bar + "N/M files indexed" counter during project open/creation. */
export function onIndexProgress(
  handler: (event: IndexProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<IndexProgressEvent>("codegraph://index-progress", (event) => {
    handler(event.payload);
  });
}

/** The live startup indexing pass's progress, as returned by
 *  `get_index_progress` — `null` when no startup pass is running. An overlay
 *  that mounted after the pass began (the webview is recreated on every app
 *  start AND every project switch) fetches this once after subscribing to
 *  catch up instead of waiting for the next throttled tick. Mirrors the
 *  backend's `IndexProgressSnapshot` (src-tauri/src/ipc/codegraph_cmds.rs). */
export interface IndexProgressSnapshot {
  /** Files processed so far. */
  done: number;
  /** Total source files the pass will walk. */
  total: number;
}

/** Fetch the startup indexing pass's live progress snapshot — `null` when no
 *  startup pass is running (or it already ended). Late-mounting
 *  IndexingOverlay instances call this right after subscribing. */
export async function getIndexProgress(): Promise<IndexProgressSnapshot | null> {
  return await invoke("get_index_progress");
}

/**
 * Fetch a visualization subgraph. With no args: the top ~300 nodes by
 * connectivity. `name` (case-insensitive substring) and `kind`
 * (`"function"`, `"struct"`, …) narrow it; `fromSymbol` (+ `depth`, default
 * 1) takes that symbol's both-directions neighborhood instead. An unknown
 * `fromSymbol` yields an empty graph, not an error.
 */
export async function codegraphGraph(opts?: {
  name?: string | null;
  kind?: string | null;
  fromSymbol?: string | null;
  depth?: number | null;
}): Promise<CodegraphGraph> {
  return await invoke("codegraph_graph", {
    name: opts?.name ?? null,
    kind: opts?.kind ?? null,
    fromSymbol: opts?.fromSymbol ?? null,
    depth: opts?.depth ?? null,
  });
}

/**
 * Get the API keys for every configured endpoint, keyed by endpoint name.
 * Endpoints with no stored key are omitted (treat an absent entry as an empty
 * password field). This is a SEPARATE command from `getConfig` (which never
 * includes secrets) so keys are only fetched when the Settings → Endpoints
 * tab is open.
 */
export async function getApiKeys(): Promise<Record<string, string>> {
  return await invoke("get_api_keys");
}

/**
 * Fetch the live model list (with per-model token caps) from an endpoint's
 * `/models` endpoint (used by the Settings → Endpoints tab's model picker and
 * cap auto-fill). OpenAI/Local kinds hit the OpenAI-compatible endpoint
 * (Bearer auth); `kind: "anthropic"` sends the `x-api-key` +
 * `anthropic-version` headers (the Anthropic Models API returns no caps, so
 * `context_length`/`max_output_tokens` come back null).
 *
 * Resolution mirrors the provider's key fallback chain: the passed
 * `baseUrl`/`apiKey` win over a saved endpoint named `endpointName`, which
 * then falls back to the kind-appropriate env vars, then `"dummy"` (local
 * endpoints). Pass `baseUrl`/`apiKey` when the card has unsaved edits so the
 * picker reflects what's on screen, not what's on disk.
 *
 * Returns the sorted, de-duplicated entries (`{ id, vision_capable,
 * context_length, max_output_tokens }`; both caps null when the provider
 * reports none). Throws a human-readable error string on HTTP/parse failure
 * (no key is leaked in the message).
 */
export async function listModels(
  endpointName: string,
  baseUrl?: string,
  apiKey?: string,
  kind?: string,
): Promise<VisionModelInfo[]> {
  return await invoke("list_models", {
    endpointName,
    baseUrl: baseUrl ?? null,
    apiKey: apiKey ?? null,
    kind: kind ?? null,
  });
}

/**
 * Fetch the live model list from an endpoint's `/models`
 * endpoint, each annotated with a vision-capability flag — used by the
 * Settings → Vision tab's model picker so the user can choose a vision-capable
 * model served by that endpoint.
 *
 * Resolution mirrors `listModels` (passed `baseUrl`/`apiKey` → saved endpoint
 * → env vars → "dummy"; `kind: "anthropic"` uses the Anthropic headers, and
 * Anthropic endpoints report no modality so every model comes back
 * `vision_capable: false`). `vision_capable` is `true` only when the provider
 * explicitly exposes image input modality (e.g. OpenRouter); providers that
 * don't expose modality report `false` for every model, and the UI falls back
 * to showing all models with a note. Throws a human-readable error string on
 * HTTP/parse failure (no key is leaked in the message).
 */
export async function listVisionModels(
  endpointName: string,
  baseUrl?: string,
  apiKey?: string,
  kind?: string,
): Promise<VisionModelInfo[]> {
  return await invoke("list_vision_models", {
    endpointName,
    baseUrl: baseUrl ?? null,
    apiKey: apiKey ?? null,
    kind: kind ?? null,
  });
}

/**
 * The editable shape of an endpoint, matching the Rust `EndpointDto`. `kind`
 * is a string ("openai"/"local"/"anthropic") from the kind dropdown.
 * `api_key` is NOT here — keys travel in a separate `apiKeys` map keyed by
 * endpoint name.
 */
/** Editable per-model config inside an endpoint (mirrors ModelConfigInfo). */
export interface ModelConfigEditable {
  id: string;
  max_context?: number | null;
  max_output_tokens?: number | null;
  reasoning_efforts: string[];
  /** Per-model reasoning-effort default (null = the endpoint's value). */
  reasoning_effort?: string | null;
  /** Per-model multimodal (vision) override (null = the endpoint's flag). */
  multimodal?: boolean | null;
}

/**
 * One endpoint as edited in the Settings → Endpoints tab. `kind` on the wire
 * is a string ("openai"/"local"/"anthropic") from the kind dropdown.
 * `api_key` is NOT here — keys travel in a separate `apiKeys` map keyed by
 * endpoint name.
 */
export interface EndpointEditable {
  name: string;
  /** "openai", "local", or "anthropic" (also accepts "OpenAI"/"Local" on save). */
  kind: string;
  base_url: string;
  models: string[];
  /**
   * Per-model config, parallel to `models` by position (the backend matches
   * by id, so order drift is tolerated). Absent/empty = no per-model config.
   */
  model_configs?: ModelConfigEditable[];
  max_context?: number | null;
  max_output_tokens?: number | null;
  multimodal: boolean;
  /**
   * Whether models accept `reasoning_effort`. Default true.
   * When false the request field is never sent and the effort UI is hidden.
   */
  supports_reasoning_effort: boolean;
  /** null/absent = endpoint default ("max"); "off" = omit the field. */
  reasoning_effort?: string | null;
  /**
   * Optional Anthropic workspace id (sent as the `anthropic-workspace-id`
   * header on every request). Anthropic endpoints only; null/absent = not set
   * (the header is never sent empty).
   */
  workspace_id?: string | null;
}

/**
 * Persist the full edited endpoint set + API keys + default provider/model.
 * The Endpoints tab sends everything (it is the source of truth for the save,
 * not a diff). Returns `{ default_provider, default_model, provider_swapped }`.
 *
 * On success the backend writes endpoints.toml / keys.toml / config.toml,
 * reloads config, and (if the default endpoint/model changed) rebuilds +
 * swaps the live provider into every agent. Throws an error string on
 * validation/write failure (no partial state change).
 */
export async function saveEndpoints(
  endpoints: EndpointEditable[],
  apiKeys: Record<string, string>,
  defaultProvider: string | null,
  defaultModel: string | null,
): Promise<{
  default_provider: string | null;
  default_model: string | null;
  provider_swapped: boolean;
}> {
  return await invoke("save_endpoints", {
    endpoints,
    apiKeys,
    defaultProvider: defaultProvider ?? null,
    defaultModel: defaultModel ?? null,
  });
}

/**
 * Switch the active model (or reasoning effort) at runtime — per agent
 * (models are agent-specific, user decision 2026-08-22).
 *
 * `agentId` selects the agent to switch; pass `null` for the legacy global
 * switch (factory + every live agent). The provider for the chosen endpoint +
 * model is rebuilt and swapped into that agent's loop only (takes effect on
 * its next turn); other agents keep their models.
 *
 * `reasoningEffort` is the raw dropdown value (`max`/`high`/`medium`/`low`/
 * `minimal`/`off`). The backend maps `"off"` to omitting the field from
 * requests entirely; when omitted (`undefined`/`null`), it falls back to the
 * endpoint's configured value (default `max`).
 */
export async function setModel(
  agentId: number | null,
  endpointName: string,
  model: string,
  reasoningEffort?: string
): Promise<void> {
  await invoke("set_model", {
    agentId,
    endpointName,
    model,
    reasoningEffort: reasoningEffort ?? null,
  });
}

// ── Project picker commands (src-tauri/src/ipc/projects.rs) ─────────────────

/** A registered project (name → path), from `projects.toml`. */
export interface ProjectInfo {
  name: string;
  path: string;
}

/**
 * Whether the app started without a resolved project and is waiting for the
 * user to pick one. The frontend checks this on mount (before
 * `getStartupError`) and shows the project picker instead of the normal UI
 * when it returns `true`.
 */
export async function getNeedsProject(): Promise<boolean> {
  return await invoke("get_needs_project");
}

/** List the registered projects (name → path) from `projects.toml`. */
export async function listProjects(): Promise<ProjectInfo[]> {
  return await invoke("list_projects");
}

/**
 * Create a project at `path`: scaffold `.coding/` + the root `agent.md`
 * template (idempotent), register it in `projects.toml` under `name`
 * (defaulting to the directory's basename), reload config, and return the
 * project path. The caller then calls `switchProject(path)` to restart into it.
 */
export async function createProject(
  path: string,
  name?: string,
): Promise<string> {
  return await invoke("create_project", { path, name: name ?? null });
}

/**
 * Switch to a project: writes its path to a one-shot marker, then restarts
 * the app into that project. Does not return on success (the process is
 * relaunched). `path` must be an existing directory containing a `.coding/`
 * directory (a Mnemo project) — the command otherwise rejects with a clear
 * error before restarting, instead of silently re-showing the picker after
 * the reload (backlog 16e4a7f8).
 */
export async function switchProject(path: string): Promise<void> {
  await invoke("switch_project", { path });
}

/** Remove a project from the registry (does not delete files on disk). */
export async function removeProject(name: string): Promise<void> {
  await invoke("remove_project", { name });
}

/**
 * Open a native folder picker and return the chosen directory path, or `null`
 * if the user cancelled. Used by the picker's "create new project" flow.
 */
export async function pickDirectory(): Promise<string | null> {
  return await invoke("pick_directory");
}

// ── Stats types (mirror the Rust structs in src/memory/types.rs) ────────────

/** Token usage for a single model × endpoint within a session or project. */
export interface ModelBreakdown {
  model: string;
  /** The serving endpoint's name; absent/null groups pre-endpoint rows —
   *  the same model on different endpoints is separate rows. */
  endpoint?: string | null;
  prompt_tokens: number;
  completion_tokens: number;
  reasoning_tokens: number;
  cached_tokens: number;
  ttft_ms_total: number;
  generation_ms_total: number;
  timed_requests: number;
  request_count: number;
}

/** Aggregated token usage + timing for a single session. */
export interface SessionStats {
  session_id: string;
  prompt_tokens: number;
  completion_tokens: number;
  reasoning_tokens: number;
  cached_tokens: number;
  ttft_ms_total: number;
  generation_ms_total: number;
  timed_requests: number;
  request_count: number;
  per_model: ModelBreakdown[];
  created_at: number;
  ended_at: number | null;
}

/** One day's worth of token usage (for the project time series). */
export interface DayBreakdown {
  day: number;
  prompt_tokens: number;
  completion_tokens: number;
  reasoning_tokens: number;
  cached_tokens: number;
  request_count: number;
}

/** Aggregated token usage + timing across the whole project (all sessions). */
export interface ProjectStats {
  prompt_tokens: number;
  completion_tokens: number;
  reasoning_tokens: number;
  cached_tokens: number;
  ttft_ms_total: number;
  generation_ms_total: number;
  timed_requests: number;
  request_count: number;
  session_count: number;
  per_model: ModelBreakdown[];
  per_day: DayBreakdown[];
}

/** A session summary row (for listing sessions in the Stats tab). */
export interface SessionSummary {
  session_id: string;
  created_at: number;
  ended_at: number | null;
  request_count: number;
  prompt_tokens: number;
  completion_tokens: number;
  reasoning_tokens: number;
}

/**
 * Get per-session token + timing stats for the agent with the given id.
 * Errors if the agent has no session yet (no prompt sent).
 */
export async function getSessionStats(agentId: AgentId): Promise<SessionStats> {
  return await invoke("get_session_stats", { agentId });
}

/** Get per-project token + timing stats (aggregated across all sessions). */
export async function getProjectStats(): Promise<ProjectStats> {
  return await invoke("get_project_stats");
}

/** Get the list of sessions with aggregated token counts (newest first). */
export async function getSessionList(): Promise<SessionSummary[]> {
  return await invoke("get_session_list");
}

export async function readFile(path: string): Promise<string> {
  return await invoke("read_file", { path });
}

/**
 * Read an image from the project (sandbox-relative path, forward slashes) as
 * a base64 `data:` URL for inline `<img>` rendering. Backend-validated: the
 * path is sandbox-checked, the extension must be an image type, and the file
 * is size-capped — so the chat can only ever render real, bounded images.
 */
export async function readImageDataUrl(path: string): Promise<string> {
  return await invoke("read_image_data_url", { path });
}

/**
 * Persist a FileViewer markdown edit (the editor's Save button / Ctrl+S).
 * Backend-validated through the project sandbox — paths outside the project
 * root are rejected, and protected `.coding` live-state files are refused.
 * Returns the written path.
 */
export async function writeFile(path: string, content: string): Promise<string> {
  return await invoke("write_file", { path, content });
}

export async function listFiles(dir?: string): Promise<FileEntry[]> {
  return await invoke("list_files", { dir: dir ?? null });
}

/**
 * The comprehensive working-tree diff against git HEAD (staged + unstaged
 * tracked changes) for the Diff tab's "vs Git" mode. `path` scopes the diff
 * to one file (sandbox-validated backend-side); `null` diffs the whole tree.
 * Empty string = no changes. Outside a repo it throws the short canonical
 * message `"not a git repository"`; a repo with no commits yet throws
 * `"no commits yet"` (both instead of git's raw multi-line output, so the UI
 * can render a friendly panel). Untracked (never-committed) files are not
 * included.
 */
export async function gitDiffHead(path: string | null): Promise<string> {
  return await invoke("git_diff_head", { path });
}

/**
 * Initialize a git repository at the project root (`git init`). Backs the
 * Diff tab's "Initialize Git Repository" button, shown when the open
 * directory is not a repository. Idempotent — `git init` on an existing repo
 * is a no-op. Throws a human-readable error string on failure.
 */
export async function gitInit(): Promise<void> {
  await invoke("git_init");
}

/** A markdown file picked from the native "Browse" file dialog. */
export interface BrowsedFile {
  path: string;
  content: string;
}

/**
 * Open a native file picker (initialized at the project root) filtered to
 * `.md` files and return the chosen file's path + content. Returns `null`
 * when the user cancels the dialog.
 */
export async function browseMarkdownFile(): Promise<BrowsedFile | null> {
  return await invoke("browse_markdown_file");
}

export async function getGitBranch(): Promise<string> {
  return await invoke("get_git_branch");
}

/** One local branch in the Git tab's list (mirrors the Rust `GitBranchInfo`). */
export interface GitBranchInfo {
  /** Short branch name (e.g. `main`, `feat/x`). */
  name: string;
  /** True when this is the currently checked-out branch. */
  is_current: boolean;
  /** True when the branch is named `main`. */
  is_main: boolean;
  /** The full commit sha this branch points at. */
  tip_sha: string;
  /** Raw upstream-tracking text (e.g. `[ahead 3, behind 1]`), or null. */
  upstream_track: string | null;
  /** True when git reports the branch fully merged into `main`. */
  merged_into_main: boolean;
}

/** One commit in the Git tab's DAG (mirrors the Rust `GitCommitInfo`). */
export interface GitCommitInfo {
  /** Full commit sha. */
  sha: string;
  /** Abbreviated sha. */
  short_sha: string;
  /** Parent shas, first parent first; empty for root commits. */
  parents: string[];
  /** Commit subject (first message line). */
  subject: string;
  /** Author name. */
  author: string;
  /** Author date as unix seconds. */
  timestamp: number;
  /** Ref decorations (`HEAD -> main`, `main`, `tag: v1`, …). */
  refs: string[];
}

/** The Git tab's dataset (mirrors the Rust `GitHistory`). */
export interface GitHistory {
  /** The currently checked-out branch. */
  current_branch: string;
  /** Local branches, `main` first (then current, then name). */
  branches: GitBranchInfo[];
  /** Up to 400 commits across local branches, topo order (newest first). */
  commits: GitCommitInfo[];
  /** `main`'s first-parent chain (the predominant spine), newest first. */
  main_spine: string[];
}

/**
 * The Git tab's dataset: local branches, the commit DAG (with parents, so
 * the view can lay out lanes), and main's first-parent spine. Throws with
 * git's message outside a repository (or any git failure).
 */
export async function gitHistory(): Promise<GitHistory> {
  return await invoke("git_history");
}

/**
 * Enter a skill on the given agent: starts the skill (transitions the
 * workflow to the Skill state with the skill's tool allow-list + prompt) and
 * sends the skill's goal as a prompt so the agent drives toward it. Backs the
 * "Merge to main" button's confirm action (the merge_to_main skill). The
 * agent-driven path is the `skill_start` tool (which goes through the normal
 * approval gate); this is the UI-initiated path.
 */
export async function enterSkill(
  agentId: AgentId,
  skill: string,
  prompt?: string,
): Promise<{ success: boolean; skill: string }> {
  return await invoke("enter_skill", { agentId, skill, prompt: prompt ?? null });
}

/**
 * Save an agent's conversation transcript to disk as JSON.
 * Returns the path that was written.
 */
export async function saveConversation(
  agentId: AgentId,
  path: string,
  content: string
): Promise<string> {
  return await invoke("save_conversation", { agentId, path, content });
}

/**
 * Load a conversation transcript from disk. Returns the raw JSON content
 * for the frontend to parse and restore into its store.
 */
export async function loadConversation(
  agentId: AgentId,
  path: string
): Promise<string> {
  return await invoke("load_conversation", { agentId, path });
}

// Events (Rust → frontend).

/** Report a diagnostic line to the backend's stderr.
 *
 * Uses `invoke`, which keeps working when Tauri EVENT delivery is broken
 * (commands travel over the custom IPC protocol; events are eval'd into the
 * webview) — so this is how the frontend can say "my listener registered"
 * even when nothing emitted ever arrives. Diagnostic only. */
export async function uiDiag(msg: string): Promise<void> {
  try {
    await invoke("ui_diag", { msg });
  } catch {
    // Diagnostics must never break the app.
  }
}

export function onAgentEvent(
  handler: (payload: AgentEventPayload) => void
): Promise<UnlistenFn> {
  return listen<AgentEventPayload>(AGENT_EVENT_CHANNEL, (event) => {
    handler(event.payload);
  });
}

// ── Backlog commands ────────────────────────────────────────────────────────

/** Add a prompt (text + base64 data-URL images) to the backlog. */
export async function backlogAdd(
  text: string,
  images: string[] = []
): Promise<BacklogItem> {
  return await invoke("backlog_add", { text, images });
}

/** List all backlog items (persisted in `.coding/backlog.jsonl`). */
export async function backlogList(): Promise<BacklogItem[]> {
  return await invoke("backlog_list");
}

/** Remove a backlog item by id (a UUIDv4 string). */
export async function backlogRemove(id: string): Promise<void> {
  await invoke("backlog_remove", { id });
}

/** Reorder the backlog to the given full id order. */
export async function backlogReorder(ids: string[]): Promise<void> {
  await invoke("backlog_reorder", { ids });
}

/** Remove all finished (done/failed/cant_resolve) backlog items. */
export async function backlogClearFinished(): Promise<void> {
  await invoke("backlog_clear_finished");
}

/** Re-queue a failed/cant_resolve item back to pending (the retry action). */
export async function backlogRetry(id: string): Promise<void> {
  await invoke("backlog_retry", { id });
}

/**
 * Set or clear the deferred (skip Run-All) flag on a backlog item — the
 * "keep but do not auto-run" state. The item keeps its status and stays
 * visible; only Run-All selection skips it (manual dispatch still works).
 * Returns `true` when the item existed (`false` = unknown id, no-op).
 */
export async function backlogSetDeferred(
  id: string,
  deferred: boolean
): Promise<boolean> {
  return await invoke("backlog_set_deferred", { id, deferred });
}

/**
 * Edit the text + images of an existing backlog item (the edit action). The
 * item's status, note, and creation time are preserved. Returns `true` when the
 * item existed (and was updated), `false` when the id was unknown (no-op).
 */
export async function backlogEdit(
  id: string,
  text: string,
  images: string[] = []
): Promise<boolean> {
  return await invoke("backlog_edit", { id, text, images });
}

/**
 * Dispatch a specific pending backlog item (by id) to the main agent (when
 * idle). Backs the ▶ button on an individual backlog card — this targets the
 * exact id the user clicked rather than always taking the top pending item.
 * No-ops on the backend if the item isn't pending, the agent is busy, or the
 * id is unknown.
 */
export async function backlogDispatchItem(id: string): Promise<void> {
  await invoke("backlog_dispatch_item", { id });
}

/** Toggle idle auto-feed (session-only, default off). */
export async function backlogSetAutoFeed(enabled: boolean): Promise<void> {
  await invoke("backlog_set_auto_feed", { enabled });
}

/** Toggle parallel run-all (session-only, default off; plan ffd7a86f).
 * Gates run-all CONCURRENCY only — auto-feed stays sequential. */
export async function backlogSetParallelRunAll(enabled: boolean): Promise<void> {
  await invoke("backlog_set_parallel_run_all", { enabled });
}

/** Start the Run-All ("overnight") loop over all pending items.
 * `concurrency` (parallel run-all, plan ffd7a86f): 1/undefined = sequential
 * (today's behavior); N > 1 = items beyond the first dispatch concurrently
 * to spawned worktree agents. */
export async function backlogRunAll(concurrency?: number): Promise<void> {
  await invoke("backlog_run_all", { concurrency });
}

/** Stop the Run-All loop (the in-flight item finishes its turn first). */
export async function backlogStopAll(): Promise<void> {
  await invoke("backlog_stop_all");
}

/** Subscribe to `backlog://changed` (emitted on every backend mutation). */
export function onBacklogChanged(
  handler: (payload: BacklogChangedPayload) => void
): Promise<UnlistenFn> {
  return listen<BacklogChangedPayload>(BACKLOG_CHANGED_CHANNEL, (event) => {
    handler(event.payload);
  });
}

// ── Headless-browser commands (src-tauri/src/ipc/browser.rs) ──

/** List every open headless-browser page (empty when the browser hasn't spawned). */
export async function browserPages(): Promise<BrowserPage[]> {
  return await invoke("browser_pages");
}

/** Get the latest screenshot of `pageId` as a base64 PNG (null when unavailable). */
export async function browserScreenshotLatest(pageId: string): Promise<string | null> {
  return await invoke("browser_screenshot_latest", { pageId });
}

/** Get the buffered console messages for `pageId` (empty on error / no page). */
export async function browserConsole(pageId: string): Promise<BrowserConsoleEntry[]> {
  return await invoke("browser_console", { pageId });
}

/** Open `url` in a new page (making it active) and return its snapshot. */
export async function browserOpen(url: string): Promise<BrowserPage> {
  return await invoke("browser_open", { url });
}

/**
 * Normalize a URL the same way browser_open / browser_navigate do: scheme-less
 * hostnames (`google.com`) get an omnibox-style scheme (`https://`; `http://`
 * for localhost/IPs), then the scheme allow-list (`http`, `https`, `data`,
 * `file`) is enforced — `file:///C:/path/page.html` loads a local HTML file
 * for pure HTML debugging. The Browser tab's URL bar calls this before setting
 * the webview URL, so `google.com` loads google (not the app's SPA fallback).
 * Throws a human-readable error string on an invalid/disallowed URL.
 */
export async function browserNormalizeUrl(url: string): Promise<string> {
  return await invoke("browser_normalize_url", { url });
}

const BROWSER_CONSOLE_CHANNEL = "browser://console";

/** Subscribe to live `browser://console` events (Rust → frontend push). */
export function onBrowserConsole(
  handler: (payload: BrowserConsoleEventPayload) => void
): Promise<UnlistenFn> {
  return listen<BrowserConsoleEventPayload>(BROWSER_CONSOLE_CHANNEL, (event) => {
    handler(event.payload);
  });
}

// ── Child-WebView2 commands (src-tauri/src/ipc/browser_webview.rs) ──
//
// The Browser tab is now a native child WebView2 embedded in the "main" window
// (replaces the iframe). The frontend reports the browser-area rect (physical
// px) so Rust can position/resize the child; on Open it ensures the child
// exists + navigates it. A child WebView2 is never "framed," so sites that
// refuse framing (Google/YouTube — X-Frame-Options / CSP frame-ancestors)
// load normally.

/**
 * Whether the native child webview for the Browser tab is supported on this
 * platform (Windows only — WebView2 + CDP). The Browser tab fetches this on
 * mount to decide between the URL bar and the unsupported-platform panel.
 */
export async function browserWebviewSupported(): Promise<boolean> {
  return await invoke("browser_webview_supported");
}

/**
 * Ensure the child webview exists at the given physical rect, then navigate it
 * to `url` (normalized). Creates the child on first call; otherwise just
 * moves/resizes it. Coordinates are physical pixels (CSS px × devicePixelRatio).
 */
export async function browserWebviewEnsure(
  x: number,
  y: number,
  w: number,
  h: number,
  url: string,
): Promise<void> {
  await invoke("browser_webview_ensure", { x, y, w, h, url });
}

/** Move/resize the existing child webview to the given physical rect. */
export async function browserWebviewSetRect(
  x: number,
  y: number,
  w: number,
  h: number,
): Promise<void> {
  await invoke("browser_webview_set_rect", { x, y, w, h });
}

/** Navigate the child webview to `url` (normalized via the scheme allow-list). */
export async function browserWebviewNavigate(url: string): Promise<void> {
  await invoke("browser_webview_navigate", { url });
}

/**
 * Stop the child webview's in-flight page load. Tauri has no native stop(),
 * so the backend evaluates `window.stop()` in the page (the JavaScript
 * stop-button equivalent). Safe no-op when no webview exists yet.
 */
export async function browserWebviewStop(): Promise<void> {
  await invoke("browser_webview_stop");
}

/**
 * Hard-reload the child webview's current page (bypasses the HTTP cache).
 * Safe no-op when no webview exists yet.
 */
export async function browserWebviewReload(): Promise<void> {
  await invoke("browser_webview_reload");
}

/**
 * Set whether the Browser tab is the active tab. When inactive, the child
 * webview is hidden (so it doesn't punch through other tabs' content).
 */
export async function browserWebviewSetTabVisible(visible: boolean): Promise<void> {
  await invoke("browser_webview_set_tab_visible", { visible });
}

/** Payload of `browser://reveal` — the URL the agent navigated the child to. */
export interface BrowserRevealPayload {
  url: string;
}

/** Subscribe to `browser://reveal` (emitted when the agent bootstraps the child webview). */
export function onBrowserReveal(
  handler: (payload: BrowserRevealPayload) => void
): Promise<UnlistenFn> {
  return listen<BrowserRevealPayload>(BROWSER_REVEAL_CHANNEL, (event) => {
    handler(event.payload);
  });
}

/**
 * Enter a modal overlay: hide the child webview (a full-viewport modal would
 * otherwise be punched through by the native HWND). Paired with
 * `browserWebviewOverlayExit` — nested modals balance via a counter.
 */
export async function browserWebviewOverlayEnter(): Promise<void> {
  await invoke("browser_webview_overlay_enter");
}

/**
 * Exit a modal overlay: show the child webview when the overlay depth returns
 * to 0 AND the tab is visible. Saturating — a stray exit can't underflow.
 */
export async function browserWebviewOverlayExit(): Promise<void> {
  await invoke("browser_webview_overlay_exit");
}

// ── LLM trace commands (src-tauri/src/ipc/trace.rs) ──

/**
 * List all recorded LLM requests as lightweight summaries, oldest first.
 * Payloads (request JSON + raw response) are excluded — fetch one via
 * `getLlmRequest` when a row is selected.
 */
export async function listLlmRequests(): Promise<LlmRequestSummary[]> {
  return await invoke("list_llm_requests");
}

/**
 * The full detail for one recorded LLM request (exact request JSON + raw
 * response text + usage + timing). `null` when the id was never recorded or
 * has been evicted from the ring buffer.
 */
export async function getLlmRequest(id: number): Promise<LlmRequestDetail | null> {
  return await invoke("get_llm_request", { id });
}

/** Drop all recorded LLM requests (ids keep counting up — no reuse). */
export async function clearLlmRequests(): Promise<void> {
  await invoke("clear_llm_requests");
}

/**
 * Steering-effectiveness counters — how often the advisory nudges (search
 * symbol nudge, shell grep TIP, graph miss hint, RECALLED CONTEXT rider,
 * read_files SYMBOL NUDGE) fired transitively through tool results, and how
 * often the immediately-following call named one of the marker's target
 * tools. Advisory instrumentation, process-wide since app start.
 */
export async function getSteeringStats(): Promise<SteeringStatsSnapshot> {
  return await invoke("get_steering_stats");
}

/**
 * Whether trace records are mirrored to `.coding/logs/traces.jsonl`. The
 * Trace tab reads this on mount so the checkbox reflects state across tabs.
 */
export async function getTraceLogging(): Promise<boolean> {
  return await invoke("get_trace_logging");
}

/** Turn trace file logging on/off. */
export async function setTraceLogging(enabled: boolean): Promise<void> {
  await invoke("set_trace_logging", { enabled });
}

/**
 * List the configured MCP servers (the live manager — startup `mcp.toml`
 * plus any saves this session).
 */
export async function listMcpServers(): Promise<McpServer[]> {
  return await invoke("mcp_list_servers");
}

/**
 * Replace the whole MCP server set: validates server-side, writes
 * `mcp.toml`, syncs the in-memory config, and pushes into the live
 * manager. Applies to agents built after the save (a restart always picks
 * changes up); returns a human-readable note for the section's ok banner.
 */
export async function saveMcpServers(servers: McpServer[]): Promise<string> {
  return await invoke("mcp_save_servers", { servers });
}

/**
 * Test one MCP server from Settings: connect, run the initialize
 * handshake, and list its tools (bounded at 45s server-side). Only
 * meaningful for SAVED servers — the live manager knows the saved set.
 */
export async function testMcpServer(name: string): Promise<McpTestResult> {
  return await invoke("mcp_test", { name });
}

/**
 * Start the OAuth 2.1 flow for a saved remote server (auth = "oauth"):
 * returns the authorization URL to open in the user's browser — the
 * redirect completes in a background task and stores the tokens.
 */
export async function startMcpOauth(name: string): Promise<McpOauthStart> {
  return await invoke("mcp_oauth_start", { name });
}

/**
 * Live per-server status (connected / tool count / last error / restart
 * count) for the Settings MCP page — a pure read, never connects.
 */
export async function mcpStatus(): Promise<McpServerStatus[]> {
  return await invoke("mcp_status");
}
