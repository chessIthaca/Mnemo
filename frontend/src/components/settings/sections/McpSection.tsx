// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from "react";
import {
  errMsg,
  listMcpServers,
  mcpStatus,
  saveMcpServers,
  startMcpOauth,
  testMcpServer,
} from "../../../lib/tauri";
import type { McpServer, McpServerStatus, McpTestResult } from "../../../lib/types";
import {
  blankMcpServer,
  mcpStatusLine,
  mcpTransportSummary,
  serializeMcpServers,
  validateMcpDraft,
} from "../types";
import type { SettingsSectionHandle } from "../types";

export interface McpSectionProps {
  /** When true, the dialog is open — load on first activation. */
  active: boolean;
  /** Report dirty so the shell can confirm discard on close. */
  onDirtyChange: (dirty: boolean) => void;
}

/** Per-server Test state (the Settings Test button). */
type TestState =
  | { status: "testing" }
  | { status: "done"; result: McpTestResult };

/** The editable form state for one server (textareas hold one entry per
 *  line — args, env-var names, and `Header=ENV_NAME` rows respectively —
 *  avoiding quoting games in single-line inputs). */
interface McpEdit {
  index: number;
  name: string;
  enabled: boolean;
  transport: "stdio" | "remote";
  command: string;
  argsText: string;
  envText: string;
  url: string;
  headersText: string;
  /** "none" | "oauth" — oauth is remote-only (OAuth 2.1 browser login,
   *  tokens in keys.toml, automatic 401 refresh). */
  auth: string;
  clientId: string;
  scopesText: string;
  /** Per-server auto-approve trust (approval-only; state gates never widen). */
  trusted: boolean;
  /** Optional idle timeout in seconds (empty = keep until failure). */
  idleTimeoutText: string;
}

function toEdit(s: McpServer, index: number): McpEdit {
  return {
    index,
    name: s.name,
    enabled: s.enabled,
    transport: s.url && s.url.trim() !== "" ? "remote" : "stdio",
    command: s.command ?? "",
    argsText: (s.args ?? []).join("\n"),
    envText: (s.env ?? []).join("\n"),
    url: s.url ?? "",
    headersText: Object.entries(s.headers_env ?? {})
      .map(([h, v]) => `${h}=${v}`)
      .join("\n"),
    auth: s.auth ?? "none",
    clientId: s.client_id ?? "",
    scopesText: (s.scopes ?? []).join(" "),
    trusted: s.trusted ?? false,
    idleTimeoutText: s.idle_timeout_secs != null ? String(s.idle_timeout_secs) : "",
  };
}

