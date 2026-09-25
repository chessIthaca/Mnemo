// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/** Shared types and constants for the Settings dialog. */

import type { EndpointEditable, ModelConfigEditable, VisionModelInfo } from "../../lib/tauri";
import type { McpServer, McpServerStatus } from "../../lib/types";
import type { SteeringNoteKey } from "../../lib/delegationNotes";

/** Left-nav section ids for the Settings shell. */
export type SettingsSectionId =
  | "providers"
  | "appearance"
  | "chat"
  | "sounds"
  | "safety"
  | "git"
  | "vision"
  | "embeddings"
  | "classifier"
  | "memory"
  | "pricing"
  | "models"
  | "mcp"
  | "advanced";

/** Nav item shown in the Settings left rail. */
export interface SettingsNavItem {
  id: SettingsSectionId;
  label: string;
  /** Short hint under the label (optional). */
  hint?: string;
  /** Extra keywords for the Settings search filter. */
  keywords?: string[];
}

/** Ordered left-nav items. */
export const SETTINGS_NAV: SettingsNavItem[] = [
  {
    id: "providers",
    label: "Providers",
    hint: "Endpoints, keys, defaults",
    keywords: ["endpoint", "api", "key", "model", "openai", "ollama", "base url"],
  },
  {
    id: "appearance",
    label: "Appearance",
    hint: "Theme, font, colors",
    keywords: ["theme", "font", "color", "dark", "light", "system"],
  },
  {
    id: "chat",
    label: "Chat",
    hint: "Chat display options",
    keywords: [
      "chat",
      "conversation",
      "token usage",
      "tool activity",
      "delegation notes",
      "tool cards",
      "activity bar",
      "activity log",
    ],
  },
  {
    id: "sounds",
    label: "Sounds",
    hint: "Notification sounds",
    keywords: [
      "sound",
      "ding",
      "audio",
      "alert",
      "notification",
      "doom",
      "complete",
      "approval",
      "question",
      "error",
    ],
  },
  {
    id: "safety",
    label: "Safety",
    hint: "Approval mode & rules",
    keywords: ["approve", "autonomous", "rules", "safety.toml"],
  },
  {
    id: "git",
    label: "Git",
    hint: "Approval-gated subcommands",
    keywords: ["git", "merge", "push", "approval", "core", "commit", "checkout"],
  },
  {
    id: "vision",
    label: "Vision",
    hint: "Image-to-text fallback",
    keywords: ["image", "multimodal", "describe"],
  },
  {
    id: "embeddings",
    label: "Embeddings",
    hint: "Semantic memory recall",
    keywords: ["embedding", "nomic", "vector", "semantic", "memory", "ollama"],
  },
  {
    id: "classifier",
    label: "Classifier",
    hint: "Laya System 1 (opt-in)",
    keywords: [
      "laya",
      "classifier",
      "system 1",
      "classify",
      "sidecar",
      "laya-serve",
      "endpoint",
    ],
  },
  {
    id: "memory",
    label: "Memory & Search",
    hint: "Cleanup & index rebuilds",
    keywords: [
      "memory",
      "cleanup",
      "consolidate",
      "rebuild",
      "fts",
      "index",
      "vacuum",
      "optimize",
      "maintenance",
      "codegraph",
      "code search",
    ],
  },
  {
    id: "pricing",
    label: "Pricing",
    hint: "Cost per 1M tokens",
    keywords: ["cost", "price", "stats", "dollars"],
  },
  {
    id: "models",
    label: "Models",
    hint: "Per-context model overrides",
    keywords: ["model", "endpoint", "skill", "subagent", "planning", "executing", "deepseek"],
  },
  {
    id: "mcp",
    label: "MCP",
    hint: "Model Context Protocol servers",
    keywords: [
      "mcp",
      "server",
      "stdio",
      "protocol",
      "tools",
      "remote",
      "model context protocol",
    ],
  },
  {
    id: "advanced",
    label: "Advanced",
    hint: "Context, paths",
    keywords: ["summarize", "context", "config", "projects", "path"],
  },
];

/** Reasoning-effort options (values pass through; "off" omits the field). */
export const REASONING_EFFORTS = [
  "max",
  "high",
  "medium",
  "low",
  "minimal",
  "off",
] as const;

