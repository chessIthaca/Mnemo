// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { getSettings, saveSettings, listVisionModels, errMsg } from "../../../lib/tauri";
import type { EndpointInfo, VisionModelInfo } from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import type { SettingsSectionHandle } from "../types";
import { ModelPickerDropdown } from "./ModelPickerDropdown";

/**
 * Vision section — optional image-to-text fallback endpoint + model.
 */
export const VisionSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function VisionSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const [endpoints, setEndpoints] = useState<EndpointInfo[]>([]);
  const [enabled, setEnabled] = useState(false);
  const [endpoint, setEndpoint] = useState("");
  const [model, setModel] = useState("");
  const [snapshot, setSnapshot] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);

  // Live model picker state — mirrors EndpointCard's fetchServerModels.
  const [visionModels, setVisionModels] = useState<VisionModelInfo[]>([]);
  const [modelsCacheKey, setModelsCacheKey] = useState<string>("");
  const [fetchState, setFetchState] = useState<
    | { status: "idle" }
    | { status: "loading" }
    | { status: "ready" }
    | { status: "error"; message: string }
  >({ status: "idle" });
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerQuery, setPickerQuery] = useState("");
  const pickerRef = useRef<HTMLDivElement>(null);
  const fetchReqId = useRef(0);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      setEndpoints(s.endpoints ?? []);
      const vm = s.general.vision_model;
      const en = !!(vm && vm.endpoint && vm.model);
      setEnabled(en);
      setEndpoint(vm?.endpoint ?? "");
      setModel(vm?.model ?? "");
      setSnapshot(JSON.stringify({ en, endpoint: vm?.endpoint ?? "", model: vm?.model ?? "" }));
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
  }, [active]);

  const dirty =
    snapshot !== "" &&
    JSON.stringify({ en: enabled, endpoint, model }) !== snapshot;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  // Fetch the endpoint's live /models list with vision-capability flags.
  // Mirrors EndpointCard's fetchServerModels: cache per base_url, race-guard
  // via fetchReqId, skip when cached+ready unless forced.
  async function fetchVisionModels(force = false) {
    if (!endpoint) {
      setFetchState({ status: "idle" });
      return;
    }
    const ep = endpoints.find((e) => e.name === endpoint);
    const key = `${ep?.base_url ?? ""}\n${ep?.kind ?? ""}`;
    if (!force && key === modelsCacheKey && fetchState.status === "ready") {
      return;
    }
    const reqId = ++fetchReqId.current;
    setFetchState({ status: "loading" });
    try {
      const list = await listVisionModels(endpoint, undefined, undefined, ep?.kind);
      if (reqId !== fetchReqId.current) return;
      setVisionModels(list);
      setModelsCacheKey(key);
      setFetchState({ status: "ready" });
    } catch (e) {
      if (reqId !== fetchReqId.current) return;
      setVisionModels([]);
      setModelsCacheKey(key);
      setFetchState({ status: "error", message: errMsg(e) });
    }
  }

  // (Re)fetch when the endpoint changes; reset when vision is disabled or no
  // endpoint is selected.
  useEffect(() => {
    if (enabled && endpoint) {
      void fetchVisionModels(true);
    } else {
      setVisionModels([]);
      setModelsCacheKey("");
      setFetchState({ status: "idle" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, endpoint]);

  function closePicker(commit: boolean) {
    if (commit && pickerQuery.trim() && pickerQuery !== model) {
      setModel(pickerQuery.trim());
    }
    setPickerOpen(false);
  }

  // Close the picker on outside click (mirrors EndpointCard's pattern).
  useEffect(() => {
    if (!pickerOpen) return;
    function handleClick(e: MouseEvent) {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        closePicker(true);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pickerOpen, pickerQuery, model]);

  const capable = visionModels.filter((m) => m.vision_capable);
  const capableIds = new Set(capable.map((m) => m.id));
  // Show ALL models — never hide the user's vision model. Sort vision-capable
  // ones to the top so a known-capable model is easy to find, but every model
  // remains pickable (the provider's modality flag can be a false negative).
  const pickerModels = [...visionModels]
    .sort((a, b) => Number(b.vision_capable) - Number(a.vision_capable))
    .map((m) => m.id);
  const filteredModels = pickerQuery.trim()
    ? pickerModels.filter((m) => m.toLowerCase().includes(pickerQuery.trim().toLowerCase()))
    : pickerModels;

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({
    save: async () => {
      setSaving(true);
      setError(null);
      setOk(false);
      try {
        if (!enabled) {
          await saveSettings({ clear_vision_model: true });
        } else {
          await saveSettings({
            vision_model: {
              endpoint: endpoint.trim(),
              model: model.trim(),
            },
          });
        }
        setSnapshot(JSON.stringify({ en: enabled, endpoint, model }));
        setOk(true);
        bumpConfigVersion();
        window.setTimeout(() => setOk(false), 2500);
        return true;
      } catch (e) {
        setError(errMsg(e));
        return false;
      } finally {
        setSaving(false);
      }
    },
  }));

  if (loading) {
    return (
      <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
        Loading vision settings…
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Vision
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          When the main provider is not multimodal, images are described via this
          fallback model (<code className="text-[color:var(--accent-color)]">describe_image</code>
          ). Prefer a multimodal endpoint.
        </p>
      </div>

      <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
          className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        Enable vision fallback
      </label>

      {enabled && (
        <>
          <div className="space-y-1">
            <label className="text-sm text-[color:var(--text-primary)]" htmlFor="vis-ep">
              Endpoint
            </label>
            <select
              id="vis-ep"
              value={endpoint}
              onChange={(e) => {
                setEndpoint(e.target.value);
                const ep = endpoints.find((x) => x.name === e.target.value);
                if (ep?.models[0] && !model) setModel(ep.models[0]);
              }}
              className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
            >
              <option value="">— select —</option>
              {endpoints.map((ep) => (
                <option key={ep.name} value={ep.name}>
                  {ep.name}
                  {ep.multimodal ? " (multimodal)" : ""}
                </option>
              ))}
            </select>
          </div>
          <div className="space-y-1" ref={pickerRef}>
            <div className="flex items-center justify-between">
              <label className="text-sm text-[color:var(--text-primary)]" htmlFor="vis-model">
                Model
              </label>
              <button
                type="button"
                onClick={() => void fetchVisionModels(true)}
                title="Re-fetch the model list from the server"
                className="flex items-center gap-1 text-xs text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--accent-color)]"
              >
                <RefreshCw
                  className={`h-3 w-3 ${fetchState.status === "loading" ? "animate-spin" : ""}`}
                />
                Pick from server
              </button>
            </div>
            <div className="relative">
              <input
                id="vis-model"
                value={pickerOpen ? pickerQuery : model}
                onChange={(e) => {
                  setPickerQuery(e.target.value);
                  if (!pickerOpen) setPickerOpen(true);
                }}
                onFocus={() => {
                  setPickerOpen(true);
                  setPickerQuery(model);
                  void fetchVisionModels();
                }}
                onKeyDown={(e) => {
                  if (e.key === "Escape" && pickerOpen) {
                    e.preventDefault();
                    closePicker(true);
                  }
                }}
                placeholder="model id (focus to fetch from server)"
                spellCheck={false}
                className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
              {pickerOpen && (
                <ModelPickerDropdown
                  status={fetchState.status}
                  error={fetchState.status === "error" ? fetchState.message : ""}
                  models={filteredModels}
                  query={pickerQuery}
                  onQueryChange={setPickerQuery}
                  onRefresh={() => {
                    void fetchVisionModels(true);
                  }}
                  onPick={(id) => {
                    setModel(id);
                    setPickerOpen(false);
                  }}
                  visionCapableIds={capableIds}
                />
              )}
            </div>
            {fetchState.status === "ready" && (
              <p className="text-[0.7rem] text-[color:var(--text-muted)]">
                {capable.length > 0
                  ? `${capable.length} of ${visionModels.length} model(s) are vision-capable (shown first)`
                  : visionModels.length > 0
                    ? "No models reported as vision-capable — pick your vision model"
                    : "No models available"}
              </p>
            )}
          </div>
        </>
      )}

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Vision settings saved.
        </div>
      )}
    </div>
  );
});
