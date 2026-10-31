// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { Plus, ShieldAlert, X } from "lucide-react";
import { useState } from "react";
import { useAgentStore } from "../../hooks/useAgentStore";
import { useShallow } from "zustand/react/shallow";
import type { AgentId } from "../../lib/types";
import { cancel, errMsg, spawnAgent } from "../../lib/tauri";
import { Conversation } from "../chat/Conversation";
import { Tabs, TabsList, TabsTrigger } from "../ui/tabs";

/** Result of the add-agent form's submission (see submitAddAgent). */
export interface AddAgentResult {
  ok: boolean;
  /** The spawned agent's id — set only on success. */
  agentId: AgentId | null;
  /** Human-readable failure message — set only on failure. */
  error: string | null;
}

/**
 * Create a new agent from the tab-rail "+" form (plan 2c26d533).
 *
 * The form's pure IPC + store seam — no React state — exported so the
 * node-env vitest suite can drive the real spawn → register → activate
 * chain without DOM test infra (the openMarkdownTarget pattern in
 * MarkdownLink.test.tsx):
 *  - trims the name and fails fast on blank (no IPC call);
 *  - spawns via the `spawn_agent` IPC wrapper (spawnAgent); `reviewOnly`
 *    true asks the backend for the strict read-only reviewer tool surface
 *    (role "reviewer", REVIEWER_BASE_TOOLS);
 *  - on success registers the new agent in the SAME single registry
 *    runtime-spawned agents land in (useAgentEvents → registerAgents —
 *    no parallel bookkeeping) and activates its tab;
 *  - on failure returns errMsg's message for the inline form display; the
 *    caller keeps the form open with its inputs intact.
 */
export async function submitAddAgent(
  name: string,
  reviewOnly: boolean,
): Promise<AddAgentResult> {
  const trimmed = name.trim();
  if (!trimmed) {
    return { ok: false, agentId: null, error: "Agent name is required" };
  }
  try {
    const info = await spawnAgent(trimmed, reviewOnly);
    // Register exactly like runtime-spawned agents so the new tab's
    // name/model/parent come from the shared store, then make it active.
    useAgentStore.getState().registerAgents([info]);
    useAgentStore.getState().setActiveAgent(info.id);
    return { ok: true, agentId: info.id, error: null };
  } catch (e) {
    return { ok: false, agentId: null, error: errMsg(e) };
  }
}

/**
 * Top tab bar: one tab per agent (main + subagents). Clicking a tab makes it
 * active. Each tab shows a status indicator:
 * - yellow shield badge when `pendingApproval` is set (UI H1/H4)
 * - cyan pulsing dot while running
 * - dim hollow dot when idle
 *
 * Each tab renders two lines: the agent name on top, and the model id (not the
 * provider) below in smaller muted text — so you can see at a glance which
 * model each subagent is running.
 *
 * Subagent tabs (those with a recorded parent) show a hover-revealed close-x
 * that cancels the subagent (its tab disappears once it exits). The main agent
 * tab never shows a close-x.
 *
 * When a *non-active* agent has a pending approval, a sticky banner offers
 * "Switch & review". Subagents that hit a security approval are
 * auto-switched to (see useAgentEvents) so the user sees the prompt.
 *
 * Built on Radix Tabs: provides role="tablist"/"tab", aria-selected, and
 * left/right/Home/End arrow-key navigation for free.
 *
 * A "+" button at the end of the tab rail opens a small inline form that
 * spawns a new agent: a name (default "agent") and a "Read-only review
 * agent" checkbox (default off). The "+" is deliberately a PLAIN button — a
 * sibling of the TabsTrigger elements, never nested inside one — so there
 * are no nested buttons and Radix arrow-key navigation skips it (it is not
 * part of the tab collection). Checking the box spawns the agent with
 * reviewOnly=true, which the backend maps to the "reviewer" role and the
 * strict read-only reviewer tool surface — the same allowlist a spawned
 * review subagent gets. On success the new agent is registered exactly like
 * runtime-spawned agents and its tab becomes active; on failure the error
 * shows inline and the form stays open (see submitAddAgent above).
 */
