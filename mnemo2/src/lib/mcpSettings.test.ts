import { describe, expect, it, vi } from "vitest";
import {
  MCP_SOURCES,
  MCP_TRANSPORTS,
  createMcpTransport,
  createTauriMcpTransport,
  draftFromServer,
  draftToServer,
  emptyServerDraft,
  normalizeMcpSettings,
  normalizeServer,
  serverSummary,
  serverTransport,
  serversKey,
  setPreviewMcpTransport,
  shadowedGlobalName,
  splitArgs,
  statusFor,
  type McpServer,
  type McpSettings,
  type McpTransport,
} from "./mcpSettings";

/** A wire snapshot: a defaulted stdio server and an explicit project row. */
const WIRE = {
  servers: [
    { name: "files", command: "npx", args: ["-y", "files-mcp"], enabled: true, source: "global" },
    {
      name: "github",
      url: "https://mcp.example.com/github",
      enabled: true,
      source: "project",
      headers_env: { Authorization: "GITHUB_TOKEN" },
    },
  ],
  project_open: true,
  live_lead: true,
  statuses: [{ name: "files", connected: true, tool_count: 3, restarts: 0 }],
};

/** A saved stdio server with the fields the form does not show. */
const SAVED_STDIO: McpServer = {
  name: "files",
  enabled: true,
  command: "npx",
  args: ["-y", "files-mcp"],
  env: ["HOME", "PATH"],
  headers_env: {},
  scopes: [],
  trusted: true,
  idle_timeout_secs: 120,
  source: "global",
};

/** A saved remote server with OAuth + header settings the form does not show. */
const SAVED_REMOTE: McpServer = {
  name: "github",
  enabled: true,
  url: "https://mcp.example.com/github",
  args: [],
  env: [],
  headers_env: { Authorization: "GITHUB_TOKEN" },
  auth: "oauth",
  client_id: "mnemo-desktop",
  scopes: ["repo"],
  trusted: false,
  source: "project",
};

describe("MCP server normalization", () => {
  it("fills the defaults serde omits", () => {
    expect(normalizeServer({ name: "files", command: "npx" })).toEqual({
      name: "files",
      enabled: true,
      command: "npx",
      args: [],
      url: undefined,
      env: [],
      headers_env: {},
      auth: undefined,
      client_id: undefined,
      scopes: [],
      trusted: false,
      idle_timeout_secs: undefined,
      source: "global",
    });
  });

  it("keeps an explicit project source and unknown sources fall back to global", () => {
    expect(normalizeServer({ name: "x", command: "npx", source: "project" }).source).toBe("project");
    expect(normalizeServer({ name: "x", command: "npx", source: "elsewhere" }).source).toBe("global");
  });

  it("normalizes a whole snapshot", () => {
    const snapshot = normalizeMcpSettings(WIRE);
    expect(snapshot.servers.map((server) => server.name)).toEqual(["files", "github"]);
    expect(snapshot.servers[0].args).toEqual(["-y", "files-mcp"]);
    expect(snapshot.servers[1].headers_env).toEqual({ Authorization: "GITHUB_TOKEN" });
    expect(snapshot.project_open).toBe(true);
    expect(snapshot.live_lead).toBe(true);
    expect(snapshot.statuses).toEqual(WIRE.statuses);
  });
});

