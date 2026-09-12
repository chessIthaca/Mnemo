// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useState, useRef } from "react";
import { Target, ArrowLeft } from "lucide-react";
import { Markdown } from "../chat/Markdown";
import { getWorkflowState, getPlan, errMsg } from "../../lib/tauri";
import { useAgentStore } from "../../hooks/useAgentStore";
import { PlanStepRow } from "./PlanStepRow";
import type { PlanFile, PlanStep } from "../../lib/types";

export function PlanProgress() {
  const [plan, setPlan] = useState<PlanFile | null>(null);
  const [state, setState] = useState<string>("");
  const [depth, setDepth] = useState<number>(0);
  const [parents, setParents] = useState<{ id: string; title: string }[]>([]);
  const [activeSkill, setActiveSkill] = useState<import("../../lib/types").ActiveSkillInfo | null>(null);
  // When viewing an ancestor plan (clicked in the staircase), this holds the
  // read-only plan being viewed. `null` = showing the active plan.
  const [viewingAncestor, setViewingAncestor] = useState<PlanFile | null>(null);
  const [viewingTitle, setViewingTitle] = useState<string>("");
  const [viewError, setViewError] = useState<string | null>(null);
  // Step-details expansion (backlog af572504): rows are collapsed by
  // default; the ACTIVE step (the first not-done one, while executing)
  // auto-expands and auto-collapses again once it completes. Manual
  // toggles record an override so a user-opened (or user-collapsed) row
  // keeps its choice; absent = follow the active-step default.
  const [expandOverrides, setExpandOverrides] = useState<
    Record<number, boolean>
  >({});
  // Ancestor-view rows are read-only — collapsed by default, no auto-expand.
  const [ancestorOverrides, setAncestorOverrides] = useState<
    Record<number, boolean>
  >({});

  // Subscribe to planVersion — it bumps whenever the plan changes (created,
  // step completed, state transitioned). We re-fetch immediately on each bump
  // so the plan window updates in real time, with the 2s poll as a fallback.
  const planVersion = useAgentStore((s) => s.planVersion);
  // Subscribe to the active agent — each agent owns its own workflow, so
  // switching agents must reload the plan tab with the new agent's plan.
  const activeAgent = useAgentStore((s) => s.activeAgent);
  // Track the last version + agent we fetched to avoid duplicate fetches.
  const lastFetchedVersion = useRef<number>(-1);
  const lastFetchedAgent = useRef<number | null>(null);

  async function refresh() {
    if (activeAgent === null) return;
    try {
      const wf = await getWorkflowState(activeAgent);
      setPlan(wf.plan);
      setState(wf.state);
      setDepth(wf.depth ?? 0);
      setParents(Array.isArray(wf.parents) ? wf.parents : []);
      setActiveSkill(wf.skill ?? null);
    } catch (e) {
      console.error("failed to get workflow state:", e);
    }
  }

  /** Click an ancestor in the staircase to view it read-only. */
  async function viewAncestor(ancestor: { id: string; title: string }) {
    if (activeAgent === null) return;
    setViewError(null);
    // Each ancestor is a different plan — don't leak the previous
    // ancestor's expansion overrides into this one (index-keyed).
    setAncestorOverrides({});
    try {
      const p = await getPlan(activeAgent, ancestor.id);
      setViewingAncestor(p);
      setViewingTitle(ancestor.title);
    } catch (e) {
      setViewError(errMsg(e));
    }
  }

  useEffect(() => {
    // Re-fetch whenever planVersion changes (real-time update).
    if (planVersion !== lastFetchedVersion.current) {
      lastFetchedVersion.current = planVersion;
      void refresh();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- fire only on planVersion change; refresh/activeAgent intentionally omitted
  }, [planVersion]);

  useEffect(() => {
    // Re-fetch whenever the active agent changes — each agent owns its own
    // workflow, so switching agents reloads the plan tab. Also clear any
    // ancestor-view state so we don't show the previous agent's ancestor.
    if (activeAgent !== lastFetchedAgent.current) {
      lastFetchedAgent.current = activeAgent;
      lastFetchedVersion.current = -1; // force a fetch even if version is same
      setViewingAncestor(null);
      setViewingTitle("");
      setViewError(null);
      setExpandOverrides({});
      setAncestorOverrides({});
      void refresh();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- fire only on activeAgent change; refresh intentionally omitted
  }, [activeAgent]);

  // Reset manual expansion when a different plan loads (create_plan
  // replaces the plan; update_plan appends/trims) — stale index-keyed
  // overrides must not leak across plans. PlanFile carries no id, so a
  // cheap signature (title + step count) is the identity signal; a
  // same-title same-length replacement is the one residual (cosmetic:
  // a row starts expanded contrary to default; a toggle self-corrects).
  const planSignature = plan ? `${plan.title}|${plan.steps.length}` : "";
  const lastPlanSignature = useRef("");
  useEffect(() => {
    if (planSignature !== lastPlanSignature.current) {
      lastPlanSignature.current = planSignature;
      setExpandOverrides({});
      setAncestorOverrides({});
    }
  }, [planSignature]);

  useEffect(() => {
    // Initial fetch + 2s polling fallback (catches changes that don't emit
    // events, e.g. an external edit to the plan file).
    void refresh();
    const interval = setInterval(refresh, 2000);
    return () => clearInterval(interval);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount-once poll; refresh intentionally omitted
  }, []);

  if (!plan) {
    return (
      <div className="flex h-full items-center justify-center text-[0.875em] text-slate-500">
        No plan yet. The agent will create one.
      </div>
    );
  }

  const completed = plan.steps.filter((s) => s.done).length;
  const total = plan.steps.length;
  const pct = total > 0 ? Math.round((completed / total) * 100) : 0;

  // The active step: the first not-done step while executing. It
  // auto-expands; when it completes it stops being first-not-done, so it
  // auto-collapses and the next step takes over.
  const activeIndex =
    state === "executing"
      ? plan.steps.find((s) => !s.done)?.index ?? null
      : null;

  function isExpanded(step: PlanStep): boolean {
    return expandOverrides[step.index] ?? step.index === activeIndex;
  }

  function toggleStep(step: PlanStep): void {
    setExpandOverrides((prev) => ({
      ...prev,
      [step.index]: !isExpanded(step),
    }));
  }

  function toggleAncestorStep(step: PlanStep): void {
    setAncestorOverrides((prev) => ({
      ...prev,
      [step.index]: !(ancestorOverrides[step.index] ?? false),
    }));
  }

  // Ancestor-view mode: a read-only view of a clicked ancestor plan.
  // Shown instead of the active plan until the user clicks "back".
  if (viewingAncestor) {
    return (
      <div className="h-full overflow-y-auto p-3">
        <div className="mb-3 flex items-center gap-2">
          <button
            type="button"
            onClick={() => {
              setViewingAncestor(null);
              setViewingTitle("");
              setViewError(null);
            }}
            className="flex items-center gap-1 rounded-lg border border-border px-2 py-1 text-[0.75em] text-slate-400 hover:text-slate-200"
            title="Back to the active plan"
          >
            <ArrowLeft className="h-3.5 w-3.5" />
            Active plan
          </button>
          <span className="text-[0.72em] text-slate-500">
            viewing ancestor (read-only)
          </span>
        </div>
        <h3 className="mb-3 text-[0.875em] font-semibold text-slate-400">
          {viewingTitle}
        </h3>
        {viewError && (
          <div className="mb-3 rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {viewError}
          </div>
        )}
        <div className="space-y-1">
          {viewingAncestor.steps.map((step) => (
            <PlanStepRow
              key={step.index}
              step={step}
              expanded={ancestorOverrides[step.index] ?? false}
              onToggle={() => toggleAncestorStep(step)}
            />
          ))}
        </div>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto p-3">
      {/* Error banner (shown in both active + ancestor views when a fetch fails). */}
      {viewError && (
        <div className="mb-3 rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {viewError}
        </div>
      )}
      {/* Plan staircase — visualize the plan stack (root → active). Each
          ancestor is an indented, CLICKABLE row (fetches + views it read-only);
          the active plan is highlighted below. Only shown when there's nesting
          (depth > 1) or an active skill overlay. */}
      {(parents.length > 0 || activeSkill) && (
        <div className="mb-3">
          {/* Ancestor plans (root first), each indented one level deeper.
              Clickable — fetches the ancestor plan + shows it read-only. */}
          {parents.map((p, i) => (
            <button
              key={p.id}
              type="button"
              onClick={() => void viewAncestor(p)}
              className="flex w-full items-center gap-1.5 py-0.5 text-left text-[0.72em] text-slate-500 transition-colors hover:text-slate-300"
              style={{ paddingLeft: `${i * 14 + 4}px` }}
              title={`view ancestor plan '${p.title}' (read-only)`}
            >
              <span className="text-slate-600 select-none">↳</span>
              <span className="truncate">{p.title}</span>
            </button>
          ))}
          {/* Active skill overlay badge (when a skill is running on top). */}
          {activeSkill && (
            <div
              className="mt-1 flex items-center gap-1.5 py-0.5 text-[0.72em]"
              style={{ paddingLeft: `${parents.length * 14 + 4}px` }}
            >
              <span className="text-purple-400 select-none">↳</span>
              <span className="rounded bg-purple-950/40 px-1.5 py-0.5 text-purple-300">
                skill: {activeSkill.name}
              </span>
            </div>
          )}
        </div>
      )}

      {/* Title + state */}
      <div className="mb-3">
        <h3 className="text-[0.875em] font-semibold text-cyan-400">{plan.title}</h3>
        <div className="mt-1 flex items-center gap-2 text-[0.75em]">
          <span className={`rounded px-1.5 py-0.5 ${
            state === "executing" ? "bg-green-950/50 text-green-400" :
            state === "reviewing" ? "bg-purple-950/50 text-purple-400" :
            state === "complete" ? "bg-blue-950/50 text-blue-400" :
            state === "subagent" ? "bg-slate-800/50 text-slate-300" :
            "bg-yellow-950/50 text-yellow-400"
          }`}>
            {state}
          </span>
          <span className="text-slate-500">{completed}/{total} steps</span>
        </div>
      </div>

      {/* Goal */}
      {plan.goal && (
        <div className="mb-3 rounded-lg border border-border bg-bg-tertiary p-2">
          <div className="flex items-center gap-1.5 text-[0.75em] font-medium text-slate-400">
            <Target className="h-3.5 w-3.5" />
            Goal
          </div>
          <div className="prose prose-invert prose-sm mt-1 max-w-none prose-pre:m-0 prose-pre:bg-bg-primary">
            <Markdown>{plan.goal}</Markdown>
          </div>
        </div>
      )}

      {/* Progress bar */}
      <div className="mb-3">
        <div className="h-1.5 overflow-hidden rounded-full bg-bg-tertiary">
          <div
            className="h-full rounded-full bg-cyan-500 transition-all"
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>

      {/* Steps — headline rows with collapsible details (the active step
          auto-expands; manual toggles override). */}
      <div className="space-y-1">
        {plan.steps.map((step) => (
          <PlanStepRow
            key={step.index}
            step={step}
            expanded={isExpanded(step)}
            onToggle={() => toggleStep(step)}
          />
        ))}
      </div>
    </div>
  );
}