/**
 * Sentinel for the effort dropdown "use endpoint default (max)" option.
 * Maps to `reasoning_effort: null` on the endpoint DTO. Distinct from `"off"`,
 * which omits the field entirely at request time.
 */
export const REASONING_EFFORT_DEFAULT = "__default__";

/** Provider kind dropdown values (serde form written to disk). */
export const KIND_OPTIONS = ["openai", "local", "anthropic"] as const;

/** Curated font options — common Windows/Mac/Linux fonts. */
export const FONT_OPTIONS = [
  "system-ui",
  "Inter",
  "Consolas",
  "JetBrains Mono",
  "Fira Code",
  "Monaco",
  "Cascadia Code",
  "Georgia",
  "Times New Roman",
];

/** Mint a unique id for an endpoint row (React key; not persisted).
 *  Uses crypto.randomUUID when available, else Math.random. This is for React
 *  list keys only — never reuse it for anything security-adjacent. */
export function makeUid(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `ep-${crypto.randomUUID().slice(0, 8)}`;
  }
  return `ep-${Math.random().toString(36).slice(2, 10)}`;
}

/** A blank endpoint used by the "Add endpoint" button. */
export function blankEndpoint(): EndpointEditable {
  return {
    name: "",
    kind: "openai",
    base_url: "",
    models: [],
    model_configs: [],
    max_context: null,
    max_output_tokens: null,
    multimodal: false,
    supports_reasoning_effort: true,
    reasoning_effort: null,
    workspace_id: null,
  };
}

/**
 * Normalize a kind string from get_config to the lowercase serde form for the
 * dropdown. `get_config` always emits the serde form
 * ("openai"/"local"/"anthropic"), so this is now a passthrough that validates
 * the value (anything unrecognized falls back to "openai").
 */
export function kindFromConfig(k: string): string {
  return k === "local" ? "local" : k === "anthropic" ? "anthropic" : "openai";
}

/** Snapshot serializer for providers dirty-check. */
export function serializeProviders(
  eps: EndpointEditable[],
  keys: Record<string, string>,
  dp: string | null,
  dm: string | null,
): string {
  return JSON.stringify({ eps, keys, dp, dm });
}

/**
 * The per-model config for a model id — looked up by id (the backend matches
 * by id, so position drift between `models` and `model_configs` is
 * tolerated). Returns a bare config (all null/empty) when absent.
 */
export function modelConfigFor(
  ep: EndpointEditable,
  modelId: string,
): NonNullable<EndpointEditable["model_configs"]>[number] {
  const found = (ep.model_configs ?? []).find((mc) => mc.id === modelId);
  return (
    found ?? {
      id: modelId,
      max_context: null,
      max_output_tokens: null,
      reasoning_efforts: [],
      reasoning_effort: null,
    }
  );
}

/**
 * Upsert a per-model config change: replaces the config for `modelId` (or
 * appends it) and returns the patched `model_configs` array — callers pass
 * it through `onEndpointChange({ model_configs })`. Configs for ids no
 * longer in `models` are pruned.
 */
export function upsertModelConfig(
  ep: EndpointEditable,
  modelId: string,
  patch: Partial<{
    max_context: number | null;
    max_output_tokens: number | null;
    reasoning_efforts: string[];
    reasoning_effort: string | null;
    multimodal: boolean | null;
  }>,
): ModelConfigEditable[] {
  const current = modelConfigFor(ep, modelId);
  const merged: ModelConfigEditable = { ...current, ...patch, id: modelId };
  const rest = (ep.model_configs ?? []).filter((mc) => mc.id !== modelId && mc.id !== "");
  // Keep the array parallel to `models` by id order (dropped ids pruned).
  return ep.models
    .filter((m) => m.trim() !== "")
    .map((m) => {
      if (m === modelId) return merged;
      const existing = rest.find((mc) => mc.id === m);
      return existing ?? { id: m, max_context: null, max_output_tokens: null, reasoning_efforts: [], reasoning_effort: null };
    });
}

/**
 * The auto-fill patch for one model's discovered token caps: fills ONLY the
 * per-model fields whose effective value is currently unset (per-model null
 * AND endpoint-level null) — an explicit value at either level is never
 * silently overwritten (the conflict hint in the card offers a one-click
 * Apply for disagreements instead). The value lands in the per-model entry
 * so a multi-model endpoint keeps each model's own caps. Returns the
 * `model_configs` patch for `onEndpointChange`, or null when nothing needs
 * filling (or the model isn't in `ep.models`).
 */
