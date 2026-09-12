// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useRef, useState, memo } from "react";
import { Shield, ShieldAlert, ShieldCheck, CheckCircle, ChevronUp, ChevronDown, Gauge, GitMerge } from "lucide-react";
import { fmtPct } from "../../lib/format";
import { useAgentStore } from "../../hooks/useAgentStore";
import {
  setSafetyMode,
  getWorkflowState,
  getSettings,
  setModel,
  getContextCaps,
  enterSkill,
  saveSettings,
  getGitBranch,
  getEmbedderStatus,
  onEmbedderStatus,
  errMsg,
} from "../../lib/tauri";
import { endpointForModel } from "../../lib/endpoints";
import type { EndpointInfo } from "../../lib/tauri";
import { SafetyToggleDialog } from "./SafetyToggleDialog";
import { MergeToMainDialog } from "./MergeToMainDialog";
import { ExecutingStepPopup } from "./ExecutingStepPopup";
import type { PlanFile, SafetyMode } from "../../lib/types";

/** Reasoning-effort options for the toolbar dropdown (values pass through to
 *  the request body verbatim; "off" omits the field). Default is "max". */
const REASONING_EFFORTS = ["max", "high", "medium", "low", "minimal", "off"];

/**
 * The reasoning-effort options for the ACTIVE endpoint + model: the model's
 * per-model `reasoning_efforts` list when set (the "off" omits the field and
 * is always available), else the endpoint-level standard list. Empty when
 * the endpoint doesn't support the field (the UI hides the dropdown).
 */
function effortOptionsFor(ep: EndpointInfo | undefined, modelId: string | null): string[] {
  if (!ep || ep.supports_reasoning_effort === false) return [];
  const perModel = (ep.model_configs ?? []).find((mc) => mc.id === modelId);
  const base = perModel && perModel.reasoning_efforts.length > 0
    ? [...perModel.reasoning_efforts]
    : [...REASONING_EFFORTS];
  // "off" is always valid — it omits the field and needs no model support.
  if (!base.includes("off")) base.push("off");
  return base;
}

/**
 * Clamp a requested effort into the target model's `reasoning_efforts`
 * allow-list — the same rule the backend applies in
 * `resolve_reasoning_effort`: an excluded value falls back to the list's
 * first entry (the highest supported); "off" stays "off" (it omits the
 * field and is always valid). Returns the requested value unchanged when
 * the model has no explicit list.
 */
function clampEffortToModel(
  ep: EndpointInfo | undefined,
  modelId: string,
  effort: string,
): string {
  if (effort === "off") return effort;
  const perModel = (ep?.model_configs ?? []).find((mc) => mc.id === modelId);
  const list = perModel?.reasoning_efforts ?? [];
  if (list.length === 0 || list.includes(effort)) return effort;
  return list[0];
}

/**
 * Bottom status bar (self-subscribing). Memoized (mem-perf review HIGH 1):
 * prop-less, so App-level re-renders can't drag it along — it only
 * re-renders when its own store slices change.
 */
