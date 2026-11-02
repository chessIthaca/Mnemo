import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/**
 * MCP server settings for the MCP page: the transport to
 * `mcp_settings_get` / `mcp_settings_save` / `mcp_settings_test`, the
 * draft/validate helpers the editor uses, and the wire shapes mirroring
 * `McpSettingsWire` in `src-tauri/src/mnemo2/mcp_settings.rs` — the JSON
 * keys are snake_case, like every other Mnemo2 DTO.
 *
 * The page talks in whole lists: the save replaces the configured set and
 * the backend splits it back into the global and project files. The
 * project definition of a name shadows the global one, exactly as the
 * engines' `merge_servers` resolves it.
 */

// ── Wire shapes (the `mcp_settings_get` snapshot) ────────────────────────────

/** Where a server definition lives: the global config dir or the project's `.coding`. */
export type McpSource = "global" | "project";

/** A server's transport kind — `mcp.toml` sets exactly one of command/url. */
export type McpTransportKind = "stdio" | "remote";

/** One configured MCP server, as the page consumes it. */
export interface McpServer {
  /** The stable id (`mcp__<server>__<tool>`, the `mcp.<server>` tool group). */
  name: string;
  /** Whether the server participates at all. */
  enabled: boolean;
  /** stdio: the command to spawn. */
  command?: string;
  /** stdio: its arguments, in order. */
  args: string[];
  /** remote: the URL to connect to. */
  url?: string;
  /** stdio: environment variable NAMES passed through (never values). */
  env: string[];
  /** remote: header name -> environment variable name (never values). */
  headers_env: Record<string, string>;
  /** remote: `"oauth"` when the server signs in through OAuth. */
  auth?: string;
  /** remote + OAuth: the pre-registered client id. */
  client_id?: string;
  /** remote + OAuth: the scopes to request. */
  scopes: string[];
  /** Whether the server's tools skip the approval prompt when the mode would ask. */
  trusted: boolean;
  /** Per-server connection idle timeout, in seconds. */
  idle_timeout_secs?: number;
  /** Which file the definition came from. */
  source: McpSource;
}

/** One live connection status (`McpManager::statuses`). */
export interface McpStatus {
  name: string;
  connected: boolean;
  tool_count: number;
  last_error?: string | null;
  restarts: number;
}

/** The MCP page snapshot. */
export interface McpSettings {
  /** The merged servers (global + project, the project definition winning by name). */
  servers: McpServer[];
  /** A project is open — project servers can be edited. */
  project_open: boolean;
  /** An open lead is live — a save swaps the shared manager now. */
  live_lead: boolean;
  /** Live status per server that connected in this session (empty without a live lead). */
  statuses: McpStatus[];
}

/** The outcome of one Test click (`McpTestResult` in `src/headless/setup.rs`). */
export interface McpTestResult {
  ok: boolean;
  tool_count: number;
  tool_names: string[];
  /** Why the test failed; absent on success. */
  error?: string | null;
}

/** The raw server payload: serde omits defaulted fields (`enabled`, `args`, …). */
interface McpServerWire {
  name: string;
  enabled?: boolean;
  command?: string;
  args?: string[];
  url?: string;
  env?: string[];
  headers_env?: Record<string, string>;
  auth?: string;
  client_id?: string;
  scopes?: string[];
  trusted?: boolean;
  idle_timeout_secs?: number;
  source?: string;
}

/** The raw snapshot as it arrives (defaults omitted). */
interface McpSettingsWire {
  servers: McpServerWire[];
  project_open: boolean;
  live_lead: boolean;
  statuses: McpStatus[];
}

// ── Normalization ───────────────────────────────────────────────────────────

/** Fill a wire server's omitted defaults so the editor never sees `undefined` lists. */
export function normalizeServer(wire: McpServerWire): McpServer {
  return {
    name: wire.name,
    enabled: wire.enabled ?? true,
    command: wire.command,
    args: wire.args ?? [],
    url: wire.url,
    env: wire.env ?? [],
    headers_env: wire.headers_env ?? {},
    auth: wire.auth,
    client_id: wire.client_id,
    scopes: wire.scopes ?? [],
    trusted: wire.trusted ?? false,
    idle_timeout_secs: wire.idle_timeout_secs,
    source: wire.source === "project" ? "project" : "global",
  };
}