export function MainPanel() {
  const activeAgent = useAgentStore((s) => s.activeAgent);
  const setActiveAgent = useAgentStore((s) => s.setActiveAgent);

  // Narrow selector for the active conversation state passed to Conversation.
  // Subscribes only to the specific active agent's state object (by id).
  const state = useAgentStore((s) =>
    s.activeAgent != null ? s.agents[s.activeAgent] : undefined,
  );

  // Narrow, shallow tab metadata — only the fields needed for the tab bar and
  // the "other approvals" banner. Avoids re-rendering on every field change
  // inside any agent's full AgentState (Perf M1).
  const tabs = useAgentStore(
    useShallow((s) => {
      const ids = Object.keys(s.agents).map((k) => Number(k) as AgentId);
      // Numeric keys enumerate in ascending order; sort for explicit stability.
      ids.sort((a, b) => a - b);
      return ids.map((id) => {
        const st = s.agents[id];
        const pending = st?.pendingApproval ?? null;
        return {
          id,
          name: s.agentNames[id] ?? `agent-${id}`,
          running: !!st?.running,
          needsApproval: pending != null,
          // Include the tool name for the non-active approval banner so the
          // UX information is preserved while keeping the selector narrow
          // (only the minimal shape, not the whole AgentState).
          pendingToolName: pending ? pending.toolName : null,
          // The model id (second line in the tab) + parent id (drives the
          // close-x: only subagents — parent_id !== null — can be closed).
          model: s.agentModels[id] ?? null,
          parentId: s.agentParents[id] ?? null,
        };
      });
    }),
  );

  // Derive other approvals (non-active agents with pendingApproval) from the
  // already-narrow tab metadata. Sorted by id for stable banner order.
  const otherApprovals = tabs
    .filter((t) => t.id !== activeAgent && t.needsApproval)
    .sort((a, b) => a.id - b.id);
  const otherApproval = otherApprovals[0];
  const otherApprovalCount = otherApprovals.length;

  // ── Add-agent form (the "+" button at the end of the tab rail) ──────────
  // All form state is local: the form is a transient UI affordance; the
  // created agent lands in the shared agent store via submitAddAgent above.
  const [addOpen, setAddOpen] = useState(false);
  // Name field — defaults to "agent", the backend's usual default naming.
  const [addName, setAddName] = useState("agent");
  // The "Read-only review agent" checkbox — off by default: a spawned agent
  // is privileged unless the user explicitly asks for the reviewer surface.
  const [addReviewOnly, setAddReviewOnly] = useState(false);
  // Inline error from a failed spawn; null while closed/clean.
  const [addError, setAddError] = useState<string | null>(null);
  // True while the spawn request is in flight — disables Create.
  const [addBusy, setAddBusy] = useState(false);

  /** Close and reset the add-agent form (Cancel, Esc, and after success). */
  const closeAddForm = () => {
    setAddOpen(false);
    setAddName("agent");
    setAddReviewOnly(false);
    setAddError(null);
  };

  /** Submit the add-agent form (Create / Enter). The IPC + store wiring
   * lives in submitAddAgent; this handler only owns the form's UI state:
   * close + reset on success, show the error inline and keep the inputs
   * intact otherwise so the user can fix the name and retry. */
  const submitAdd = async () => {
    if (addBusy || !addName.trim()) return;
    setAddBusy(true);
    setAddError(null);
    const res = await submitAddAgent(addName, addReviewOnly);
    setAddBusy(false);
    if (res.ok) {
      closeAddForm();
    } else {
      setAddError(res.error ?? "Failed to spawn agent");
    }
  };

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      {/* Agent tab bar */}
      {/* `value` must never be undefined: Radix reads an undefined value as
          "uncontrolled" and then warns ("Tabs is changing from uncontrolled to
          controlled") the moment the first agent exists. The empty string is
          the controlled spelling of "nothing selected" — no trigger matches
          it, so no tab renders active. */}
      <Tabs
        value={activeAgent !== null ? String(activeAgent) : ""}
        onValueChange={(v: string) => setActiveAgent(Number(v))}
        activationMode="automatic"
      >
        <TabsList className="flex items-stretch overflow-x-auto border-b border-border bg-bg-secondary">
          {tabs.map((t) => {
            const isActive = t.id === activeAgent;
            const name = t.name;
            const needsApproval = t.needsApproval;
            const isSubagent = t.parentId !== null;
            const title = needsApproval
              ? `${name} (needs approval)`
              : t.running
                ? `${name} (running)`
                : `${name} (idle)`;
            return (
              <TabsTrigger
                key={t.id}
                value={String(t.id)}
                className={`group flex h-10 items-center gap-2 border-b-2 px-3 text-sm transition-colors ${
                  isActive
                    ? "border-cyan-500 text-cyan-400"
                    : "border-transparent text-slate-500 hover:text-slate-300"
                } ${
                  needsApproval && !isActive
                    ? "ring-1 ring-yellow-500/50"
                    : ""
                }`}
                title={title}
              >
                {needsApproval ? (
                  <span
                    className="relative flex h-2.5 w-2.5 items-center justify-center"
                    aria-label="approval pending"
                  >
                    <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-yellow-400 opacity-40" />
                    <span className="relative h-2 w-2 rounded-full bg-yellow-400" />
                  </span>
                ) : (
                  <span
                    className={`h-2 w-2 shrink-0 rounded-full ${
                      t.running
                        ? "agent-running-dot bg-cyan-400"
                        : "border border-slate-500"
                    }`}
                  />
                )}
                <span className="flex min-w-0 flex-col leading-tight">
                  <span className="truncate">{name}</span>
                  {/* Second line: the model id (not the provider). Shown for
                      every agent that has a known model; muted + smaller so
                      the name stays primary. */}
                  {t.model && (
                    <span className="truncate text-[0.625em] text-slate-500">
                      {t.model}
                    </span>
                  )}
                </span>
                {needsApproval && (
                  <span className="rounded-sm bg-yellow-950/50 px-1 py-0.5 text-[0.625em] font-medium uppercase tracking-wide text-yellow-300">
                    approve
                  </span>
                )}
                {/* Close-x: only on subagent tabs (never the main agent).
                    Hover-revealed; stops propagation so clicking it cancels
                    the agent instead of switching to its tab. */}
                {isSubagent && (
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      e.preventDefault();
                      void cancel(t.id).catch((err) =>
                        console.error("failed to cancel subagent:", err),
                      );
                    }}
                    className="ml-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-sm text-slate-500 opacity-0 transition-opacity hover:bg-bg-tertiary hover:text-red-400 group-hover:opacity-100"
                    title="Close subagent"
                    aria-label={`Close ${name}`}
                  >
                    <X className="h-3 w-3" />
                  </button>
                )}
              </TabsTrigger>
            );
          })}
          {tabs.length === 0 && (
            <span className="px-2 py-1 text-xs text-slate-500">No agents</span>
          )}
          {/* Add agent: a "+" at the end of the rail. Deliberately a PLAIN
              button and a sibling of the TabsTrigger elements (never nested
              inside one) — no nested buttons (the f3dd412 fix), and Radix
              arrow-key navigation skips it since it is not part of the tab
              collection. */}
          <button
            type="button"
            onClick={() => {
              setAddOpen(true);
              setAddError(null);
            }}
            className="ml-1 flex h-7 w-7 shrink-0 self-center items-center justify-center rounded-sm text-slate-400 transition-colors hover:bg-bg-tertiary hover:text-cyan-400"
            title="Add agent"
            aria-label="Add agent"
          >
            <Plus className="h-4 w-4" />
          </button>
        </TabsList>
      </Tabs>

      {/* Add-agent inline form (plan 2c26d533): its own chrome row directly
          below the tab rail — NOT inside the scrollable TabsList (no
          overflow clipping) and clear of the tablist's keyboard semantics.
          Enter submits, Esc cancels; Create is disabled while the spawn
          request is in flight or the name is blank after trimming. A failed
          spawn shows the error inline and leaves the form + inputs intact
          so the user can fix the name and retry. */}
      {addOpen && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void submitAdd();
          }}
          onKeyDown={(e) => {
            // Esc cancels from anywhere inside the form (key events bubble
            // up from the focused input).
            if (e.key === "Escape") {
              e.preventDefault();
              closeAddForm();
            }
          }}
          aria-label="Add agent form"
          className="flex items-center gap-2 border-b border-border bg-bg-secondary px-3 py-1.5 text-sm"
        >
          <input
            value={addName}
            onChange={(e) => setAddName(e.target.value)}
            autoFocus
            required
            aria-label="Agent name"
            placeholder="Agent name"
            className="w-44 shrink-0 rounded-sm border border-border bg-bg-primary px-2.5 py-1 text-sm text-slate-200 outline-none focus:border-cyan-500"
          />
          <label className="flex shrink-0 cursor-pointer select-none items-center gap-1.5 text-xs text-slate-400">
            <input
              type="checkbox"
              checked={addReviewOnly}
              onChange={(e) => setAddReviewOnly(e.target.checked)}
              className="h-3.5 w-3.5 accent-(--accent-color)"
            />
            Read-only review agent
          </label>
          <button
            type="submit"
            disabled={addBusy || !addName.trim()}
            className="shrink-0 rounded-sm bg-cyan-600 px-3 py-1 text-xs font-medium text-white hover:bg-cyan-500 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {addBusy ? "Creating…" : "Create"}
          </button>
          <button
            type="button"
            onClick={closeAddForm}
            className="shrink-0 rounded-sm border border-border px-3 py-1 text-xs text-slate-400 hover:text-slate-200"
          >
            Cancel
          </button>
          {/* Inline spawn error (errMsg of the backend's IpcError DTO) —
              keeps the form + inputs intact; see submitAdd. */}
          {addError && (
            <span
              role="alert"
              className="min-w-0 flex-1 truncate text-xs text-red-400"
              title={addError}
            >
              {addError}
            </span>
          )}
        </form>
      )}

      {/* Non-active agent needs approval — banner only; never auto-steal focus. */}
      {otherApproval && (
        <div className="flex items-center gap-3 border-b border-yellow-600/40 bg-yellow-950/30 px-3 py-2 text-sm text-yellow-100">
          <ShieldAlert className="h-4 w-4 shrink-0 text-yellow-400" />
          <span className="min-w-0 flex-1 truncate">
            <span className="font-medium text-yellow-300">
              {otherApproval.name}
            </span>{" "}
            is waiting for approval
            {otherApproval.needsApproval && otherApproval.pendingToolName
              ? ` (${otherApproval.pendingToolName})`
              : ""}
            {otherApprovalCount > 1
              ? ` · ${otherApprovalCount} agents need approval`
              : ""}
            .
          </span>
          <button
            type="button"
            onClick={() => setActiveAgent(otherApproval.id)}
            className="shrink-0 rounded-sm bg-yellow-600 px-3 py-1 text-xs font-medium text-white hover:bg-yellow-500"
          >
            Switch &amp; review
          </button>
        </div>
      )}

      {/* Conversation */}
      <div className="flex-1 overflow-hidden">
        {state ? (
          <Conversation state={state} />
        ) : (
          <div className="flex h-full items-center justify-center text-slate-500">
            No agent active
          </div>
        )}
      </div>
    </div>
  );
}
