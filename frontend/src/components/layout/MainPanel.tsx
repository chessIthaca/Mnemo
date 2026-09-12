// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { ShieldAlert, X } from "lucide-react";
import { useAgentStore } from "../../hooks/useAgentStore";
import { useShallow } from "zustand/react/shallow";
import type { AgentId } from "../../lib/types";
import { cancel } from "../../lib/tauri";
import { Conversation } from "../chat/Conversation";
import { Tabs, TabsList, TabsTrigger } from "../ui/tabs";

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

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      {/* Agent tab bar */}
      <Tabs
        value={activeAgent !== null ? String(activeAgent) : undefined}
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
                  <span className="rounded bg-yellow-950/50 px-1 py-0.5 text-[0.625em] font-medium uppercase tracking-wide text-yellow-300">
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
                    className="ml-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded text-slate-500 opacity-0 transition-opacity hover:bg-bg-tertiary hover:text-red-400 group-hover:opacity-100"
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
        </TabsList>
      </Tabs>

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
            className="shrink-0 rounded bg-yellow-600 px-3 py-1 text-xs font-medium text-white hover:bg-yellow-500"
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