describe("MCP row metadata", () => {
  it("names both sources and both transports", () => {
    expect(MCP_SOURCES.map((source) => source.id)).toEqual(["global", "project"]);
    expect(MCP_TRANSPORTS.map((transport) => transport.id)).toEqual(["stdio", "remote"]);
    for (const entry of [...MCP_SOURCES, ...MCP_TRANSPORTS]) {
      expect(entry.label.trim(), entry.id).not.toBe("");
      expect(entry.hint.trim(), entry.id).not.toBe("");
    }
  });

  it("summarizes each transport and reports which one is set", () => {
    expect(serverTransport(SAVED_STDIO)).toBe("stdio");
    expect(serverSummary(SAVED_STDIO)).toBe("npx -y files-mcp");
    expect(serverTransport(SAVED_REMOTE)).toBe("remote");
    expect(serverSummary(SAVED_REMOTE)).toBe("https://mcp.example.com/github");
    expect(serverSummary({ ...SAVED_STDIO, command: undefined })).toBe("(no transport set)");
  });

  it("finds a live status by name only", () => {
    expect(statusFor(WIRE.statuses, "files")?.tool_count).toBe(3);
    expect(statusFor(WIRE.statuses, "github")).toBeUndefined();
  });

  it("keys the unsaved-changes comparison by content", () => {
    expect(serversKey([SAVED_STDIO])).toBe(serversKey([{ ...SAVED_STDIO }]));
    expect(serversKey([SAVED_STDIO])).not.toBe(serversKey([{ ...SAVED_STDIO, trusted: false }]));
  });
});

describe("MCP server drafts", () => {
  it("starts a blank stdio draft in the requested scope", () => {
    expect(emptyServerDraft("project")).toEqual({
      name: "",
      enabled: true,
      transport: "stdio",
      command: "",
      args: "",
      url: "",
      trusted: false,
      source: "project",
    });
  });

  it("round-trips a saved server into the form", () => {
    const draft = draftFromServer(SAVED_STDIO);
    expect(draft.transport).toBe("stdio");
    expect(draft.command).toBe("npx");
    expect(draft.args).toBe("-y\nfiles-mcp");
    expect(draftFromServer(SAVED_REMOTE).url).toBe("https://mcp.example.com/github");
  });

  it("splits arguments one per line, keeping inner spaces and dropping blank lines", () => {
    expect(splitArgs("-y\n\n  --root C:\\My Files  \n")).toEqual(["-y", "--root C:\\My Files"]);
    expect(splitArgs("")).toEqual([]);
  });
});