export function capsAutofillPatch(
  ep: EndpointEditable,
  modelId: string,
  caps: { ctx: number | null; out: number | null },
): Partial<EndpointEditable> | null {
  if (!ep.models.some((m) => m.trim() === modelId)) return null;
  const current = modelConfigFor(ep, modelId);
  const patch: Partial<{ max_context: number | null; max_output_tokens: number | null }> = {};
  if (caps.ctx != null && current.max_context == null && ep.max_context == null) {
    patch.max_context = caps.ctx;
  }
  if (caps.out != null && current.max_output_tokens == null && ep.max_output_tokens == null) {
    patch.max_output_tokens = caps.out;
  }
  if (Object.keys(patch).length === 0) return null;
  return { model_configs: upsertModelConfig(ep, modelId, patch) };
}

/**
 * Map one exported bundle's endpoint record to an [`EndpointEditable`] for
 * the import path. Mirrors the ProvidersSection load mapping — including the
 * per-model `model_configs` array (absent on legacy exports → empty). The
 * export bundle DOES carry `model_configs`, so dropping it here would
 * silently discard every per-model cap/effort list on re-import (review
 * 2026-08-22 M1).
 */
export function importEndpointEditable(e: Record<string, unknown>): EndpointEditable {
  return {
    name: String(e.name ?? ""),
    kind: kindFromConfig(String(e.kind ?? "openai")),
    base_url: String(e.base_url ?? ""),
    models: Array.isArray(e.models) ? (e.models as string[]) : [],
    model_configs: Array.isArray(e.model_configs)
      ? (e.model_configs as Array<Record<string, unknown>>).map((mc) => ({
          id: String(mc.id ?? ""),
          max_context: (mc.max_context as number | null | undefined) ?? null,
          max_output_tokens:
            (mc.max_output_tokens as number | null | undefined) ?? null,
          reasoning_efforts: Array.isArray(mc.reasoning_efforts)
            ? (mc.reasoning_efforts as string[])
            : [],
          reasoning_effort: (mc.reasoning_effort as string | null | undefined) ?? null,
          multimodal: (mc.multimodal as boolean | null | undefined) ?? null,
        }))
      : [],
    max_context: (e.max_context as number | null | undefined) ?? null,
    max_output_tokens: (e.max_output_tokens as number | null | undefined) ?? null,
    multimodal: !!e.multimodal,
    supports_reasoning_effort:
      (e.supports_reasoning_effort as boolean | null | undefined) ?? true,
    reasoning_effort: (e.reasoning_effort as string | null | undefined) ?? null,
    workspace_id: (e.workspace_id as string | null | undefined) ?? null,
  };
}

/**
 * Map stored `reasoning_effort` to a select value.
 * `null`/`undefined` → default (runtime max); `"off"` stays off.
 */
export function effortToSelectValue(
  effort: string | null | undefined,
): string {
  if (effort == null || effort === "") return REASONING_EFFORT_DEFAULT;
  return effort;
}

/**
 * Map select value back to stored `reasoning_effort`.
 * Default sentinel → `null`; `"off"` → `"off"`.
 */
export function effortFromSelectValue(value: string): string | null {
  if (value === REASONING_EFFORT_DEFAULT) return null;
  return value;
}

/**
 * Snapshot of chat display prefs (for draft / discard). These control what the
 * chat surface shows — NOT visual styling — so they live in their own Chat
 * section, not Appearance (moved out of AppearanceDraft 2026-08-20).
 */
export interface ChatDraft {
  showTokenUsage: boolean;
  showToolImages: boolean;
  showToolActivity: boolean;
  showKnowledgeActivity: boolean;
  /**
   * Whether the live shell-output preview renders in running tool cards.
   * Persisted to localStorage (mh.showShellPreview) — NOT config.toml, per
   * backlog 6f25fb7e; the store setter writes localStorage directly.
   */
  showShellPreview: boolean;
  /**
   * Per-kind steering-note visibility (config.toml [ui.steering_notes]), one
   * flag per registry kind — the positive form of the store's hidden list;
   * the Chat section renders one checkbox per `STEERING_NOTES` entry.
   */
  steeringNotes: Record<SteeringNoteKey, boolean>;
  chatThreadLine: boolean;
  chatProseCap: boolean;
  chatTurnTint: boolean;
  chatHoverTimestamps: boolean;
}