function fromEdit(e: McpEdit): McpServer {
  const lines = (t: string) =>
    t
      .split("\n")
      .map((s) => s.trim())
      .filter((s) => s !== "");
  const headers_env: Record<string, string> = {};
  for (const line of lines(e.headersText)) {
    const eq = line.indexOf("=");
    if (eq > 0) {
      headers_env[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
    }
  }
  const oauth = e.transport === "remote" && e.auth === "oauth";
  return {
    name: e.name.trim(),
    enabled: e.enabled,
    command: e.transport === "stdio" ? e.command.trim() || null : null,
    args: e.transport === "stdio" ? lines(e.argsText) : [],
    env: e.transport === "stdio" ? lines(e.envText) : [],
    url: e.transport === "remote" ? e.url.trim() || null : null,
    headers_env: e.transport === "remote" ? headers_env : {},
    auth: oauth ? "oauth" : null,
    client_id: oauth ? e.clientId.trim() || null : null,
    scopes: oauth ? e.scopesText.split(/\s+/).filter(Boolean) : [],
    trusted: e.trusted,
    idle_timeout_secs:
      e.idleTimeoutText.trim() === ""
        ? null
        : Number(e.idleTimeoutText.trim()) > 0
          ? Number(e.idleTimeoutText.trim())
          : null,
  };
}

const inputCls =
  "w-full rounded-lg border border-border bg-bg-primary px-3 py-1.5 text-sm text-[color:var(--text-primary)] outline-none focus:border-[color:var(--accent-color)]";
const btnCls =
  "rounded-lg border border-border px-3 py-1 text-xs text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)] disabled:opacity-40";

/**
 * MCP section — configure Model Context Protocol servers (`mcp.toml`).
 * Servers expose their tools to the agent as a deferred group
 * (`load_tools "mcp.<name>"`); every MCP tool call goes through the
 * approval gate. This section manages the WHOLE set: Save replaces it
 * (validate → write → live manager swap). Test connects + lists a server's
 * tools — only saved servers can be tested (the live manager knows the
 * saved set), so the button is disabled while the section is dirty.
 */
export const McpSection = forwardRef<SettingsSectionHandle, McpSectionProps>(
  function McpSection({ active, onDirtyChange }, ref) {
    const [draft, setDraft] = useState<McpServer[] | null>(null);
    const [snapshot, setSnapshot] = useState("");
    const [loading, setLoading] = useState(false);
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [ok, setOk] = useState<string | null>(null);
    const [tests, setTests] = useState<Record<string, TestState>>({});
    const [oauthNote, setOauthNote] = useState<string | null>(null);
    const [statuses, setStatuses] = useState<Record<string, McpServerStatus>>({});
    const [editing, setEditing] = useState<McpEdit | null>(null);

    useEffect(() => {
      if (!active || draft !== null || loading) return;
      setLoading(true);
      listMcpServers()
        .then((servers) => {
          setDraft(servers);
          setSnapshot(serializeMcpServers(servers));
          setError(null);
        })
        .catch((e) => setError(errMsg(e)))
        .finally(() => setLoading(false));
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [active]);

    /** Refresh the live per-server status lines (pure read, never connects). */
    function refreshStatus() {
      mcpStatus()
        .then((list) => {
          const byName: Record<string, McpServerStatus> = {};
          for (const s of list) byName[s.name] = s;
          setStatuses(byName);
        })
        .catch(() => {
          // Best-effort: status lines just stay stale.
        });
    }

    useEffect(() => {
      if (active) refreshStatus();
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [active]);

    const dirty = useMemo(() => {
      if (!draft) return false;
      return serializeMcpServers(draft) !== snapshot;
    }, [draft, snapshot]);

    useEffect(() => {
      onDirtyChange(dirty);
    }, [dirty, onDirtyChange]);

    function patchAt(index: number, p: Partial<McpServer>) {
      setDraft((prev) =>
        prev ? prev.map((s, i) => (i === index ? { ...s, ...p } : s)) : prev,
      );
    }

    function removeAt(index: number) {
      setEditing(null);
      setDraft((prev) => (prev ? prev.filter((_, i) => i !== index) : prev));
    }

    async function runTest(name: string) {
      setTests((p) => ({ ...p, [name]: { status: "testing" } }));
      try {
        const result = await testMcpServer(name);
        setTests((p) => ({ ...p, [name]: { status: "done", result } }));
        refreshStatus();
      } catch (e) {
        setTests((p) => ({
          ...p,
          [name]: {
            status: "done",
            result: { ok: false, tool_count: 0, tool_names: [], error: errMsg(e) },
          },
        }));
      }
    }

    /** Start the OAuth 2.1 flow for a SAVED remote server: open the
     *  authorization URL in the browser; the redirect completes the
     *  exchange in a background task and stores the tokens. */
    async function connectOauth(name: string) {
      try {
        const start = await startMcpOauth(name);
        window.open(start.authorize_url, "_blank");
        setOauthNote(start.note);
        refreshStatus();
      } catch (e) {
        setOauthNote(errMsg(e));
      }
    }

    useImperativeHandle(ref, () => ({
      save: async () => {
        if (!draft) return false;
        const invalid = validateMcpDraft(draft);
        if (invalid) {
          setError(invalid);
          return false;
        }
        setSaving(true);
        setError(null);
        setOk(null);
        try {
          const note = await saveMcpServers(draft);
          setSnapshot(serializeMcpServers(draft));
          setOk(note);
          refreshStatus();
          window.setTimeout(() => setOk(null), 6000);
          return true;
        } catch (e) {
          setError(errMsg(e));
          return false;
        } finally {
          setSaving(false);
        }
      },
    }));

    if (loading && !draft) {
      return (
        <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
          Loading MCP servers…
        </div>
      );
    }
    if (!draft) {
      return (
        <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
          {error ?? "MCP servers unavailable."}
        </div>
      );
    }

    const inputId = "mcp-";

    return (
      <div className="space-y-5">
        <div className="space-y-3">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
            MCP Servers
          </h3>
          <p className="text-xs text-[color:var(--text-muted)]">
            Servers expose their tools to the agent as a deferred group — the agent calls{" "}
            <code>load_tools</code> with <code>mcp.&lt;name&gt;</code> on first use. Every MCP
            tool call goes through your approval gate. Env entries are variable NAMES; values
            come from your environment (never stored here).
          </p>

          {draft.length === 0 && (
            <div className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-xs text-[color:var(--text-muted)]">
              No MCP servers configured. Add one to expose its tools to the agent.
            </div>
          )}

          {draft.map((s, i) => {
            const t = tests[s.name];
            return (
              <div
                key={`${s.name}-${i}`}
                className="space-y-2 rounded-lg border border-border bg-bg-tertiary/50 px-3 py-2.5"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium text-[color:var(--text-primary)]">
                    {s.name || "(unnamed)"}
                  </span>
                  <select
                    value={s.source ?? "global"}
                    onChange={(e) =>
                      patchAt(i, { source: e.target.value as "global" | "project" })
                    }
                    title="Which file this server is saved to — project overrides win by name over global"
                    className="rounded border border-border bg-bg-primary px-1 py-0.5 text-[0.7rem] text-[color:var(--text-muted)]"
                  >
                    <option value="global">global</option>
                    <option value="project">project</option>
                  </select>
                  <span className="min-w-0 flex-1 truncate text-xs text-[color:var(--text-muted)]">
                    {mcpTransportSummary(s)}
                  </span>
                  <label className="flex items-center gap-1.5 text-xs text-[color:var(--text-muted)]">
                    <input
                      type="checkbox"
                      checked={s.enabled}
                      onChange={(e) => patchAt(i, { enabled: e.target.checked })}
                      className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
                    />
                    enabled
                  </label>
                  <label
                    className="flex items-center gap-1.5 text-xs text-[color:var(--text-muted)]"
                    title="Trusted servers' tools run without per-call approval prompts (approval-only — the plan-first state gates never widen)"
                  >
                    <input
                      type="checkbox"
                      checked={s.trusted ?? false}
                      onChange={(e) => patchAt(i, { trusted: e.target.checked })}
                      className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
                    />
                    trusted
                  </label>
                  <button
                    type="button"
                    onClick={() => void runTest(s.name)}
                    disabled={dirty || saving || !s.name.trim()}
                    title={dirty ? "Save first — Test runs against the saved set" : "Connect + list tools"}
                    className={btnCls}
                  >
                    {t?.status === "testing" ? "Testing…" : "Test"}
                  </button>
                  {(s.auth ?? "none") === "oauth" && (s.url ?? "").trim() !== "" && (
                    <button
                      type="button"
                      onClick={() => void connectOauth(s.name)}
                      disabled={dirty || saving || !s.name.trim()}
                      title={
                        dirty
                          ? "Save first — Connect runs against the saved set"
                          : "Open the OAuth login in your browser"
                      }
                      className={btnCls}
                    >
                      Connect
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => setEditing(editing?.index === i ? null : toEdit(s, i))}
                    className={btnCls}
                  >
                    {editing?.index === i ? "Close" : "Edit"}
                  </button>
                  <button
                    type="button"
                    onClick={() => removeAt(i)}
                    className={btnCls}
                  >
                    Remove
                  </button>
                </div>

                {t?.status === "done" && (
                  <div
                    className={`rounded-lg border px-3 py-1.5 text-xs ${
                      t.result.ok
                        ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-300"
                        : "border-red-500/40 bg-red-500/10 text-red-300"
                    }`}
                  >
                    {t.result.ok
                      ? `Connected — ${t.result.tool_count} tool(s): ${t.result.tool_names.join(", ")}`
                      : t.result.error ?? "Test failed."}
                  </div>
                )}

                {(() => {
                  const line = mcpStatusLine(statuses[s.name]);
                  if (line === null) return null;
                  return (
                    <div
                      className={`flex items-center gap-2 text-xs ${
                        statuses[s.name]?.connected
                          ? "text-emerald-300"
                          : statuses[s.name]?.last_error
                            ? "text-red-300"
                            : "text-[color:var(--text-muted)]"
                      }`}
                    >
                      {line}
                    </div>
                  );
                })()}

                {editing?.index === i && (
                  <div className="space-y-2 border-t border-border pt-2">
                    <div className="grid grid-cols-2 gap-2">
                      <label className="space-y-1">
                        <span className="text-xs text-[color:var(--text-muted)]">Name</span>
                        <input
                          id={`${inputId}name-${i}`}
                          value={editing.name}
                          onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                          className={inputCls}
                          placeholder="filesystem"
                        />
                      </label>
                      <label className="space-y-1">
                        <span className="text-xs text-[color:var(--text-muted)]">Transport</span>
                        <select
                          value={editing.transport}
                          onChange={(e) =>
                            setEditing({
                              ...editing,
                              transport: e.target.value as "stdio" | "remote",
                            })
                          }
                          className={inputCls}
                        >
                          <option value="stdio">stdio (spawn a command)</option>
                          <option value="remote">remote (HTTP URL)</option>
                        </select>
                      </label>
                    </div>
                    {editing.transport === "stdio" ? (
                      <div className="space-y-2">
                        <label className="block space-y-1">
                          <span className="text-xs text-[color:var(--text-muted)]">Command</span>
                          <input
                            value={editing.command}
                            onChange={(e) => setEditing({ ...editing, command: e.target.value })}
                            className={inputCls}
                            placeholder="npx"
                          />
                        </label>
                        <label className="block space-y-1">
                          <span className="text-xs text-[color:var(--text-muted)]">
                            Arguments (one per line)
                          </span>
                          <textarea
                            value={editing.argsText}
                            onChange={(e) => setEditing({ ...editing, argsText: e.target.value })}
                            className={`${inputCls} font-mono text-xs`}
                            rows={2}
                            placeholder={"-y\n@modelcontextprotocol/server-filesystem"}
                          />
                        </label>
                        <label className="block space-y-1">
                          <span className="text-xs text-[color:var(--text-muted)]">
                            Env var names (one per line — values come from your environment)
                          </span>
                          <textarea
                            value={editing.envText}
                            onChange={(e) => setEditing({ ...editing, envText: e.target.value })}
                            className={`${inputCls} font-mono text-xs`}
                            rows={2}
                            placeholder="GITHUB_TOKEN"
                          />
                        </label>
                      </div>
                    ) : (
                      <div className="space-y-2">
                        <label className="block space-y-1">
                          <span className="text-xs text-[color:var(--text-muted)]">URL</span>
                          <input
                            value={editing.url}
                            onChange={(e) => setEditing({ ...editing, url: e.target.value })}
                            className={inputCls}
                            placeholder="https://mcp.example.com/mcp"
                          />
                        </label>
                        <label className="block space-y-1">
                          <span className="text-xs text-[color:var(--text-muted)]">Auth</span>
                          <select
                            value={editing.auth}
                            onChange={(e) => setEditing({ ...editing, auth: e.target.value })}
                            className={inputCls}
                          >
                            <option value="none">none</option>
                            <option value="oauth">
                              oauth (browser login, tokens in keys.toml)
                            </option>
                          </select>
                        </label>
                        {editing.auth === "oauth" && (
                          <label className="block space-y-1">
                            <span className="text-xs text-[color:var(--text-muted)]">
                              OAuth client id (pre-registered with the server)
                            </span>
                            <input
                              value={editing.clientId}
                              onChange={(e) =>
                                setEditing({ ...editing, clientId: e.target.value })
                              }
                              className={inputCls}
                              placeholder="my-registered-client"
                            />
                          </label>
                        )}
                        {editing.auth === "oauth" && (
                          <label className="block space-y-1">
                            <span className="text-xs text-[color:var(--text-muted)]">
                              OAuth scopes (space-separated)
                            </span>
                            <input
                              value={editing.scopesText}
                              onChange={(e) =>
                                setEditing({ ...editing, scopesText: e.target.value })
                              }
                              className={inputCls}
                              placeholder="read write"
                            />
                          </label>
                        )}
                        <label className="block space-y-1">
                          <span className="text-xs text-[color:var(--text-muted)]">
                            Headers (one per line: Header=ENV_VAR_NAME — values come from your
                            environment)
                          </span>
                          <textarea
                            value={editing.headersText}
                            onChange={(e) =>
                              setEditing({ ...editing, headersText: e.target.value })
                            }
                            className={`${inputCls} font-mono text-xs`}
                            rows={2}
                            placeholder="Authorization=MY_MCP_TOKEN"
                          />
                        </label>
                      </div>
                    )}
                    <label className="block space-y-1">
                      <span className="text-xs text-[color:var(--text-muted)]">
                        Idle timeout (seconds — empty = keep the connection until it fails)
                      </span>
                      <input
                        type="number"
                        min={1}
                        value={editing.idleTimeoutText}
                        onChange={(e) =>
                          setEditing({ ...editing, idleTimeoutText: e.target.value })
                        }
                        className={inputCls}
                        placeholder="300"
                      />
                    </label>
                    <div className="flex justify-end">
                      <button
                        type="button"
                        onClick={() => {
                          patchAt(i, fromEdit(editing));
                          setEditing(null);
                        }}
                        className={btnCls}
                      >
                        Apply
                      </button>
                    </div>
                  </div>
                )}
              </div>
            );
          })}

          <button
            type="button"
            onClick={() => setDraft([...draft, blankMcpServer()])}
            className="rounded-lg border border-dashed border-border px-3 py-1.5 text-xs text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
          >
            + Add server
          </button>
          <button
            type="button"
            onClick={refreshStatus}
            className="rounded-lg border border-border px-3 py-1.5 text-xs text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
            title="Refresh the live status lines"
          >
            Refresh status
          </button>
        </div>

        {error && (
          <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {error}
          </div>
        )}
        {ok && (
          <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
            {ok}
          </div>
        )}
        {oauthNote && (
          <div className="rounded-lg border border-sky-500/40 bg-sky-500/10 px-3 py-2 text-xs text-sky-300">
            {oauthNote}
          </div>
        )}
        {saving && (
          <div className="text-xs text-[color:var(--text-muted)]">Saving…</div>
        )}
      </div>
    );
  },
);