export const StatusBar = memo(function StatusBar() {
  const model = useAgentStore((s) => s.model);
  const provider = useAgentStore((s) => s.provider);
  const reasoningEffort = useAgentStore((s) => s.reasoningEffort);
  const setModelStore = useAgentStore((s) => s.setModel);
  const setProviderStore = useAgentStore((s) => s.setProvider);
  const setReasoningEffortStore = useAgentStore((s) => s.setReasoningEffort);
  const setAgentEffort = useAgentStore((s) => s.setAgentEffort);
  const clearAgentEfforts = useAgentStore((s) => s.clearAgentEfforts);
  const gitBranch = useAgentStore((s) => s.gitBranch);
  const planVersion = useAgentStore((s) => s.planVersion);
  const configVersion = useAgentStore((s) => s.configVersion);
  const activeAgent = useAgentStore((s) => s.activeAgent);
  // The ACTIVE agent's effective model: the per-context override resolved for
  // its most recent turn (from AgentInfo.model — the backend reports the
  // resolved model, not the default slot), falling back to the default model.
  const agentModels = useAgentStore((s) => s.agentModels);
  const agentProviders = useAgentStore((s) => s.agentProviders);
  const effectiveModel = activeAgent !== null ? (agentModels[activeAgent] ?? model) : model;
  // Per-agent reasoning-effort records (models are agent-specific, so effort
  // is too — see the effectiveEffort computation after activeEndpoint).
  const agentEfforts = useAgentStore((s) => s.agentEfforts);
  // Per-agent WIRE-reported effective efforts (backlog 51dab4da): the
  // backend's resolution of the model actually serving each agent's current
  // context — authoritative over the toolbar echo / endpoint default.
  const agentWireEfforts = useAgentStore((s) => s.agentWireEfforts);
  // The active agent's workflow phase (per-agent map; falls back to a fetch).
  const workflowState = useAgentStore((s) =>
    activeAgent !== null ? s.workflowStates[activeAgent] ?? null : null,
  );
  const safetyMode = useAgentStore((s) => s.safetyMode);
  const setSafetyModeStore = useAgentStore((s) => s.setSafetyMode);
  const setGitBranch = useAgentStore((s) => s.setGitBranch);
  const [dialogOpen, setDialogOpen] = useState(false);

  // Embedder status badge — shows whether semantic recall is active (green
  // "nomic"/model name), degraded (yellow ⚠), or off (gray "hash"). Fetched
  // on mount + updated via the embedder://status event.
  const [embedderStatus, setEmbedderStatus] = useState<string | Record<string, unknown> | null>(null);
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    // Subscribe BEFORE the initial fetch so no emit is missed in the gap
    // between fetch-read and subscribe-complete.
    onEmbedderStatus((s) => setEmbedderStatus(s)).then((fn) => {
      unlisten = fn;
    });
    (async () => {
      try {
        setEmbedderStatus(await getEmbedderStatus());
      } catch {
        /* ignore — badge is non-critical */
      }
    })();
    return () => {
      unlisten?.();
    };
  }, []);

  // Merge-to-main: confirm dialog + busy + transient result message.
  const [mergeOpen, setMergeOpen] = useState(false);
  const [merging, setMerging] = useState(false);
  const [mergeResult, setMergeResult] = useState<{ ok: boolean; text: string } | null>(null);
  const mergeResultTimer = useRef<number | null>(null);

  // Model-picker dropdown state + the configured endpoints (from endpoints.toml).
  const [modelOpen, setModelOpen] = useState(false);
  const modelRef = useRef<HTMLDivElement>(null);
  const [endpoints, setEndpoints] = useState<EndpointInfo[]>([]);
  // The endpoint serving the active agent's model (models are agent-specific:
  // the active agent may run a model from a different endpoint than the
  // configured default). Falls back to the global default provider when no
  // endpoint lists the model (e.g. before the first settings load).
  // Wire-first endpoint resolution: the backend NAMES the serving endpoint
  // (AgentInfo.provider / ModelChanged.provider — the agent loop reports the
  // client's endpoints.toml name), which is authoritative even when the SAME
  // model id is listed under two endpoints (first-match over the endpoint
  // list cannot disambiguate those — backlog 2980ca67: "model display shows
  // the wrong provider when the name matches in two providers"). Absent
  // (mock-backed agents / before the first load) → resolve by model id,
  // then fall back to the global default provider.
  const wireProvider = activeAgent !== null ? agentProviders[activeAgent] : undefined;
  const activeEndpoint = wireProvider ?? endpointForModel(endpoints, effectiveModel) ?? provider;
  // The ACTIVE agent's reasoning effort (models are agent-specific, so effort
  // is too). Wire-first (backlog 51dab4da): the backend reports the effective
  // effort of the model actually serving the agent's current context
  // (AgentInfo.reasoning_effort / ModelChanged.reasoning_effort — the same
  // resolution the request builder uses: the per-context ModelRef override,
  // else the model's default chain ModelSpec.reasoning_effort → endpoint
  // default → "max", gated + clamped), which is authoritative even when the
  // model was auto-selected / resolved per-context. Absent (mock-backed
  // loops / before the first load) → the per-agent effort last chosen in the
  // toolbar, then its endpoint's configured default. The global store is the
  // fallback while no agent is active.
  const effectiveEffort =
    activeAgent !== null
      ? (agentWireEfforts[activeAgent] ??
        agentEfforts[activeAgent] ??
        effortForEndpoint(endpoints.find((e) => e.name === activeEndpoint)))
      : reasoningEffort;
  // A transient error from a failed set_model call, shown inline next to the
  // model button so a failed switch is visible (not just a console.error).
  const [modelError, setModelError] = useState<string | null>(null);
  const modelErrorTimer = useRef<number | null>(null);

  // Reasoning-effort dropdown state.
  const [effortOpen, setEffortOpen] = useState(false);
  const effortRef = useRef<HTMLDivElement>(null);

  function showModelError(msg: string) {
    setModelError(msg);
    if (modelErrorTimer.current !== null) window.clearTimeout(modelErrorTimer.current);
    modelErrorTimer.current = window.setTimeout(() => setModelError(null), 4000);
  }

  /** Run the merge after the user confirms in the dialog (the approval gate).
   *  Enters the merge_to_main skill on the active agent + sends the skill's
   *  goal as a prompt — the agent then drives the merge itself (stash, commit,
   *  merge, resolve conflicts, delete branch) using its tools. */
  async function handleMergeConfirm() {
    if (activeAgent === null) return;
    setMerging(true);
    try {
      await enterSkill(activeAgent, "merge_to_main");
      setMergeResult({ ok: true, text: "Merge skill started — the agent is driving the merge." });
      // Refresh the status-bar branch (the merge may change it).
      try {
        setGitBranch(await getGitBranch());
      } catch {
        /* non-fatal */
      }
    } catch (e) {
      setMergeResult({ ok: false, text: errMsg(e) });
    } finally {
      setMerging(false);
      setMergeOpen(false);
      if (mergeResultTimer.current !== null) window.clearTimeout(mergeResultTimer.current);
      mergeResultTimer.current = window.setTimeout(() => setMergeResult(null), 6000);
    }
  }

  /** Resolve the display effort for an endpoint (capability flag wins). */
  function effortForEndpoint(ep: EndpointInfo | undefined): string {
    if (!ep || ep.supports_reasoning_effort === false) return "off";
    return ep.reasoning_effort ?? "max";
  }

  /** Re-sync the toolbar labels from the backend's authoritative config. */
  async function resyncFromBackend() {
    try {
      // E5: get_settings is a strict superset of the old get_config. The
      // safety field reads the live runtime value (matches getSafetyMode);
      // resync only fires post-save when disk==runtime, so no on-disk
      // fallback is needed.
      const settings = await getSettings();
      const def = settings.general;
      setEndpoints(settings.endpoints ?? []);
      if (def?.default_model) setModelStore(def.default_model);
      if (def?.default_provider) setProviderStore(def.default_provider);
      // Sync reasoning effort from the default endpoint. Unset → runtime
      // default "max"; unsupported endpoints force "off"; explicit "off"
      // must also win (do not skip falsy).
      const ep = (settings.endpoints ?? []).find((e) => e.name === def?.default_provider);
      if (ep) {
        setReasoningEffortStore(effortForEndpoint(ep));
      }
      // Safety label reflects the live runtime value (matches getSafetyMode).
      if (def?.safety) {
        setSafetyModeStore(def.safety as SafetyMode);
      }
    } catch (e) {
      console.error("failed to re-sync model config:", e);
    }
  }

  // The plan, fetched whenever planVersion bumps (real-time) + on mount + when
  // the active agent changes (so switching agents reloads the plan tab with
  // the newly-active agent's plan).
  const [plan, setPlan] = useState<PlanFile | null>(null);
  // Whether the step-picker dropdown is open.
  const [pickerOpen, setPickerOpen] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);
  const lastFetchedVersion = useRef<number>(-1);
  const lastFetchedAgent = useRef<number | null>(null);

  const isAutonomous = safetyMode === "autonomous";

  // Safety-mode dropdown state.
  const [safetyOpen, setSafetyOpen] = useState(false);
  const safetyRef = useRef<HTMLDivElement>(null);

  async function refreshPlan() {
    if (activeAgent === null) return;
    try {
      const wf = await getWorkflowState(activeAgent);
      setPlan(wf.plan);
      useAgentStore.getState().setWorkflowState(activeAgent, wf.state);
    } catch (e) {
      console.error("failed to get workflow state:", e);
    }
  }

  // Re-fetch the plan whenever planVersion changes (created, step completed,
  // state transitioned) so the dropdown stays in sync.
  useEffect(() => {
    if (planVersion !== lastFetchedVersion.current) {
      lastFetchedVersion.current = planVersion;
      void refreshPlan();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [planVersion]);

  // Re-fetch the plan whenever the active agent changes — each agent owns its
  // own workflow, so switching agents must reload the plan tab.
  useEffect(() => {
    if (activeAgent !== lastFetchedAgent.current) {
      lastFetchedAgent.current = activeAgent;
      lastFetchedVersion.current = -1; // force a fetch even if version is same
      void refreshPlan();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeAgent]);

  // Initial fetch.
  useEffect(() => {
    void refreshPlan();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Close the dropdown on outside click.
  useEffect(() => {
    if (!pickerOpen) return;
    function handleClick(e: MouseEvent) {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [pickerOpen]);

  // Close the safety dropdown on outside click.
  useEffect(() => {
    if (!safetyOpen) return;
    function handleClick(e: MouseEvent) {
      if (safetyRef.current && !safetyRef.current.contains(e.target as Node)) {
        setSafetyOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [safetyOpen]);

  // Close the model dropdown on outside click.
  useEffect(() => {
    if (!modelOpen) return;
    function handleClick(e: MouseEvent) {
      if (modelRef.current && !modelRef.current.contains(e.target as Node)) {
        setModelOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [modelOpen]);

  // Close the reasoning-effort dropdown on outside click.
  useEffect(() => {
    if (!effortOpen) return;
    function handleClick(e: MouseEvent) {
      if (effortRef.current && !effortRef.current.contains(e.target as Node)) {
        setEffortOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [effortOpen]);

  /**
   * Keyboard support for an open dropdown: Escape closes it, ArrowUp/Down
   * move focus between the option buttons (roving focus), and Enter/Space
   * activate the focused option (native button behavior). Shared by the model
   * and reasoning-effort dropdowns.
   */
  function onDropdownKeyDown(e: React.KeyboardEvent, close: () => void) {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const buttons = Array.from(
      e.currentTarget.querySelectorAll<HTMLButtonElement>("[role='menuitem']"),
    );
    if (buttons.length === 0) return;
    const current = document.activeElement as HTMLElement | null;
    const idx = current ? buttons.indexOf(current as HTMLButtonElement) : -1;
    const next =
      e.key === "ArrowDown"
        ? (idx + 1) % buttons.length
        : (idx - 1 + buttons.length) % buttons.length;
    buttons[next]?.focus();
  }

  // Load the configured endpoints (and their models) once on mount so the
  // model picker can list every available model.
  useEffect(() => {
    void (async () => {
      try {
        // E5: get_settings is a strict superset of get_config (endpoints
        // field is identical — same endpoint_wire type + source).
        const settings = await getSettings();
        setEndpoints(settings.endpoints ?? []);
      } catch (e) {
        console.error("failed to load endpoints:", e);
      }
    })();
  }, []);

  // Re-sync the toolbar labels + endpoint list when the global config changes
  // (the Settings → Endpoints tab save bumps `configVersion`). Mount is
  // covered by the effect above; this handles subsequent saves while the app
  // is running.
  useEffect(() => {
    if (configVersion === 0) return; // initial value — mount effect handles it
    void resyncFromBackend();
    // A config save rebuilt every agent's provider with the new default
    // effort — drop the stale per-agent effort records so the dropdown
    // falls back to the fresh endpoint defaults. (resyncFromBackend itself
    // also runs on switch-failure recovery, where the records must survive.)
    clearAgentEfforts();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [configVersion]);

  /**
   * Switch to a model: call the backend to rebuild + swap the provider for
   * the ACTIVE agent only (models are agent-specific — other agents keep
   * their models), then stamp the store so the label reflects the new model
   * immediately. The `model_changed` event the backend emits for that agent
   * lands moments later with the same value (idempotent).
   *
   * Effort rules:
   * - target unsupported → force "off" (field omitted; never carry "max")
   * - leaving unsupported for a supported target → adopt that endpoint's
   *   configured default (so "off" does not stick forever)
   * - supported → supported → keep the current toolbar effort (live override)
   *
   * The label is clamped into the TARGET model's `reasoning_efforts`
   * allow-list (same rule as the backend's resolve_reasoning_effort) so the
   * toolbar can't desync from what requests actually carry.
   */
  async function selectModel(endpointName: string, modelId: string) {
    const ep = endpoints.find((e) => e.name === endpointName);
    const prevSupports =
      endpoints.find((e) => e.name === activeEndpoint)?.supports_reasoning_effort !==
      false;
    const targetSupports = ep?.supports_reasoning_effort !== false;
    const effort = !targetSupports
      ? "off"
      : !prevSupports
        ? effortForEndpoint(ep)
        : effectiveEffort;
    const clampedEffort = clampEffortToModel(ep, modelId, effort);
    try {
      await setModel(activeAgent, endpointName, modelId, clampedEffort);
      if (activeAgent !== null) {
        // Stamp the ACTIVE agent's model optimistically (the backend's
        // per-agent `model_changed` event confirms it moments later). The
        // global model/provider stores are NOT touched — they stay the
        // configured default, and every agent keeps its own agentModels
        // entry.
        useAgentStore.setState((s) => ({
          agentModels: { ...s.agentModels, [activeAgent]: modelId },
        }));
        // The agent's provider was rebuilt with clampedEffort — record it so
        // the effort dropdown shows this agent's actual effort.
        setAgentEffort(activeAgent, clampedEffort);
      } else {
        // No active agent yet (startup before the first listAgents): the
        // backend took the legacy global branch (factory + every loop), so
        // update the global stores too — the label reads them while
        // activeAgent is null and must not stay stale after the switch.
        setModelStore(modelId);
        setProviderStore(endpointName);
      }
      setReasoningEffortStore(clampedEffort);
      // The swap rebuilt the loop's ContextManager with the new provider's
      // max context — re-seed the ctx-bar max so it reflects the new window
      // immediately (non-fatal: it re-syncs on the next turn's ContextUsage).
      void getContextCaps()
        .then((caps) => useAgentStore.getState().seedContextCaps(caps))
        .catch((e) => console.error("failed to refresh context caps:", e));
      // Close the effort menu so it doesn't remount open after switching
      // away from (or back to) a supported endpoint (review M1).
      setEffortOpen(false);
      setModelError(null);
    } catch (e) {
      // Surface the failure inline (auto-clears) and re-sync the label from
      // the backend so it can't silently desync from the real endpoint.
      showModelError(`Failed to switch model: ${e}`);
      await resyncFromBackend();
    }
    setModelOpen(false);
  }

  /**
   * Change the reasoning effort: rebuild the provider with the ACTIVE
   * agent's current model but the new effort. The backend maps "off" to
   * omitting the field from requests entirely, and ignores the value when
   * the endpoint has `supports_reasoning_effort = false`.
   */
  async function selectReasoningEffort(effort: string) {
    try {
      await setModel(activeAgent, activeEndpoint, effectiveModel, effort);
      setReasoningEffortStore(effort);
      // Record the ACTIVE agent's effort (models are agent-specific, so each
      // agent's provider carries its own effort) — the dropdown label reads
      // it back when this agent is active again.
      if (activeAgent !== null) setAgentEffort(activeAgent, effort);
      // Same provider rebuild → same ContextManager rebuild: re-seed the
      // ctx-bar max (non-fatal — re-syncs on the next turn's ContextUsage).
      void getContextCaps()
        .then((caps) => useAgentStore.getState().seedContextCaps(caps))
        .catch((e) => console.error("failed to refresh context caps:", e));
      setModelError(null);
    } catch (e) {
      showModelError(`Failed to set reasoning effort: ${e}`);
      await resyncFromBackend();
    }
    setEffortOpen(false);
  }

  /** Whether the ACTIVE agent's endpoint accepts the reasoning_effort
   *  request field. */
  const activeSupportsEffort =
    endpoints.find((e) => e.name === activeEndpoint)?.supports_reasoning_effort !== false;

  /**
   * Switch to a safety mode. "autonomous" (Unsafe) opens the warning dialog
   * first; the other two switch immediately.
   */
  async function selectSafetyMode(mode: SafetyMode) {
    if (mode === "autonomous") {
      // Dangerous direction — show the warning dialog first.
      setDialogOpen(true);
      setSafetyOpen(false);
      return;
    }
    await setSafetyMode(mode);
    setSafetyModeStore(mode);
    // Persist default so restarts match the StatusBar choice.
    void saveSettings({ safety: mode }).catch((e) =>
      console.error("failed to persist safety mode:", e),
    );
    setSafetyOpen(false);
  }

  async function confirmAutonomous() {
    await setSafetyMode("autonomous");
    setSafetyModeStore("autonomous");
    void saveSettings({ safety: "autonomous" }).catch((e) =>
      console.error("failed to persist safety mode:", e),
    );
    setDialogOpen(false);
  }

  const hasPlan = plan !== null && plan.steps.length > 0;

  // Progress label: "Executing x/y" while a plan is in flight, "Complete"
  // when every step is done, otherwise the capitalized state name. All
  // states (Planning, Executing, Reviewing, Complete, Skill, Subagent)
  // render with consistent capitalization so the label always reflects the
  // agent's actual phase — Subagent being a parented sub-agent's role state
  // (its plan stack is the parent's, shown read-only).
  const total = plan?.steps.length ?? 0;
  const completed = plan?.steps.filter((s) => s.done).length ?? 0;
  const stateLabel = (() => {
    const st = (workflowState ?? "").toLowerCase();
    if (st === "reviewing") return "Reviewing";
    if (st === "complete" || (total > 0 && completed >= total)) return "Complete";
    if (st === "executing" && total > 0) {
      // x = the step currently being worked on = completed + 1, capped at total.
      const x = Math.min(completed + 1, total);
      // Compact "Executing x/y" — the current step's headline is shown in
      // the popout dropdown (below), so it's not duplicated here to save room.
      return `Executing ${x}/${total}`;
    }
    if (st === "executing") return "Executing";
    if (st === "planning") return "Planning";
    if (st === "skill") return "Skill";
    if (st === "subagent") return "Subagent";
    // Unknown value — fall back to the raw state, capitalized.
    return workflowState
      ? workflowState.charAt(0).toUpperCase() + workflowState.slice(1)
      : "";
  })();

  return (
    <>
      <div className="flex items-center gap-3 border-t border-border bg-bg-secondary px-4 py-1 text-xs text-slate-400">
        {/* Model + provider — clickable to open the model picker. */}
        <div className="relative" ref={modelRef}>
          <button
            onClick={() => setModelOpen((o) => !o)}
            className="flex items-center gap-1 rounded px-1 py-0.5 transition-colors hover:bg-bg-tertiary"
            title="Switch model"
            aria-haspopup="menu"
            aria-expanded={modelOpen}
          >
            {effectiveModel && (
              <span
                className="max-w-44 truncate font-medium text-cyan-400"
                title={`${effectiveModel} — The active agent's effective model — reflects per-context overrides (workflow state / skill / subagent) when active; the picker below switches the active agent's model (models are agent-specific — other agents keep theirs).`}
              >
                {effectiveModel}
              </span>
            )}
            {activeEndpoint && (
              <>
                <span>·</span>
                <span
                  className="max-w-32 truncate text-blue-400"
                  title={activeEndpoint}
                >
                  {activeEndpoint}
                </span>
              </>
            )}
            <ChevronDown
              className={`h-3 w-3 text-slate-500 transition-transform ${
                modelOpen ? "rotate-180" : ""
              }`}
            />
          </button>
          {modelError && (
            <span className="absolute bottom-full left-0 z-50 mb-1 max-w-xs truncate rounded border border-red-600/40 bg-red-950/20 px-2 py-1 text-[0.7rem] text-red-400" title={modelError}>
              {modelError}
            </span>
          )}

          {/* Upward dropdown of every configured model. */}
          {modelOpen && (
            <div
              className="absolute bottom-full left-0 z-50 mb-1 max-h-80 w-72 overflow-y-auto rounded-lg border border-border bg-bg-secondary shadow-2xl"
              role="menu"
              aria-label="Switch model"
              onKeyDown={(e) => onDropdownKeyDown(e, () => setModelOpen(false))}
            >
              <div className="border-b border-border px-3 py-2 text-xs font-medium text-slate-300">
                Switch model
              </div>
              <div className="py-1">
                {endpoints.length === 0 && (
                  <div className="px-3 py-1.5 text-xs text-slate-500">
                    No endpoints configured
                  </div>
                )}
                {endpoints.map((ep) => (
                  <div key={ep.name}>
                    <div className="px-3 pb-0.5 pt-1.5 text-[0.7rem] font-medium uppercase tracking-wide text-slate-500">
                      {ep.name}
                      <span className="ml-1.5 normal-case text-slate-600">
                        {ep.kind}
                      </span>
                    </div>
                    {ep.models.map((m) => (
                      <button
                        key={`${ep.name}/${m}`}
                        role="menuitem"
                        onClick={() => selectModel(ep.name, m)}
                        className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs transition-colors hover:bg-bg-tertiary ${
                          effectiveModel === m && activeEndpoint === ep.name
                            ? "bg-bg-tertiary"
                            : ""
                        }`}
                      >
                        <span className="text-slate-200">{m}</span>
                        {effectiveModel === m && activeEndpoint === ep.name && (
                          <CheckCircle className="ml-auto h-3.5 w-3.5 shrink-0 text-cyan-400" />
                        )}
                      </button>
                    ))}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        {/* Reasoning effort — right next to the model picker. Hidden when the
            active endpoint does not support the parameter (field is omitted). */}
        {activeSupportsEffort ? (
          <div className="relative" ref={effortRef}>
            <button
              onClick={() => setEffortOpen((o) => !o)}
              className="flex items-center gap-1 rounded px-1 py-0.5 transition-colors hover:bg-bg-tertiary"
              title="Reasoning effort — click to change"
              aria-haspopup="menu"
              aria-expanded={effortOpen}
            >
              <Gauge className="h-3 w-3 text-slate-500" />
              <span className="text-cyan-400">{effectiveEffort}</span>
              <ChevronDown
                className={`h-3 w-3 text-slate-500 transition-transform ${
                  effortOpen ? "rotate-180" : ""
                }`}
              />
            </button>

            {/* Upward dropdown of reasoning-effort options. */}
            {effortOpen && (
              <div
                className="absolute bottom-full left-0 z-50 mb-1 w-44 rounded-lg border border-border bg-bg-secondary shadow-2xl"
                role="menu"
                aria-label="Reasoning effort"
                onKeyDown={(e) => onDropdownKeyDown(e, () => setEffortOpen(false))}
              >
                <div className="border-b border-border px-3 py-2 text-xs font-medium text-slate-300">
                  Reasoning effort
                </div>
                <div className="py-1">
                  {effortOptionsFor(
                    endpoints.find((e) => e.name === activeEndpoint),
                    effectiveModel,
                  ).map((effort) => (
                    <button
                      key={effort}
                      role="menuitem"
                      onClick={() => selectReasoningEffort(effort)}
                      className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs transition-colors hover:bg-bg-tertiary ${
                        effectiveEffort === effort ? "bg-bg-tertiary" : ""
                      }`}
                    >
                      <span className="text-slate-200">{effort}</span>
                      {effort === "off" && (
                        <span className="text-[0.7rem] text-slate-500">
                          (omit field)
                        </span>
                      )}
                      {effectiveEffort === effort && (
                        <CheckCircle className="ml-auto h-3.5 w-3.5 shrink-0 text-cyan-400" />
                      )}
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
        ) : (
          <span
            className="flex items-center gap-1 px-1 text-slate-500"
            title="This endpoint's models do not accept reasoning_effort — field omitted from requests"
          >
            <Gauge className="h-3 w-3" />
            <span className="text-[0.7rem]">n/a</span>
          </span>
        )}
        <span>│</span>
        {/* Workflow state — clickable to open the step picker. */}
        <div className="relative" ref={pickerRef}>
          <button
            onClick={() => hasPlan && setPickerOpen((o) => !o)}
            disabled={!hasPlan}
            className={`flex items-center gap-1 rounded px-1 py-0.5 transition-colors ${
              hasPlan
                ? "hover:bg-bg-tertiary hover:text-green-300"
                : "cursor-default"
            }`}
            title={
              hasPlan
                ? "Click to view the current step"
                : "No plan yet"
            }
          >
            {stateLabel && (
              <span
                className="max-w-24 truncate text-green-400"
                title={stateLabel}
              >
                {stateLabel}
              </span>
            )}
            {hasPlan && (
              <ChevronUp
                className={`h-3 w-3 text-slate-500 transition-transform ${
                  pickerOpen ? "" : "rotate-180"
                }`}
              />
            )}
          </button>

          {/* Upward popup: the current step's headline only (single
              truncated line — never the recipe body; the full step list
              lives in the plan window). */}
          {pickerOpen && hasPlan && <ExecutingStepPopup plan={plan} />}
        </div>
        <span>│</span>
        {gitBranch && (
          <span
            className="max-w-32 truncate text-purple-400"
            title={gitBranch}
          >
            {gitBranch}
          </span>
        )}
        {/* Merge-to-main: shown when the workflow is Complete or Planning and
            we're on a non-main branch. Gated by the MergeToMainDialog
            confirmation. Enters the merge_to_main skill — the agent drives
            the merge itself (stash, commit, merge, resolve conflicts, delete
            branch). */}
        {((workflowState === "complete" || workflowState === "planning") && gitBranch && gitBranch !== "main" && gitBranch !== "no-branch") && (
          <button
            onClick={() => setMergeOpen(true)}
            className="flex items-center gap-1 rounded px-2 py-0.5 text-xs font-medium text-cyan-400 transition-colors hover:bg-bg-tertiary whitespace-nowrap"
            title={`Merge '${gitBranch}' into main (one atomic, confirmation-gated action)`}
          >
            <GitMerge className="h-3.5 w-3.5" />
            Merge to main
          </button>
        )}
        {mergeResult && (
          <span
            className={`max-w-md truncate text-xs ${mergeResult.ok ? "text-green-400" : "text-red-400"}`}
            title={mergeResult.text}
          >
            {mergeResult.text}
          </span>
        )}
        {/* Embedder status badge — green ◆ when semantic recall is active,
            amber ⚠ when degraded (fallback/failed), a progress bar when a
            model is downloading, gray when no model is configured (keyword-
            only). Clicking opens Settings → Embeddings. */}
        {(() => {
          // The status is a string for unit variants, or an object for the
          // downloading variant: {"downloading": {model, progress}}.
          const s = embedderStatus;
          if (s && typeof s === "object" && "downloading" in s) {
            const d = (s as { downloading: { model: string; progress: number } }).downloading;
            return (
              <button
                onClick={() => useAgentStore.getState().openSettings("embeddings")}
                className="flex items-center gap-1.5 rounded px-1.5 py-0.5 text-xs text-cyan-400 transition-colors hover:bg-bg-tertiary"
                title={`Downloading ${d.model}… ${fmtPct(d.progress * 100)}%`}
              >
                <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-cyan-400" />
                {fmtPct(d.progress * 100)}%
              </button>
            );
          }
          if (s === "ready") {
            return (
              <span
                className="text-xs text-green-500/70"
                title="Semantic memory recall is active"
              >
                ◆
              </span>
            );
          }
          if (s && s !== "ready") {
            return (
              <button
                onClick={() => useAgentStore.getState().openSettings("embeddings")}
                className="flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-amber-400 transition-colors hover:bg-bg-tertiary"
                title="Embedding service issue — click to configure"
              >
                ⚠
              </button>
            );
          }
          return null;
        })()}
        {/* Safety-mode dropdown — pushed to the right */}
        <div className="relative ml-auto" ref={safetyRef}>
          <button
            onClick={() => setSafetyOpen((o) => !o)}
            className={`flex items-center gap-1.5 whitespace-nowrap rounded px-2 py-0.5 text-xs font-medium transition-colors ${
              safetyMode === "autonomous"
                ? "bg-red-600/20 text-red-400 hover:bg-red-600/30"
                : safetyMode === "auto-approve-project"
                  ? "bg-amber-600/20 text-amber-400 hover:bg-amber-600/30"
                  : "text-slate-400 hover:bg-bg-tertiary hover:text-slate-200"
            }`}
            title="Safety mode — click to change"
          >
            {safetyMode === "autonomous" ? (
              <ShieldAlert className="h-3.5 w-3.5" />
            ) : safetyMode === "auto-approve-project" ? (
              <ShieldCheck className="h-3.5 w-3.5" />
            ) : (
              <Shield className="h-3.5 w-3.5" />
            )}
            {safetyMode === "autonomous"
              ? "Unsafe"
              : safetyMode === "auto-approve-project"
                ? "Auto Project"
                : "Approve Each"}
            <ChevronUp
              className={`h-3 w-3 text-slate-500 transition-transform ${
                safetyOpen ? "" : "rotate-180"
              }`}
            />
          </button>

          {/* Upward dropdown of safety modes. */}
          {safetyOpen && (
            <div className="absolute bottom-full right-0 z-50 mb-1 w-56 rounded-lg border border-border bg-bg-secondary shadow-2xl">
              <div className="border-b border-border px-3 py-2 text-xs font-medium text-slate-300">
                Safety mode
              </div>
              <div className="py-1">
                {([
                  {
                    mode: "approve-each-action" as SafetyMode,
                    label: "Approve Each",
                    desc: "Prompt before every mutation",
                    icon: <Shield className="h-3.5 w-3.5 shrink-0 text-slate-400" />,
                  },
                  {
                    mode: "auto-approve-project" as SafetyMode,
                    label: "Auto Approve Project",
                    desc: "Auto-approve project-scoped calls",
                    icon: <ShieldCheck className="h-3.5 w-3.5 shrink-0 text-amber-400" />,
                  },
                  {
                    mode: "autonomous" as SafetyMode,
                    label: "Unsafe",
                    desc: "No approval prompts (dangerous)",
                    icon: <ShieldAlert className="h-3.5 w-3.5 shrink-0 text-red-400" />,
                  },
                ]).map((opt) => (
                  <button
                    key={opt.mode}
                    onClick={() => selectSafetyMode(opt.mode)}
                    className={`flex w-full items-start gap-2 px-3 py-1.5 text-left text-xs transition-colors hover:bg-bg-tertiary ${
                      safetyMode === opt.mode ? "bg-bg-tertiary" : ""
                    }`}
                  >
                    {opt.icon}
                    <div className="flex flex-col">
                      <span className="font-medium text-slate-200">{opt.label}</span>
                      <span className="text-[0.7rem] text-slate-500">{opt.desc}</span>
                    </div>
                    {safetyMode === opt.mode && (
                      <CheckCircle className="ml-auto h-3.5 w-3.5 shrink-0 text-cyan-400" />
                    )}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>
      </div>
      <SafetyToggleDialog
        open={dialogOpen}
        onConfirm={confirmAutonomous}
        onCancel={() => setDialogOpen(false)}
      />
      <MergeToMainDialog
        open={mergeOpen}
        sourceBranch={gitBranch ?? ""}
        merging={merging}
        onConfirm={handleMergeConfirm}
        onCancel={() => { if (!merging) setMergeOpen(false); }}
      />
    </>
  );
});