/** Serialize chat prefs for dirty comparison. */
export function serializeChat(d: ChatDraft): string {
  return JSON.stringify(d);
}

/**
 * Snapshot of the Laya classifier draft (for dirty / discard). `mode` picks
 * managed (Mnemo downloads + runs laya-serve) vs external; `checkpoint` is
 * the managed-mode checkpoint id; `endpoint` is the raw external-mode input
 * value; "" means "not configured" (the backend clears the stored URL when
 * the save patch carries a blank string).
 */
export interface ClassifierDraft {
  enabled: boolean;
  mode: "external" | "managed";
  checkpoint: string;
  endpoint: string;
}

/** Serialize the classifier draft for dirty comparison. */
export function serializeClassifier(d: ClassifierDraft): string {
  return JSON.stringify(d);
}

/**
 * Snapshot of the notification-sound toggles (for draft / discard). Each
 * sound is disabled independently (persisted to config.toml [ui]).
 */
export interface SoundDraft {
  soundComplete: boolean;
  soundInput: boolean;
  soundDoom: boolean;
}

/** Serialize sound prefs for dirty comparison. */
export function serializeSound(d: SoundDraft): string {
  return JSON.stringify(d);
}

/** Snapshot of all appearance prefs (for draft / discard). */
export interface AppearanceDraft {
  theme: "dark" | "light" | "system";
  fontFamily: string;
  fontSize: number;
  accentColor: string;
  borderColor: string;
  textPrimaryColor: string;
  textMutedColor: string;
  codeTextColor: string;
  codeCommentColor: string;
  codeKeywordColor: string;
  codeStringColor: string;
  codeNumberColor: string;
  codeTitleColor: string;
  codeVariableColor: string;
}

/** Serialize appearance for dirty comparison. */
export function serializeAppearance(d: AppearanceDraft): string {
  return JSON.stringify(d);
}

/** Whether a Settings section id is valid (for deep-link parsing). */
export function isSettingsSectionId(v: string): v is SettingsSectionId {
  return (
    v === "providers" ||
    v === "appearance" ||
    v === "chat" ||
    v === "sounds" ||
    v === "safety" ||
    v === "git" ||
    v === "vision" ||
    v === "embeddings" ||
    v === "classifier" ||
    v === "memory" ||
    v === "pricing" ||
    v === "models" ||
    v === "mcp" ||
    v === "advanced"
  );
}

/**
 * Imperative handle a Settings section exposes so the dialog shell can trigger
 * its save on OK. `save()` persists the section's draft and returns `true` on
 * success or `false` on failure (the section surfaces its own error toast).
 */
export interface SettingsSectionHandle {
  /** Persist the section's draft. Returns true on success, false on failure. */
  save: () => Promise<boolean>;
}

/**
 * Return the ids of sections currently marked dirty in a dirty map. Pure
 * helper so the dialog shell's OK handler can be unit-tested without React.
 */
export function dirtySectionIds(
  dirtyMap: Partial<Record<SettingsSectionId, boolean>>,
): SettingsSectionId[] {
  return (Object.keys(dirtyMap) as SettingsSectionId[]).filter(
    (id) => dirtyMap[id] === true,
  );
}

/**
 * Build an exportable settings bundle (no API keys by default).
 * Pure helper so unit tests can cover shape without DOM/Tauri.
 */
export function buildExportBundle(
  settings: {
    general: unknown;
    context: unknown;
    ui: unknown;
    endpoints: unknown;
    pricing: unknown;
    projects?: unknown;
  },
  opts?: { apiKeys?: Record<string, string>; includeKeys?: boolean },
): {
  version: 1;
  exported_at: string;
  general: unknown;
  context: unknown;
  ui: unknown;
  endpoints: unknown;
  pricing: unknown;
  projects?: unknown;
  api_keys?: Record<string, string>;
} {
  const bundle: {
    version: 1;
    exported_at: string;
    general: unknown;
    context: unknown;
    ui: unknown;
    endpoints: unknown;
    pricing: unknown;
    projects?: unknown;
    api_keys?: Record<string, string>;
  } = {
    version: 1,
    exported_at: new Date().toISOString(),
    general: settings.general,
    context: settings.context,
    ui: settings.ui,
    endpoints: settings.endpoints,
    pricing: settings.pricing,
    projects: settings.projects,
  };
  if (opts?.includeKeys && opts.apiKeys) {
    bundle.api_keys = opts.apiKeys;
  }
  return bundle;
}

