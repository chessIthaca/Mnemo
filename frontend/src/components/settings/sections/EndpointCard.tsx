// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useMemo, useRef, useState } from "react";
import {
  ChevronDown,
  Eye,
  EyeOff,
  KeyRound,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { listModels, errMsg } from "../../../lib/tauri";
import type { VisionModelInfo } from "../../../lib/tauri";
import { fmtTokens } from "../../../lib/format";
import type { EndpointEditable } from "../../../lib/tauri";
import {
  KIND_OPTIONS,
  REASONING_EFFORTS,
  REASONING_EFFORT_DEFAULT,
  capsAutofillPatch,
  type DiscoveredCaps,
  discoveredCapsById,
  effectiveCaps,
  effortFromSelectValue,
  effortToSelectValue,
  modelConfigFor,
  parseEffortsList,
  parsePositiveIntInput,
  upsertModelConfig,
} from "../types";
import { ModelPickerDropdown } from "./ModelPickerDropdown";

/**
 * One endpoint card. Parent owns the list; this calls up via `on*` props.
 * Renaming re-keys the API-key map via `onNameChange` before the name patch.
 */
export function EndpointCard({
  endpoint,
  apiKey,
  isDefault,
  defaultModel,
  open,
  onToggle,
  onEndpointChange,
  onKeyChange,
  onModelChange,
  onAddModel,
  onPickModel,
  onDeleteModel,
  onDelete,
  onSetDefault,
  onDefaultModel,
  onNameChange,
}: {
  endpoint: EndpointEditable;
  apiKey: string;
  isDefault: boolean;
  defaultModel: string | null;
  open: boolean;
  onToggle: () => void;
  onEndpointChange: (patch: Partial<EndpointEditable>) => void;
  onKeyChange: (v: string) => void;
  onModelChange: (modelIndex: number, value: string) => void;
  onAddModel: () => void;
  onPickModel: (value: string) => void;
  onDeleteModel: (modelIndex: number) => void;
  onDelete: () => void;
  onSetDefault: () => void;
  onDefaultModel: (model: string) => void;
  onNameChange: (newName: string) => void;
}) {
  const [showKey, setShowKey] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);
  const fetchReqId = useRef(0);
  const [pickerOpen, setPickerOpen] = useState<number | "__add__" | null>(null);
  const [modelsCache, setModelsCache] = useState<string[]>([]);
  const [modelsCacheKey, setModelsCacheKey] = useState<string>("");
  // Token caps reported by the provider's /models endpoint, keyed by model
  // id (populated by the same fetch that fills modelsCache). Empty when the
  // provider exposes no cap fields — discovery is best-effort.
  const [capsById, setCapsById] = useState<Record<string, DiscoveredCaps>>({});
  // The model the cap hints/auto-fill refer to: the last id picked from the
  // server picker, else the first configured model row.
  const [lastPicked, setLastPicked] = useState<string | null>(null);
  // Guards auto-fill to once per (reference model + discovered caps) so a
  // user clearing a field afterwards keeps it cleared (no refill loop).
  const filledForRef = useRef<string | null>(null);
  const [fetchState, setFetchState] = useState<
    | { status: "idle" }
    | { status: "loading" }
    | { status: "ready" }
    | { status: "error"; message: string }
  >({ status: "idle" });
  const [pickerQuery, setPickerQuery] = useState("");
  const [testState, setTestState] = useState<
    | { status: "idle" }
    | { status: "loading" }
    | { status: "ok"; count: number }
    | { status: "error"; message: string }
  >({ status: "idle" });

  // Reset transient per-card state when the card collapses: a revealed key
  // shouldn't linger, and an in-flight model picker shouldn't be able to
  // commit stale query text after its row unmounts.
  useEffect(() => {
    if (!open) {
      setShowKey(false);
      setPickerOpen(null);
      setPickerQuery("");
    }
  }, [open]);

  // The model the cap hints refer to: the last id picked from the server
  // picker, else the first configured model row. Null when the endpoint has
  // no models at all.
  const capsRefModel = lastPicked ?? endpoint.models[0] ?? null;
  const refCaps = capsRefModel ? capsById[capsRefModel] : undefined;

  // The effective caps for the reference model: its per-model override, else
  // the endpoint-level value. The conflict hints below compare THIS against
  // the provider-reported values.
  const { ctx: effCapsCtx, out: effCapsOut } = capsRefModel
    ? effectiveCaps(endpoint, capsRefModel)
    : { ctx: null, out: null };

  // Auto-fill empty cap fields with the provider-reported values for the
  // reference model, once per (model + caps) so manually clearing a field
  // afterwards keeps it cleared. The value lands in the PER-MODEL entry
  // (each model keeps its own caps — the endpoint-level fields stay the
  // manual default). Fires only when the effective value is unset — an
  // explicit value is never silently overwritten (that path is the Apply
  // button on the conflict warning instead).
  useEffect(() => {
    if (!capsRefModel || !refCaps) return;
    const guardKey = `${capsRefModel}\n${refCaps.ctx ?? "-"}\n${refCaps.out ?? "-"}`;
    if (filledForRef.current === guardKey) return;
    const patch = capsAutofillPatch(endpoint, capsRefModel, refCaps);
    if (patch) {
      filledForRef.current = guardKey;
      onEndpointChange(patch);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- deliberate: guarded by filledForRef, runs once per (model, caps)
  }, [capsRefModel, refCaps, endpoint]);

  async function fetchServerModels(force = false) {
    const key = `${endpoint.base_url}\n${apiKey}\n${endpoint.kind}`;
    if (!force && key === modelsCacheKey && fetchState.status === "ready") {
      return;
    }
    if (!endpoint.base_url.trim()) {
      setFetchState({ status: "error", message: "Enter a base URL first." });
      return;
    }
    const reqId = ++fetchReqId.current;
    setFetchState({ status: "loading" });
    try {
      const list = await listModels(endpoint.name, endpoint.base_url, apiKey, endpoint.kind);
      if (reqId !== fetchReqId.current) return;
      applyServerModels(list);
      setModelsCacheKey(key);
      setFetchState({ status: "ready" });
    } catch (e) {
      if (reqId !== fetchReqId.current) return;
      setModelsCache([]);
      setCapsById({});
      setModelsCacheKey(key);
      setFetchState({ status: "error", message: errMsg(e) });
    }
  }

  async function testConnection() {
    if (!endpoint.base_url.trim()) {
      setTestState({ status: "error", message: "Enter a base URL first." });
      return;
    }
    setTestState({ status: "loading" });
    try {
      const list = await listModels(endpoint.name, endpoint.base_url, apiKey, endpoint.kind);
      applyServerModels(list);
      setModelsCacheKey(`${endpoint.base_url}\n${apiKey}\n${endpoint.kind}`);
      setFetchState({ status: "ready" });
      setTestState({ status: "ok", count: list.length });
    } catch (e) {
      // Mirror fetchServerModels' error path: drop the discovered caps and
      // invalidate the cache key so stale hints can't linger under a URL
      // that failed to respond, and the next fetch re-probes instead of
      // trusting the previous URL's ready cache.
      setCapsById({});
      setModelsCacheKey("");
      setTestState({ status: "error", message: errMsg(e) });
    }
  }

  /** Split a fetched /models list into the picker's id cache and the
   *  per-model discovered caps map. Called by every successful fetch so the
   *  two views of the same response can't drift. */
  function applyServerModels(list: VisionModelInfo[]) {
    setModelsCache(list.map((m) => m.id));
    setCapsById(discoveredCapsById(list));
  }

  function closePicker(commit: boolean) {
    setPickerOpen((cur) => {
      if (cur !== null && cur !== "__add__" && commit) {
        const mi = cur;
        const m = endpoint.models[mi];
        if (pickerQuery !== m) {
          if (pickerQuery.trim() || !m) {
            onModelChange(mi, pickerQuery);
          }
        }
      }
      return null;
    });
  }

  function openRowPicker(mi: number) {
    setPickerOpen(mi);
    setPickerQuery(endpoint.models[mi] ?? "");
    void fetchServerModels();
  }

  function openAddPicker() {
    setPickerOpen("__add__");
    setPickerQuery("");
    void fetchServerModels();
  }

  useEffect(() => {
    if (pickerOpen === null) return;
    function handleClick(e: MouseEvent) {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        closePicker(true);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pickerOpen, pickerQuery]);

  const filteredModels = useMemo(() => {
    const q = pickerQuery.trim().toLowerCase();
    if (!q) return modelsCache;
    return modelsCache.filter((m) => m.toLowerCase().includes(q));
  }, [modelsCache, pickerQuery]);

  const nameId = `ep-name-${endpoint.name || "new"}`;
  const kindId = `ep-kind-${endpoint.name || "new"}`;
  const urlId = `ep-url-${endpoint.name || "new"}`;
  const keyId = `ep-key-${endpoint.name || "new"}`;
  const effortId = `ep-effort-${endpoint.name || "new"}`;
  const wsId = `ep-wsid-${endpoint.name || "new"}`;
  const maxCtxId = `ep-maxctx-${endpoint.name || "new"}`;
  const maxOutId = `ep-maxout-${endpoint.name || "new"}`;

  return (
    <div
      className={`space-y-3 rounded-lg border px-3 py-3 ${
        isDefault
          ? "border-[color:var(--accent-color)]/50 bg-[color:var(--accent-color)]/5"
          : "border-border bg-bg-primary"
      }`}
    >
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          aria-label={open ? "Collapse endpoint" : "Expand endpoint"}
          title={open ? "Collapse endpoint" : "Expand endpoint"}
          className="shrink-0 rounded p-0.5 text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--text-primary)]"
        >
          <ChevronDown
            className={`h-4 w-4 transition-transform duration-150 ${open ? "" : "-rotate-90"}`}
          />
        </button>
        <KeyRound
          className={`h-4 w-4 shrink-0 ${
            isDefault
              ? "text-[color:var(--accent-color)]"
              : "text-[color:var(--text-muted)]"
          }`}
        />
        <label className="sr-only" htmlFor={nameId}>
          Endpoint name
        </label>
        <input
          id={nameId}
          value={endpoint.name}
          onChange={(e) => onNameChange(e.target.value)}
          placeholder="endpoint name"
          spellCheck={false}
          className="min-w-0 flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1 text-sm font-medium text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
        {isDefault && (
          <span className="shrink-0 rounded-full bg-[color:var(--accent-color)]/20 px-2 py-0.5 text-[0.65rem] font-medium uppercase tracking-wide text-[color:var(--accent-color)]">
            Default
          </span>
        )}
        {isDefault && defaultModel && (
          <span
            title={defaultModel}
            className="max-w-40 shrink-0 truncate rounded-full bg-[color:var(--accent-color)]/10 px-2 py-0.5 text-[0.65rem] text-[color:var(--accent-color)]"
          >
            ★ {defaultModel}
          </span>
        )}
        {!open && !(isDefault && defaultModel) && (
          <span className="shrink-0 text-[0.7rem] text-[color:var(--text-muted)]">
            {endpoint.models.length} model{endpoint.models.length === 1 ? "" : "s"}
          </span>
        )}
        <button
          type="button"
          onClick={onDelete}
          title="Delete endpoint"
          aria-label="Delete endpoint"
          className="shrink-0 rounded p-1 text-[color:var(--text-muted)] transition-colors hover:bg-red-500/10 hover:text-red-400"
        >
          <Trash2 className="h-4 w-4" />
        </button>
      </div>

      {open && (
        <div className="flex items-center gap-2 rounded bg-bg-secondary px-2 py-1.5">
          <label className="sr-only" htmlFor={kindId}>
            Provider kind
          </label>
          <select
            id={kindId}
            value={endpoint.kind}
            onChange={(e) => onEndpointChange({ kind: e.target.value })}
            title="Provider kind"
            className="shrink-0 rounded border border-border bg-bg-primary px-1.5 py-1 font-mono text-[0.7rem] uppercase text-[color:var(--text-muted)] focus:border-[color:var(--accent-color)] focus:outline-none"
          >
            {KIND_OPTIONS.map((k) => (
              <option key={k} value={k}>
                {k}
              </option>
            ))}
          </select>
          <label className="sr-only" htmlFor={urlId}>
            Base URL
          </label>
          <input
            id={urlId}
            value={endpoint.base_url}
            onChange={(e) => onEndpointChange({ base_url: e.target.value })}
            placeholder="https://api.example.com/v1/ (trailing / auto-added)"
            spellCheck={false}
            className="min-w-0 flex-1 rounded border border-transparent bg-transparent px-1 py-1 text-[0.7rem] text-[color:var(--text-muted)] focus:border-[color:var(--accent-color)] focus:bg-bg-primary focus:outline-none"
          />
          <button
            type="button"
            onClick={onToggle}
            aria-expanded={open}
            title={open ? "Collapse endpoint" : "Expand endpoint"}
            className="shrink-0 rounded px-1.5 py-0.5 text-[0.7rem] text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--text-primary)]"
          >
            {endpoint.models.length} model{endpoint.models.length === 1 ? "" : "s"}
          </button>
        </div>
      )}

      {open && (
        <>
          <div className="flex items-center gap-2">
            <label
              className="w-16 shrink-0 text-xs text-[color:var(--text-muted)]"
              htmlFor={keyId}
            >
              API key
            </label>
            <input
              id={keyId}
              type={showKey ? "text" : "password"}
              value={apiKey}
              onChange={(e) => onKeyChange(e.target.value)}
              placeholder={showKey ? "enter key…" : "••••••••••••"}
              spellCheck={false}
              autoComplete="off"
              className="flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
            />
            <button
              type="button"
              onClick={() => setShowKey((s) => !s)}
              title={showKey ? "Hide key" : "Show key"}
              aria-label={showKey ? "Hide API key" : "Show API key"}
              className="shrink-0 rounded p-1.5 text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--text-primary)]"
            >
              {showKey ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
            </button>
          </div>

          <div className="grid grid-cols-2 gap-2">
            {endpoint.kind !== "anthropic" && (
              <label className="flex items-center gap-2 text-xs text-[color:var(--text-muted)]">
                <input
                  type="checkbox"
                  checked={endpoint.supports_reasoning_effort}
                  onChange={(e) =>
                    onEndpointChange({ supports_reasoning_effort: e.target.checked })
                  }
                  className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
                />
                Supports reasoning effort
              </label>
            )}
            <label
              className="flex items-center gap-2 text-xs text-[color:var(--text-muted)]"
              title="Endpoint-level default. Per-model Vision checkboxes (under each model row) override this — e.g. a text-only GLM and a vision-capable GLM flash on one Ollama endpoint."
            >
              <input
                type="checkbox"
                checked={endpoint.multimodal}
                onChange={(e) => onEndpointChange({ multimodal: e.target.checked })}
                className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
              />
              Multimodal (images)
            </label>
          </div>

          {endpoint.kind !== "anthropic" && (
            <div className="flex items-center gap-2">
              <label
                className="shrink-0 text-xs text-[color:var(--text-muted)]"
                htmlFor={effortId}
              >
                Effort
              </label>
              <select
                id={effortId}
                value={effortToSelectValue(endpoint.reasoning_effort)}
                onChange={(e) =>
                  onEndpointChange({
                    reasoning_effort: effortFromSelectValue(e.target.value),
                  })
                }
                disabled={!endpoint.supports_reasoning_effort}
                title={
                  endpoint.supports_reasoning_effort
                    ? "Default reasoning effort for this endpoint"
                    : "Disabled — models at this endpoint do not accept reasoning_effort"
                }
                className="flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value={REASONING_EFFORT_DEFAULT}>Default (max)</option>
                {REASONING_EFFORTS.map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </select>
              {!endpoint.supports_reasoning_effort && (
                <span className="shrink-0 text-[0.7rem] text-[color:var(--text-muted)]">
                  field omitted
                </span>
              )}
            </div>
          )}

          {endpoint.kind === "anthropic" && (
            <div className="flex items-center gap-2">
              <label
                className="shrink-0 text-xs text-[color:var(--text-muted)]"
                htmlFor={wsId}
              >
                Workspace ID
              </label>
              <input
                id={wsId}
                type="text"
                value={endpoint.workspace_id ?? ""}
                onChange={(e) => onEndpointChange({ workspace_id: e.target.value })}
                placeholder="ws_… (optional)"
                title="Optional Anthropic workspace id — sent as the anthropic-workspace-id header so usage is attributed to that workspace"
                className="flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
          )}

          <div className="grid grid-cols-2 gap-2">
            <div className="flex items-center gap-2">
              <label
                className="shrink-0 text-xs text-[color:var(--text-muted)]"
                htmlFor={maxCtxId}
              >
                Max context
              </label>
              <input
                id={maxCtxId}
                type="number"
                min={1}
                placeholder="default"
                value={endpoint.max_context ?? ""}
                onChange={(e) => {
                  const v = parsePositiveIntInput(e.target.value.trim());
                  if (v !== undefined) {
                    onEndpointChange({ max_context: v });
                  }
                }}
                className="min-w-0 flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
            <div className="flex items-center gap-2">
              <label
                className="shrink-0 text-xs text-[color:var(--text-muted)]"
                htmlFor={maxOutId}
              >
                Max output
              </label>
              <input
                id={maxOutId}
                type="number"
                min={1}
                placeholder="default"
                value={endpoint.max_output_tokens ?? ""}
                onChange={(e) => {
                  const v = parsePositiveIntInput(e.target.value.trim());
                  if (v !== undefined) {
                    onEndpointChange({ max_output_tokens: v });
                  }
                }}
                className="min-w-0 flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
          </div>

          {/* Discovered-cap hints: auto-filled values get a "detected" note;
              explicit values that disagree with the provider's report get a
              warning + one-click Apply (writes the PER-MODEL entry, so the
              endpoint default stays untouched for the other models). Only
              rendered when the reference model has discovered caps AND is
              still in the list — a picked-then-deleted model is a ghost
              whose Apply would no-op via upsert pruning (review 2026-08-22
              L2). */}
          {capsRefModel && endpoint.models.some((m) => m === capsRefModel) && refCaps && (
            <div className="flex flex-col gap-0.5 text-[0.7rem]">
              {refCaps.ctx != null &&
                effCapsCtx != null &&
                effCapsCtx !== refCaps.ctx && (
                  <span className="text-amber-400">
                    Endpoint reports {fmtTokens(refCaps.ctx)} context for “
                    {capsRefModel}” (configured {fmtTokens(effCapsCtx)}).{" "}
                    <button
                      type="button"
                      onClick={() =>
                        onEndpointChange({
                          model_configs: upsertModelConfig(endpoint, capsRefModel, {
                            max_context: refCaps.ctx,
                          }),
                        })
                      }
                      className="underline underline-offset-2 hover:text-[color:var(--accent-color)]"
                    >
                      Apply discovered
                    </button>
                  </span>
                )}
              {refCaps.out != null &&
                effCapsOut != null &&
                effCapsOut !== refCaps.out && (
                  <span className="text-amber-400">
                    Endpoint reports {fmtTokens(refCaps.out)} max output for “
                    {capsRefModel}” (configured{" "}
                    {fmtTokens(effCapsOut)}).{" "}
                    <button
                      type="button"
                      onClick={() =>
                        onEndpointChange({
                          model_configs: upsertModelConfig(endpoint, capsRefModel, {
                            max_output_tokens: refCaps.out,
                          }),
                        })
                      }
                      className="underline underline-offset-2 hover:text-[color:var(--accent-color)]"
                    >
                      Apply discovered
                    </button>
                  </span>
                )}
              {((refCaps.ctx != null && effCapsCtx == null) ||
                (refCaps.out != null && effCapsOut == null)) && (
                <span className="text-[color:var(--text-muted)]">
                  Caps detected from endpoint for “{capsRefModel}” — empty
                  per-model fields auto-fill with these values; Save to persist.
                </span>
              )}
            </div>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={() => void testConnection()}
              disabled={testState.status === "loading"}
              className="flex items-center gap-1 rounded-lg border border-border bg-bg-secondary px-2 py-1 text-xs text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--accent-color)] disabled:opacity-50"
            >
              <RefreshCw
                className={`h-3 w-3 ${testState.status === "loading" ? "animate-spin" : ""}`}
              />
              Test connection
            </button>
            {testState.status === "ok" && (
              <span className="text-xs text-emerald-400">
                OK — {testState.count} model{testState.count === 1 ? "" : "s"}
              </span>
            )}
            {testState.status === "error" && (
              <span className="max-w-full truncate text-xs text-red-300" title={testState.message}>
                {testState.message}
              </span>
            )}
          </div>

          <div className="space-y-1.5" ref={pickerRef}>
            <div className="flex items-center justify-between">
              <span className="text-xs font-medium text-[color:var(--text-muted)]">Models</span>
              <div className="flex items-center gap-3">
                <button
                  type="button"
                  onClick={openAddPicker}
                  title="Fetch the model list from this endpoint's server and pick one to add"
                  className="flex items-center gap-1 text-xs text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--accent-color)]"
                >
                  <RefreshCw className="h-3 w-3" />
                  Pick from server
                </button>
                <button
                  type="button"
                  onClick={onAddModel}
                  className="flex items-center gap-1 text-xs text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--accent-color)]"
                >
                  <Plus className="h-3 w-3" />
                  Add model
                </button>
              </div>
            </div>

            {pickerOpen === "__add__" && (
              <ModelPickerDropdown
                autoFocusFilter
                status={fetchState.status}
                error={fetchState.status === "error" ? fetchState.message : ""}
                models={filteredModels}
                query={pickerQuery}
                onQueryChange={setPickerQuery}
                onRefresh={() => {
                  void fetchServerModels(true);
                }}
                onPick={(id) => {
                  setLastPicked(id);
                  onPickModel(id);
                  setPickerOpen(null);
                }}
              />
            )}

            {endpoint.models.length === 0 && pickerOpen !== "__add__" && (
              <div className="rounded border border-dashed border-border px-2 py-1.5 text-[0.7rem] text-[color:var(--text-muted)]">
                No models — click “Add model” or “Pick from server”.
              </div>
            )}
            {endpoint.models.map((m, mi) => (
              <div key={mi} className="relative flex flex-col gap-1">
                <div className="flex items-center gap-1.5">
                  <input
                    value={pickerOpen === mi ? pickerQuery : m}
                    onChange={(e) => {
                      setPickerQuery(e.target.value);
                    }}
                    onFocus={() => openRowPicker(mi)}
                    onKeyDown={(e) => {
                      if (e.key === "Escape" && pickerOpen === mi) {
                        e.preventDefault();
                        closePicker(true);
                      }
                    }}
                    placeholder="model id (focus to fetch from server)"
                    spellCheck={false}
                    className="flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
                  />
                  {isDefault && m && (
                    <button
                      type="button"
                      onClick={() => onDefaultModel(m)}
                      title={defaultModel === m ? "Default model" : "Set as default model"}
                      className={`shrink-0 rounded px-1.5 py-1 text-[0.65rem] transition-colors ${
                        defaultModel === m
                          ? "bg-[color:var(--accent-color)]/20 text-[color:var(--accent-color)]"
                          : "text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
                      }`}
                    >
                      {defaultModel === m ? "★ default" : "set ★"}
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => onDeleteModel(mi)}
                    title="Remove model"
                    aria-label="Remove model"
                    className="shrink-0 rounded p-1 text-[color:var(--text-muted)] transition-colors hover:bg-red-500/10 hover:text-red-400"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>

                {m.trim() !== "" && (
                  <ModelRowConfig
                    endpoint={endpoint}
                    modelId={m}
                    disabled={!endpoint.supports_reasoning_effort}
                    onChange={(patch) => {
                      onEndpointChange({
                        model_configs: upsertModelConfig(endpoint, m, patch),
                      });
                    }}
                  />
                )}

                {pickerOpen === mi && (
                  <ModelPickerDropdown
                    status={fetchState.status}
                    error={fetchState.status === "error" ? fetchState.message : ""}
                    models={filteredModels}
                    query={pickerQuery}
                    onQueryChange={setPickerQuery}
                    onRefresh={() => {
                      void fetchServerModels(true);
                    }}
                    onPick={(id) => {
                      setLastPicked(id);
                      onModelChange(mi, id);
                      setPickerOpen(null);
                    }}
                  />
                )}
              </div>
            ))}
          </div>

          <div className="flex items-center gap-2 pt-1">
            <label className="flex items-center gap-2 text-xs text-[color:var(--text-muted)]">
              <input
                type="radio"
                checked={isDefault}
                onChange={onSetDefault}
                name="default-endpoint"
                className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
              />
              Use as default endpoint
            </label>
          </div>
        </>
      )}
    </div>
  );
}

/**
 * The per-model config strip under one model row: this model's own
 * max-context / max-output overrides, its reasoning-effort allow-list and
 * persisted default, and its Vision flag. All fields fall back to the
 * endpoint-level values when left empty/unchecked (the backend merges:
 * per-model wins over endpoint-level) — so one endpoint can mix text-only
 * and vision-capable models.
 */
function ModelRowConfig({
  endpoint,
  modelId,
  disabled,
  onChange,
}: {
  endpoint: EndpointEditable;
  modelId: string;
  /** Disabled when the endpoint doesn't support reasoning_effort. */
  disabled: boolean;
  onChange: (patch: { max_context?: number | null; max_output_tokens?: number | null; reasoning_efforts?: string[]; reasoning_effort?: string | null; multimodal?: boolean | null }) => void;
}) {
  const config = modelConfigFor(endpoint, modelId);

  const effortValue =
    config.reasoning_efforts.length > 0
      ? config.reasoning_efforts.join(",")
      : "";

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded bg-bg-secondary/60 px-2 py-1 text-[0.7rem] text-[color:var(--text-muted)]">
      {/* Per-model max-context override */}
      <label className="flex items-center gap-1">
        ctx
        <input
          type="number"
          min={1}
          placeholder="endpoint default"
          value={config.max_context ?? ""}
          onChange={(e) => {
            const v = parsePositiveIntInput(e.target.value.trim());
            if (v !== undefined) onChange({ max_context: v });
          }}
          className="w-24 rounded border border-border bg-bg-primary px-1 py-0.5 font-mono text-[0.7rem] text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
      </label>
      {/* Per-model max-output override */}
      <label className="flex items-center gap-1">
        out
        <input
          type="number"
          min={1}
          placeholder="default"
          value={config.max_output_tokens ?? ""}
          onChange={(e) => {
            const v = parsePositiveIntInput(e.target.value.trim());
            if (v !== undefined) onChange({ max_output_tokens: v });
          }}
          className="w-24 rounded border border-border bg-bg-primary px-1 py-0.5 font-mono text-[0.7rem] text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
      </label>
      {/* Per-model reasoning-effort allow-list (comma-separated; "off" is
          always available at request time regardless of this list). */}
      <label className="flex items-center gap-1">
        efforts
        <input
          type="text"
          disabled={disabled}
          placeholder={disabled ? "n/a" : "e.g. max,high"}
          value={effortValue}
          onChange={(e) => onChange({ reasoning_efforts: parseEffortsList(e.target.value) })}
          title={
            disabled
              ? "This endpoint does not accept reasoning_effort"
              : "Comma-separated reasoning-effort values this model supports (\"off\" is always available)"
          }
          className="w-40 rounded border border-border bg-bg-primary px-1 py-0.5 font-mono text-[0.7rem] text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
        />
      </label>
      {/* Per-model reasoning-effort default (backlog 5b099aef): the
          persisted default for THIS model — the toolbar dropdown stays the
          runtime override on top. "endpoint default" = inherit the
          endpoint's Effort value (then the app default "max"). Hidden for
          anthropic, like the endpoint-level Effort control. */}
      {endpoint.kind !== "anthropic" && (
        <label className="flex items-center gap-1">
          effort
          <select
            value={effortToSelectValue(config.reasoning_effort ?? null)}
            onChange={(e) =>
              onChange({ reasoning_effort: effortFromSelectValue(e.target.value) })
            }
            disabled={disabled}
            title={
              disabled
                ? "This endpoint does not accept reasoning_effort"
                : "Default reasoning effort for this model (unset = the endpoint's Effort value)"
            }
            className="rounded border border-border bg-bg-primary px-1 py-0.5 font-mono text-[0.7rem] text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
          >
            <option value={REASONING_EFFORT_DEFAULT}>endpoint default</option>
            {REASONING_EFFORTS.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </label>
      )}
      {/* Per-model multimodal (vision) override: checked sends image blocks
          to THIS model (a vision-capable model also stops the vision-model
          fallback at runtime); unchecked inherits the endpoint's Multimodal
          flag. An explicit false is settable in endpoints.toml only. */}
      <label
        className="flex items-center gap-1"
        title="Send image blocks to this model even when the endpoint default is text-only (per-model multimodal). Unchecked = inherit the endpoint's Multimodal flag; an explicit off, set in endpoints.toml, also shows unchecked."
      >
        vision
        <input
          type="checkbox"
          checked={config.multimodal === true}
          onChange={(e) => onChange({ multimodal: e.target.checked ? true : null })}
          className="h-3 w-3 accent-[color:var(--accent-color)]"
        />
      </label>
      <span className="text-[0.65rem] text-[color:var(--text-muted)]">
        per-model overrides (empty = endpoint values)
      </span>
    </div>
  );
}