/** Normalize a whole snapshot. */
export function normalizeMcpSettings(wire: McpSettingsWire): McpSettings {
  return {
    servers: (wire.servers ?? []).map(normalizeServer),
    project_open: wire.project_open === true,
    live_lead: wire.live_lead === true,
    statuses: wire.statuses ?? [],
  };
}

// ── Row metadata ────────────────────────────────────────────────────────────

/** The source labels the form's scope picker and the row badges show. */
export const MCP_SOURCES: { id: McpSource; label: string; hint: string }[] = [
  {
    id: "global",
    label: "Global",
    hint: "Every project on this machine — <config_dir>/mcp.toml.",
  },
  {
    id: "project",
    label: "This project",
    hint: "Only the open project — <project>/.coding/mcp.toml; a global server with the same name is hidden.",
  },
];

/** The two transports with the form's one-line explanations. */
export const MCP_TRANSPORTS: { id: McpTransportKind; label: string; hint: string }[] = [
  {
    id: "stdio",
    label: "Local command (stdio)",
    hint: "Spawns the command and speaks JSON-RPC over stdin/stdout, e.g. npx -y some-mcp-server.",
  },
  {
    id: "remote",
    label: "Remote (HTTP)",
    hint: "Connects to the server's URL. OAuth sign-in is not on this page — set auth/client_id in mcp.toml.",
  },
];

/** The one-line transport description a row shows. */
export function serverSummary(server: McpServer): string {
  if (server.command) {
    return [server.command, ...server.args].join(" ");
  }
  return server.url ?? "(no transport set)";
}

/** Which transport a saved server uses. */
export function serverTransport(server: McpServer): McpTransportKind {
  return server.command ? "stdio" : "remote";
}

/** The live status row for a server, if it connected this session. */
export function statusFor(statuses: McpStatus[], name: string): McpStatus | undefined {
  return statuses.find((status) => status.name === name);
}

/** A stable key for the unsaved-changes comparison (the save sends the whole list). */
export function serversKey(servers: McpServer[]): string {
  return JSON.stringify(servers);
}

// ── Draft (the add/edit form) ───────────────────────────────────────────────

/** The editable form state for one server — a new row or an existing one. */
export interface McpServerDraft {
  name: string;
  enabled: boolean;
  transport: McpTransportKind;
  /** stdio: the command to spawn. */
  command: string;
  /** stdio: one argument per line (inner spaces stay; blank lines vanish). */
  args: string;
  /** remote: the URL. */
  url: string;
  trusted: boolean;
  source: McpSource;
}

/** A blank draft, scoped to `source`. */
export function emptyServerDraft(source: McpSource): McpServerDraft {
  return {
    name: "",
    enabled: true,
    transport: "stdio",
    command: "",
    args: "",
    url: "",
    trusted: false,
    source,
  };
}

/** The form state for an existing server. */
export function draftFromServer(server: McpServer): McpServerDraft {
  return {
    name: server.name,
    enabled: server.enabled,
    transport: serverTransport(server),
    command: server.command ?? "",
    args: server.args.join("\n"),
    url: server.url ?? "",
    trusted: server.trusted,
    source: server.source,
  };
}