/**
 * Validate a parsed import bundle. Returns a human-readable error or null if OK.
 */
export function validateImportBundle(raw: unknown): string | null {
  if (!raw || typeof raw !== "object") return "Import file must be a JSON object.";
  const o = raw as Record<string, unknown>;
  if (o.version !== 1 && o.version !== undefined) {
    return `Unsupported export version: ${String(o.version)}`;
  }
  if (!o.endpoints && !o.general && !o.ui && !o.pricing) {
    return "Import file has no recognizable settings sections.";
  }
  if (o.endpoints !== undefined && !Array.isArray(o.endpoints)) {
    return "endpoints must be an array.";
  }
  if (o.pricing !== undefined && !Array.isArray(o.pricing)) {
    return "pricing must be an array.";
  }
  return null;
}

/** A blank MCP server row for the "Add server" button (stdio transport,
 *  global source — project overrides are opted into via the source select). */
export function blankMcpServer(): McpServer {
  return {
    name: "",
    enabled: true,
    command: null,
    args: [],
    env: [],
    url: null,
    headers_env: {},
    source: "global",
    auth: null,
    client_id: null,
    scopes: [],
    trusted: false,
    idle_timeout_secs: null,
  };
}

/**
 * Normalize one server into the canonical serialization shape (every
 * optional field defaulted) so dirty comparison is shape-stable.
 */
function canonicalMcp(s: McpServer): McpServer {
  return {
    name: s.name,
    enabled: s.enabled,
    command: s.command ?? null,
    args: s.args ?? [],
    env: s.env ?? [],
    url: s.url ?? null,
    headers_env: s.headers_env ?? {},
    source: s.source ?? "global",
    auth: s.auth ?? null,
    client_id: s.client_id ?? null,
    scopes: s.scopes ?? [],
    trusted: s.trusted ?? false,
    idle_timeout_secs: s.idle_timeout_secs ?? null,
  };
}

/** Serialize the MCP server set for dirty comparison. */
export function serializeMcpServers(list: McpServer[]): string {
  return JSON.stringify(list.map(canonicalMcp));
}

/** One-line transport summary for a server card. */
export function mcpTransportSummary(s: McpServer): string {
  if (s.url && s.url.trim() !== "") return `remote: ${s.url}`;
  const cmd = [s.command ?? "", ...(s.args ?? [])].join(" ").trim();
  return `stdio: ${cmd || "(no command set)"}`;
}

/**
 * One-line live status for a server card from `mcp_status`: the
 * connection state (connected · N tools), the last error, or "not
 * connected this session" when the manager has seen no traffic yet —
 * plus a restart count badge when the connection was re-established.
 * Undefined status (never seen this session) renders as nothing, so
 * callers return null for it instead of a line.
 */
export function mcpStatusLine(s: McpServerStatus | undefined): string | null {
  if (!s) return null;
  const state = s.connected
    ? `connected · ${s.tool_count} tool${s.tool_count === 1 ? "" : "s"}`
    : s.last_error
      ? `error: ${s.last_error}`
      : "not connected this session";
  return s.restarts > 0 ? `${state} (restarted ${s.restarts}×)` : state;
}

/**
 * A bare env-var NAME: non-empty, no `=`, no whitespace (a spaced string
 * like `Bearer abc` is a VALUE, not a name). Mirrors the backend's
 * `is_bare_env_name` so client and server validation never disagree.
 */
function isBareEnvName(v: string): boolean {
  return v.trim() !== "" && !v.includes("=") && !/\s/.test(v);
}

/**
 * Client-side sanity validation before save (the backend validates
 * strictly and is the source of truth — this just fails fast with a
 * friendlier message). Returns an error message or null when OK.
 */
