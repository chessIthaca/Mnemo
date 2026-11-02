/**
 * Dev-only seed for the `settings-mcp` preview scene: an in-memory MCP
 * transport over a seeded snapshot, so the page's add/edit/save/Test
 * controls visibly take effect in a plain browser (no Tauri host).
 *
 * Test-side code like the rest of this folder: `index.html` never reaches
 * it, so `vite build` bundles none of it and `npm run check:no-mock` keeps
 * passing.
 */

import {
  setPreviewMcpTransport,
  type McpServer,
  type McpSettings,
  type McpTestResult,
  type McpTransport,
} from "../lib/mcpSettings";

/** The snapshot the scene shows: a global stdio server and a project remote one. */
export function previewMcpSettings(): McpSettings {
  return {
    servers: [
      {
        name: "files",
        enabled: true,
        command: "npx",
        args: ["-y", "files-mcp"],
        env: ["HOME"],
        headers_env: {},
        scopes: [],
        trusted: true,
        source: "global",
      },
      {
        name: "github",
        enabled: true,
        url: "https://mcp.example.com/github",
        args: [],
        env: [],
        headers_env: { Authorization: "GITHUB_TOKEN" },
        scopes: [],
        trusted: false,
        source: "project",
      },
      {
        name: "browser-tools",
        enabled: false,
        command: "uvx",
        args: ["browser-tools-mcp"],
        env: [],
        headers_env: {},
        scopes: [],
        trusted: false,
        source: "global",
      },
    ],
    project_open: true,
    live_lead: true,
    statuses: [{ name: "files", connected: true, tool_count: 4, last_error: null, restarts: 0 }],
  };
}

/** A copy of the seeded snapshot (the page never mutates the transport's own state). */
function clone(snapshot: McpSettings): McpSettings {
  return {
    ...snapshot,
    servers: snapshot.servers.map((server) => ({
      ...server,
      args: [...server.args],
      env: [...server.env],
      scopes: [...server.scopes],
      headers_env: { ...server.headers_env },
    })),
    statuses: snapshot.statuses.map((status) => ({ ...status })),
  };
}

/** A fake transport over the seeded snapshot; saves mutate it like the real ones would. */
export function createPreviewMcpTransport(): McpTransport {
  let snapshot = previewMcpSettings();
  return {
    get: async () => clone(snapshot),
    save: async (servers: McpServer[]) => {
      snapshot = { ...snapshot, servers: servers.map((server) => ({ ...server })) };
      return clone(snapshot);
    },
    test: async (name: string): Promise<McpTestResult> => {
      if (name === "github") {
        return {
          ok: false,
          tool_count: 0,
          tool_names: [],
          error: "connecting to https://mcp.example.com/github failed: 401 Unauthorized",
        };
      }
      return {
        ok: true,
        tool_count: 4,
        tool_names: ["read_file", "write_file", "list_directory", "search"],
      };
    },
  };
}

/** Wire the preview transport for the `settings-mcp` scene; returns the restore. */
export function seedPreviewMcp(): () => void {
  setPreviewMcpTransport(createPreviewMcpTransport());
  return () => setPreviewMcpTransport(null);
}
