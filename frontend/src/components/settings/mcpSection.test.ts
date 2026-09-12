// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Unit tests for the MCP settings helpers (settings/types.ts): draft
// creation, shape-stable serialization, transport summaries, and the
// client-side validation that mirrors the backend's mcp.toml rules
// (unique names, exactly one transport, env-var NAMES only).

import { describe, expect, it } from "vitest";

import {
  blankMcpServer,
  mcpStatusLine,
  mcpTransportSummary,
  serializeMcpServers,
  validateMcpDraft,
} from "./types";
import type { McpServer, McpServerStatus } from "../../lib/types";

/** A minimal valid stdio server. */
function stdio(over: Partial<McpServer> = {}): McpServer {
  return { ...blankMcpServer(), name: "fs", command: "npx", ...over };
}

/** A minimal mcp_status row (default: never connected, no restarts). */
function status(over: Partial<McpServerStatus> = {}): McpServerStatus {
  return {
    name: "fs",
    connected: false,
    tool_count: 0,
    last_error: null,
    restarts: 0,
    idle_timeout_secs: null,
    ...over,
  };
}

describe("mcp settings helpers", () => {
  it("blankMcpServer starts enabled and transport-less", () => {
    const b = blankMcpServer();
    expect(b.enabled).toBe(true);
    expect(b.command).toBeNull();
    expect(b.url).toBeNull();
    expect(validateMcpDraft([b])).toContain("name");
  });

  it("serializeMcpServers is shape-stable across optional-field absence", () => {
    // A sparse server (optional fields absent) serializes identically to
    // one with explicit nulls/empties — dirty comparison never false-
    // positives on shape drift between loads and edits.
    const sparse = [{ name: "x", enabled: true }];
    const explicit = [
      {
        name: "x",
        enabled: true,
        command: null,
        args: [],
        env: [],
        url: null,
        headers_env: {},
      },
    ];
    expect(serializeMcpServers(sparse as McpServer[])).toBe(
      serializeMcpServers(explicit as McpServer[]),
    );
  });

  it("mcpTransportSummary renders both transports", () => {
    expect(mcpTransportSummary(stdio({ args: ["-y", "pkg"] }))).toBe("stdio: npx -y pkg");
    expect(
      mcpTransportSummary({ ...blankMcpServer(), name: "r", url: "https://x.example/mcp" }),
    ).toBe("remote: https://x.example/mcp");
    expect(mcpTransportSummary(blankMcpServer())).toContain("no command");
  });

  it("validateMcpDraft accepts a valid mixed set", () => {
    const set = [
      stdio(),
      { ...blankMcpServer(), name: "remote", url: "https://x.example/mcp" },
    ];
    expect(validateMcpDraft(set)).toBeNull();
  });

  it("validateMcpDraft rejects duplicate names", () => {
    expect(validateMcpDraft([stdio(), stdio()])).toContain("Duplicate");
  });

  it("validateMcpDraft keeps names revealable and collision-free", () => {
    // LOW 3 mirror: whitespace (unrevealable mcp.<name> group) and '__'
    // (mcp__<server>__<tool> aliasing) are rejected client-side too.
    expect(validateMcpDraft([stdio({ name: "fs " })])).toContain("whitespace");
    expect(validateMcpDraft([stdio({ name: "a__b" })])).toContain("__");
  });

  it("validateMcpDraft enforces idle timeout and trust rules", () => {
    // A zero/negative idle timeout is rejected (mirrors the backend).
    expect(validateMcpDraft([stdio({ idle_timeout_secs: 0 })])).toContain("idle timeout");
    // trusted defaults to false and needs no validation (approval-only).
    const draft = stdio({ trusted: true, idle_timeout_secs: 300 });
    expect(validateMcpDraft([draft])).toBeNull();
    expect(serializeMcpServers([draft])).toContain('"trusted":true');
    expect(serializeMcpServers([draft])).toContain('"idle_timeout_secs":300');
  });

  it("mcpStatusLine renders connection state, errors, and restarts", () => {
    // Never seen this session: no status line at all.
    expect(mcpStatusLine(undefined)).toBeNull();
    // Connected: tool count with plural handling.
    expect(mcpStatusLine(status({ connected: true, tool_count: 1 }))).toBe(
      "connected · 1 tool",
    );
    expect(mcpStatusLine(status({ connected: true, tool_count: 3 }))).toBe(
      "connected · 3 tools",
    );
    // Not connected: the last error, or the never-connected line.
    expect(mcpStatusLine(status({ connected: false, last_error: "boom" }))).toBe(
      "error: boom",
    );
    expect(mcpStatusLine(status({ connected: false }))).toBe(
      "not connected this session",
    );
    // Restarts append the badge (the crash/respawn counter).
    expect(
      mcpStatusLine(status({ connected: true, tool_count: 2, restarts: 2 })),
    ).toBe("connected · 2 tools (restarted 2×)");
  });

  it("validateMcpDraft enforces the oauth auth rules", () => {
    // oauth on a stdio server is rejected (remote-only).
    expect(validateMcpDraft([stdio({ auth: "oauth", client_id: "c" })])).toContain("remote");
    // oauth without a pre-registered client_id is rejected.
    expect(
      validateMcpDraft([
        { ...blankMcpServer(), name: "r", url: "https://x", auth: "oauth" },
      ]),
    ).toContain("client_id");
    // A valid oauth remote passes.
    expect(
      validateMcpDraft([
        {
          ...blankMcpServer(),
          name: "r",
          url: "https://x",
          auth: "oauth",
          client_id: "cid",
        },
      ]),
    ).toBeNull();
    // Unknown schemes are rejected.
    expect(
      validateMcpDraft([
        { ...blankMcpServer(), name: "r", url: "https://x", auth: "magic" },
      ]),
    ).toContain("unknown auth");
  });

  it("validateMcpDraft enforces exactly-one-transport", () => {
    expect(validateMcpDraft([stdio({ url: "https://x" })])).toContain("one transport");
    const neither = stdio();
    neither.command = null;
    expect(validateMcpDraft([neither])).toContain("command (stdio) or a url");
  });

  it("validateMcpDraft keeps values out of the file (names only)", () => {
    // `K=V` in env smuggles a secret into mcp.toml — rejected.
    expect(validateMcpDraft([stdio({ env: ["TOKEN=secret"] })])).toContain("variable NAMES");
    // A literal header value in headers_env is likewise rejected.
    expect(
      validateMcpDraft([
        {
          ...blankMcpServer(),
          name: "r",
          url: "https://x",
          headers_env: { Authorization: "Bearer abc" },
        },
      ]),
    ).toContain("env-var NAMES");
  });
});