export function validateMcpDraft(list: McpServer[]): string | null {
  const names = new Set<string>();
  for (const s of list) {
    const name = s.name.trim();
    if (!name) return "Every server needs a name.";
    // The name feeds mcp.<name> + mcp__<name>__<tool>: whitespace makes
    // the group unrevealable and '__' aliases tool names (review LOW 3 —
    // mirrors the backend validation).
    if (/\s/.test(s.name))
      return `Server name '${s.name}' must not contain whitespace.`;
    if (s.name.includes("__"))
      return `Server name '${s.name}' must not contain '__' (reserved by the mcp__<server>__<tool> namespacing).`;
    if (names.has(name)) return `Duplicate server name '${name}'.`;
    names.add(name);
    const hasCmd = !!s.command && s.command.trim() !== "";
    const hasUrl = !!s.url && s.url.trim() !== "";
    if (hasCmd && hasUrl)
      return `Server '${name}': pick one transport — command (stdio) OR url (remote).`;
    if (!hasCmd && !hasUrl)
      return `Server '${name}': set a command (stdio) or a url (remote).`;
    // Auth rules mirror the backend: only "oauth" is known, it is
    // remote-only, and it requires a pre-registered client id.
    if (s.auth && s.auth !== "none" && s.auth !== "oauth")
      return `Server '${name}': unknown auth scheme '${s.auth}' (supported: oauth).`;
    if (s.auth === "oauth") {
      if (!hasUrl)
        return `Server '${name}': auth = "oauth" requires a remote (url) server.`;
      if (!s.client_id || s.client_id.trim() === "")
        return `Server '${name}': auth = "oauth" requires a client_id (dynamic client registration is not supported).`;
    }
    if (s.idle_timeout_secs != null && s.idle_timeout_secs < 1)
      return `Server '${name}': idle timeout must be at least 1 second.`;
    for (const v of s.env ?? []) {
      if (!isBareEnvName(v))
        return `Server '${name}': env entries are variable NAMES, not values — values come from your environment.`;
    }
    for (const [h, v] of Object.entries(s.headers_env ?? {})) {
      if (!h.trim()) return `Server '${name}': headers_env has an empty header name.`;
      if (!isBareEnvName(v))
        return `Server '${name}': headers_env values are env-var NAMES (e.g. Authorization = MY_TOKEN).`;
    }
  }
  return null;
}

/** Discovered token caps for one model id (null = provider didn't report). */
export type DiscoveredCaps = { ctx: number | null; out: number | null };

/**
 * Map a fetched /models list to the per-model discovered-caps record: each
 * model id → its reported context/output caps (absent fields → null).
 * Discovery is best-effort — a provider exposing no cap fields yields an
 * empty record.
 */
export function discoveredCapsById(list: VisionModelInfo[]): Record<string, DiscoveredCaps> {
  const caps: Record<string, DiscoveredCaps> = {};
  for (const m of list) {
    caps[m.id] = {
      ctx: m.context_length ?? null,
      out: m.max_output_tokens ?? null,
    };
  }
  return caps;
}

/**
 * The effective caps for one model at an endpoint: its per-model override,
 * else the endpoint-level value. (The EndpointCard conflict hints compare
 * this against the provider-reported values.)
 */
export function effectiveCaps(
  ep: EndpointEditable,
  modelId: string,
): { ctx: number | null; out: number | null } {
  const config = modelConfigFor(ep, modelId);
  return {
    ctx: config.max_context ?? ep.max_context ?? null,
    out: config.max_output_tokens ?? ep.max_output_tokens ?? null,
  };
}

/**
 * Parse a comma-separated efforts input into the stored list: split on
 * commas, trim each entry, drop empties (e.g. "max, high" → ["max","high"]).
 */
export function parseEffortsList(value: string): string[] {
  return value
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s !== "");
}

/**
 * Parse a positive-integer number-input value. Three outcomes:
 * - "" → null (the field was cleared — persist the clear)
 * - a finite number > 0 → Math.floor(n) (a valid value)
 * - anything else (0, negatives, non-numeric) → undefined — "no change":
 *   the caller skips the onChange call so an in-progress invalid edit
 *   doesn't clobber the stored value.
 */
export function parsePositiveIntInput(raw: string): number | null | undefined {
  if (raw === "") return null;
  const n = Number(raw);
  if (Number.isFinite(n) && n > 0) return Math.floor(n);
  return undefined;
}