/** Split the args textarea: one argument per line, blank lines dropped, a line's spaces kept. */
export function splitArgs(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

/** The draft's server, or the reason it cannot be saved yet. */
export type DraftResult = { ok: true; server: McpServer } | { ok: false; error: string };

/**
 * Turn a draft into the wire server. `original` (an edit) contributes the
 * settings the form does not show — env names, header names, OAuth fields,
 * the idle timeout — but only where the chosen transport can use them, so a
 * transport switch cannot save a definition the shared validator rejects.
 */
export function draftToServer(draft: McpServerDraft, original?: McpServer): DraftResult {
  const name = draft.name.trim();
  if (name === "") {
    return { ok: false, error: "Enter a name for the server." };
  }
  if (/\s/.test(name)) {
    return { ok: false, error: "A server name must not contain spaces." };
  }
  if (name.includes("__")) {
    return {
      ok: false,
      error: "A server name must not contain '__' — it would alias the mcp__<server>__<tool> names.",
    };
  }
  const stdio = draft.transport === "stdio";
  const command = draft.command.trim();
  const url = draft.url.trim();
  if (stdio && command === "") {
    return { ok: false, error: "Enter the command to run, e.g. npx." };
  }
  if (!stdio && url === "") {
    return { ok: false, error: "Enter the server URL, e.g. https://example.com/mcp." };
  }
  if (!stdio && !/^https?:\/\//i.test(url)) {
    return { ok: false, error: "The URL must start with http:// or https://." };
  }
  const server: McpServer = {
    name,
    enabled: draft.enabled,
    args: stdio ? splitArgs(draft.args) : [],
    env: stdio && original ? [...original.env] : [],
    headers_env: !stdio && original ? { ...original.headers_env } : {},
    scopes: !stdio && original ? [...original.scopes] : [],
    trusted: draft.trusted,
    source: draft.source,
  };
  if (stdio) {
    server.command = command;
  } else {
    server.url = url;
    if (original?.auth) server.auth = original.auth;
    if (original?.client_id) server.client_id = original.client_id;
  }
  if (original?.idle_timeout_secs !== undefined) {
    server.idle_timeout_secs = original.idle_timeout_secs;
  }
  return { ok: true, server };
}

/**
 * Whether adding `draft` as a GLOBAL server would be hidden by a project
 * twin of the same name (the merged list — and therefore the page — never
 * shows that global definition). `servers` is the list the row would join;
 * a caller editing an existing row leaves that row out first.
 */
export function shadowedGlobalName(servers: McpServer[], draft: McpServerDraft): string | null {
  if (draft.source !== "global") return null;
  const name = draft.name.trim();
  if (name === "") return null;
  return servers.some((server) => server.name === name && server.source === "project") ? name : null;
}

// ── Transport ───────────────────────────────────────────────────────────────

/** The Tauri API surface the transport needs, injected for tests and previews. */
export interface McpTransportBindings {
  invoke: typeof invoke;
}

/** The IPC surface the MCP page needs. */
export interface McpTransport {
  /** The full snapshot: every server with its source, plus the live statuses. */
  get(): Promise<McpSettings>;
  /** Replace the whole configured set (the backend splits it by source). */
  save(servers: McpServer[]): Promise<McpSettings>;
  /** Connect to one saved server on a throwaway connection and list its tools. */
  test(name: string): Promise<McpTestResult>;
}

/** The real IPC transport over the Tauri bindings. */
export function createTauriMcpTransport(bindings: McpTransportBindings): McpTransport {
  return {
    get: () => bindings.invoke<McpSettingsWire>("mcp_settings_get").then(normalizeMcpSettings),
    save: (servers) =>
      bindings
        .invoke<McpSettingsWire>("mcp_settings_save", { servers })
        .then(normalizeMcpSettings),
    test: (name) => bindings.invoke<McpTestResult>("mcp_settings_test", { name }),
  };
}

// ── Preview seam (mirrors lib/safetySettings.ts) ────────────────────────────

/** The dev-only transport `src/test/previewMcp.ts` wires up in a browser. */
let previewMcpTransport: McpTransport | null = null;

/** Install the preview transport (dev-only; no Tauri host in a browser). */
export function setPreviewMcpTransport(transport: McpTransport | null): void {
  previewMcpTransport = transport;
}

/**
 * The MCP page's transport: the Tauri one inside the desktop app, the
 * preview one when a test-side seed installed it, null in a plain browser
 * (the dialog shows its empty state).
 */
export function createMcpTransport(bindings?: McpTransportBindings): McpTransport | null {
  if (previewMcpTransport) return previewMcpTransport;
  if (!isTauriRuntime()) return null;
  return createTauriMcpTransport(bindings ?? { invoke });
}