describe("draftToServer", () => {
  it("rejects a missing or malformed name", () => {
    const draft = { ...emptyServerDraft("global"), command: "npx" };
    expect(draftToServer(draft)).toMatchObject({ ok: false, error: expect.stringContaining("name") });
    expect(draftToServer({ ...draft, name: "two words" })).toMatchObject({
      ok: false,
      error: expect.stringContaining("spaces"),
    });
    expect(draftToServer({ ...draft, name: "a__b" })).toMatchObject({
      ok: false,
      error: expect.stringContaining("__"),
    });
  });

  it("requires the chosen transport's field and ignores the other one", () => {
    const stdio = { ...emptyServerDraft("global"), name: "files" };
    expect(draftToServer(stdio)).toMatchObject({
      ok: false,
      error: expect.stringContaining("command"),
    });
    const remote = { ...emptyServerDraft("global"), name: "remote", transport: "remote" as const };
    expect(draftToServer(remote)).toMatchObject({ ok: false, error: expect.stringContaining("URL") });
    expect(draftToServer({ ...remote, url: "ftp://example.com/mcp" })).toMatchObject({
      ok: false,
      error: expect.stringContaining("http"),
    });
    // Text typed into the inactive field never leaks into the saved server.
    const built = draftToServer({ ...stdio, command: "npx", url: "https://stale.example.com" });
    expect(built.ok).toBe(true);
    if (built.ok) {
      expect(built.server.command).toBe("npx");
      expect(built.server.url).toBeUndefined();
    }
  });

  it("builds a new stdio server with the form's fields", () => {
    const built = draftToServer({
      name: " files ",
      enabled: false,
      transport: "stdio",
      command: "npx",
      args: "-y\nfiles-mcp",
      url: "",
      trusted: true,
      source: "project",
    });
    expect(built).toEqual({
      ok: true,
      server: {
        name: "files",
        enabled: false,
        command: "npx",
        args: ["-y", "files-mcp"],
        env: [],
        headers_env: {},
        scopes: [],
        trusted: true,
        source: "project",
      },
    });
  });

  it("keeps the settings the form does not show when editing", () => {
    const edited = draftToServer(
      { ...draftFromServer(SAVED_STDIO), args: "-y\nfiles-mcp\n--safe" },
      SAVED_STDIO,
    );
    expect(edited.ok).toBe(true);
    if (edited.ok) {
      expect(edited.server.env).toEqual(["HOME", "PATH"]);
      expect(edited.server.idle_timeout_secs).toBe(120);
      expect(edited.server.args).toEqual(["-y", "files-mcp", "--safe"]);
    }
    const remote = draftToServer({ ...draftFromServer(SAVED_REMOTE), trusted: true }, SAVED_REMOTE);
    expect(remote.ok).toBe(true);
    if (remote.ok) {
      expect(remote.server.auth).toBe("oauth");
      expect(remote.server.client_id).toBe("mnemo-desktop");
      expect(remote.server.scopes).toEqual(["repo"]);
      expect(remote.server.headers_env).toEqual({ Authorization: "GITHUB_TOKEN" });
    }
  });

  it("drops the settings the chosen transport cannot use", () => {
    // Remote -> stdio must not carry auth/client_id/scopes/headers_env into a
    // definition the shared validator rejects (or that means nothing there).
    const built = draftToServer(
      { ...draftFromServer(SAVED_REMOTE), transport: "stdio", command: "uvx", url: "" },
      SAVED_REMOTE,
    );
    expect(built.ok).toBe(true);
    if (built.ok) {
      expect(built.server.headers_env).toEqual({});
      expect(built.server.scopes).toEqual([]);
      expect(built.server.auth).toBeUndefined();
      expect(built.server.client_id).toBeUndefined();
      expect(built.server.url).toBeUndefined();
    }
  });

  it("flags a new global server a project twin would hide", () => {
    const draft = { ...emptyServerDraft("global"), name: "github" };
    expect(shadowedGlobalName([SAVED_STDIO, SAVED_REMOTE], draft)).toBe("github");
    // A project row is never shadowed, and a free name stays free.
    expect(shadowedGlobalName([SAVED_REMOTE], { ...draft, source: "project" })).toBeNull();
    expect(shadowedGlobalName([SAVED_STDIO], draft)).toBeNull();
  });
});

describe("MCP transport", () => {
  it("maps every method to its command and normalizes the reply", async () => {
    const invoked: { command: string; args?: Record<string, unknown> }[] = [];
    const invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
      invoked.push({ command, args });
      return command === "mcp_settings_test"
        ? { ok: true, tool_count: 1, tool_names: ["read_file"] }
        : WIRE;
    });
    const transport = createTauriMcpTransport({
      invoke: invoke as unknown as typeof import("@tauri-apps/api/core").invoke,
    });
    const servers = [SAVED_STDIO];

    await expect(transport.get()).resolves.toEqual(normalizeMcpSettings(WIRE));
    await expect(transport.save(servers)).resolves.toEqual(normalizeMcpSettings(WIRE));
    await expect(transport.test("files")).resolves.toEqual({
      ok: true,
      tool_count: 1,
      tool_names: ["read_file"],
    });

    expect(invoked).toEqual([
      { command: "mcp_settings_get", args: undefined },
      { command: "mcp_settings_save", args: { servers } },
      { command: "mcp_settings_test", args: { name: "files" } },
    ]);
  });

  it("prefers the preview transport and falls back to null in a plain browser", async () => {
    const planted: McpTransport = {
      get: async () => normalizeMcpSettings(WIRE) as McpSettings,
      save: async () => normalizeMcpSettings(WIRE) as McpSettings,
      test: async () => ({ ok: false, tool_count: 0, tool_names: [], error: "no host" }),
    };
    expect(createMcpTransport()).toBeNull();
    setPreviewMcpTransport(planted);
    expect(createMcpTransport()).toBe(planted);
    await expect(createMcpTransport()?.get()).resolves.toMatchObject({ project_open: true });
    setPreviewMcpTransport(null);
    expect(createMcpTransport()).toBeNull();
  });
});
