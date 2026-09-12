// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from "react";
import { Plus } from "lucide-react";
import { useAgentStore } from "../../../hooks/useAgentStore";
import { getSettings, getApiKeys, saveEndpoints, errMsg } from "../../../lib/tauri";
import type { EndpointEditable } from "../../../lib/tauri";
import { ErrorDialog } from "../ErrorDialog";
import {
  blankEndpoint,
  kindFromConfig,
  makeUid,
  serializeProviders,
} from "../types";
import type { SettingsSectionHandle } from "../types";
import { EndpointCard } from "./EndpointCard";

export interface ProvidersSectionProps {
  /** When true, (re)load endpoints + keys from the backend. */
  active: boolean;
  /** Report dirty state to the Settings shell (for close-confirm). */
  onDirtyChange: (dirty: boolean) => void;
}

/**
 * Providers section — editable endpoint cards, API keys, default provider/model,
 * dirty-gated Save. Reloads whenever `active` becomes true so reopening Settings
 * never shows a stale draft.
 */
export const ProvidersSection = forwardRef<SettingsSectionHandle, ProvidersSectionProps>(
  function ProvidersSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);

  const [endpoints, setEndpoints] = useState<EndpointEditable[]>([]);
  const [uids, setUids] = useState<string[]>([]);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [apiKeys, setApiKeys] = useState<Record<string, string>>({});
  const [defaultProvider, setDefaultProvider] = useState<string | null>(null);
  const [defaultModel, setDefaultModel] = useState<string | null>(null);
  const [snapshot, setSnapshot] = useState<string>("");
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // The full-error dialog (long validation chains clip in the inline strip).
  const [showErrorDialog, setShowErrorDialog] = useState(false);
  const [savedOk, setSavedOk] = useState<{ swapped: boolean } | null>(null);

  async function load() {
    setLoading(true);
    setLoadError(null);
    try {
      const [settings, keys] = await Promise.all([getSettings(), getApiKeys()]);
      const eps: EndpointEditable[] = (settings.endpoints ?? []).map((e) => ({
        name: e.name,
        kind: kindFromConfig(e.kind),
        base_url: e.base_url,
        models: [...e.models],
        // Per-model configs ride the wire in a parallel array (id-keyed on
        // the backend, so order drift is tolerated). Absent on legacy
        // endpoints → empty.
        model_configs: (e.model_configs ?? []).map((mc) => ({
          id: mc.id,
          max_context: mc.max_context ?? null,
          max_output_tokens: mc.max_output_tokens ?? null,
          reasoning_efforts: [...(mc.reasoning_efforts ?? [])],
          reasoning_effort: mc.reasoning_effort ?? null,
          multimodal: mc.multimodal ?? null,
        })),
        max_context: e.max_context ?? null,
        max_output_tokens: e.max_output_tokens ?? null,
        multimodal: e.multimodal ?? false,
        // Absent key → true (matches backend serde default for back-compat).
        supports_reasoning_effort: e.supports_reasoning_effort ?? true,
        reasoning_effort: e.reasoning_effort ?? null,
        workspace_id: e.workspace_id ?? null,
      }));
      setEndpoints(eps);
      setUids(eps.map(() => makeUid()));
      setExpanded(new Set());
      setApiKeys({ ...keys });
      setDefaultProvider(settings.general?.default_provider ?? null);
      setDefaultModel(settings.general?.default_model ?? null);
      setSnapshot(
        serializeProviders(
          eps,
          keys,
          settings.general?.default_provider ?? null,
          settings.general?.default_model ?? null,
        ),
      );
      setSaveError(null);
      setSavedOk(null);
    } catch (e) {
      setLoadError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  // Reload every time the section becomes active (Settings open / nav to Providers).
  useEffect(() => {
    if (active) void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  // Clear secrets from React state when the section deactivates (dialog closed).
  useEffect(() => {
    if (!active) {
      setApiKeys({});
      setSnapshot("");
      onDirtyChange(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  const dirty = useMemo(() => {
    if (!snapshot) return false;
    return (
      serializeProviders(endpoints, apiKeys, defaultProvider, defaultModel) !==
      snapshot
    );
  }, [endpoints, apiKeys, defaultProvider, defaultModel, snapshot]);

  useEffect(() => {
    onDirtyChange(dirty);
  }, [dirty, onDirtyChange]);

  // The endpoint the default-model dropdown draws its options from — the one
  // marked as the default provider.
  const defaultEndpoint = endpoints.find((e) => e.name === defaultProvider) ?? null;

  // Models the default endpoint serves. The configured default is kept as an
  // option even when absent from the endpoint's list (the backend tolerates a
  // pre-save running model not in the list).
  const defaultModelOptions = useMemo(() => {
    const models = (defaultEndpoint?.models ?? []).filter((m) => m.trim() !== "");
    if (defaultModel && !models.includes(defaultModel)) {
      models.push(defaultModel);
    }
    return models;
  }, [defaultEndpoint, defaultModel]);

  function updateEndpoint(i: number, patch: Partial<EndpointEditable>) {
    setEndpoints((eps) => eps.map((e, idx) => (idx === i ? { ...e, ...patch } : e)));
  }

  function addEndpoint() {
    const uid = makeUid();
    setEndpoints((eps) => [...eps, blankEndpoint()]);
    setUids((ids) => [...ids, uid]);
    setExpanded((s) => new Set(s).add(uid));
  }

  function deleteEndpoint(i: number) {
    const ep = endpoints[i];
    const label = ep.name.trim() || "(unnamed)";
    if (!window.confirm(`Delete endpoint "${label}"? This removes its API key and cannot be undone.`)) {
      return;
    }
    setEndpoints((eps) => eps.filter((_, idx) => idx !== i));
    setUids((ids) => ids.filter((_, idx) => idx !== i));
    setExpanded((s) => {
      const next = new Set(s);
      next.delete(uids[i]);
      return next;
    });
    setApiKeys((keys) => {
      const next = { ...keys };
      delete next[ep.name];
      return next;
    });
    if (defaultProvider === ep.name) {
      setDefaultProvider(endpoints.find((_, idx) => idx !== i)?.name ?? null);
      setDefaultModel(null);
    }
  }

  function setKey(name: string, value: string) {
    setApiKeys((keys) => ({ ...keys, [name]: value }));
  }

  function setModelAt(epIndex: number, modelIndex: number, value: string) {
    setEndpoints((eps) =>
      eps.map((e, idx) =>
        idx === epIndex
          ? { ...e, models: e.models.map((m, mi) => (mi === modelIndex ? value : m)) }
          : e,
      ),
    );
  }

  function addModel(epIndex: number) {
    setEndpoints((eps) =>
      eps.map((e, idx) => (idx === epIndex ? { ...e, models: [...e.models, ""] } : e)),
    );
  }

  function addModelWithValue(epIndex: number, value: string) {
    const v = value.trim();
    if (!v) return;
    setEndpoints((eps) =>
      eps.map((e, idx) => {
        if (idx !== epIndex) return e;
        if (e.models.some((m) => m === v)) return e;
        return { ...e, models: [...e.models, v] };
      }),
    );
  }

  function deleteModel(epIndex: number, modelIndex: number) {
    const ep = endpoints[epIndex];
    setEndpoints((eps) =>
      eps.map((e, idx) =>
        idx === epIndex
          ? { ...e, models: e.models.filter((_, mi) => mi !== modelIndex) }
          : e,
      ),
    );
    if (defaultModel === ep.models[modelIndex] && defaultProvider === ep.name) {
      setDefaultModel(null);
    }
  }

  async function handleSave(): Promise<boolean> {
    setSaving(true);
    setSaveError(null);
    setSavedOk(null);
    try {
      const res = await saveEndpoints(
        endpoints,
        apiKeys,
        defaultProvider,
        defaultModel,
      );
      setSavedOk({ swapped: res.provider_swapped });
      setSnapshot(
        serializeProviders(endpoints, apiKeys, defaultProvider, defaultModel),
      );
      bumpConfigVersion();
      window.setTimeout(() => setSavedOk(null), 3000);
      return true;
    } catch (e) {
      setSaveError(errMsg(e));
      // Pop the full text into the dialog immediately — the inline strip
      // clips long multi-line errors.
      setShowErrorDialog(true);
      return false;
    } finally {
      setSaving(false);
    }
  }

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({ save: handleSave }));

  if (loading) {
    return (
      <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
        Loading providers…
      </div>
    );
  }
  if (loadError) {
    return (
      <div className="space-y-2">
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          Failed to load config: {loadError}
        </div>
        <button
          type="button"
          onClick={() => void load()}
          className="rounded-lg border border-border bg-bg-primary px-3 py-1.5 text-xs text-[color:var(--text-primary)] hover:opacity-90"
        >
          Retry
        </button>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Providers
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Configure OpenAI-compatible endpoints, API keys, and the default
          model. Changes require Save before they take effect.
        </p>
        <div className="flex items-center gap-2 pt-1">
          <label
            htmlFor="default-model-select"
            className="shrink-0 text-xs text-[color:var(--text-muted)]"
          >
            Default model
          </label>
          <select
            id="default-model-select"
            value={defaultModel ?? ""}
            onChange={(e) => setDefaultModel(e.target.value || null)}
            disabled={defaultEndpoint === null}
            title="Model the app starts with — saved to config.toml; Save re-syncs the live provider"
            className="min-w-0 flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
          >
            {defaultEndpoint === null ? (
              <option value="">(set a default endpoint first)</option>
            ) : defaultModelOptions.length === 0 ? (
              <option value="">(no models on this endpoint)</option>
            ) : (
              <>
                {defaultModel === null && <option value="">(choose a model)</option>}
                {defaultModelOptions.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </>
            )}
          </select>
        </div>
        {(defaultProvider || defaultModel) && (
          <p className="text-xs text-[color:var(--text-primary)]">
            Default:{" "}
            <span className="font-medium text-[color:var(--accent-color)]">
              {defaultProvider ?? "—"}
              {defaultModel ? ` / ${defaultModel}` : ""}
            </span>
          </p>
        )}
      </div>

      {endpoints.length === 0 && (
        <div className="rounded-lg border border-border bg-bg-primary px-3 py-4 text-center text-xs text-[color:var(--text-muted)]">
          No endpoints configured. Click “Add endpoint” to create one.
        </div>
      )}

      {endpoints.map((ep, i) => (
        <EndpointCard
          key={uids[i] ?? `ep-fallback-${i}`}
          endpoint={ep}
          apiKey={apiKeys[ep.name] ?? ""}
          isDefault={defaultProvider === ep.name}
          defaultModel={defaultProvider === ep.name ? defaultModel : null}
          open={expanded.has(uids[i] ?? `ep-fallback-${i}`)}
          onToggle={() => {
            const uid = uids[i];
            if (!uid) return;
            setExpanded((s) => {
              const next = new Set(s);
              if (next.has(uid)) next.delete(uid);
              else next.add(uid);
              return next;
            });
          }}
          onEndpointChange={(patch) => updateEndpoint(i, patch)}
          onKeyChange={(v) => setKey(ep.name, v)}
          onModelChange={(mi, v) => setModelAt(i, mi, v)}
          onAddModel={() => addModel(i)}
          onPickModel={(v) => addModelWithValue(i, v)}
          onDeleteModel={(mi) => deleteModel(i, mi)}
          onDelete={() => deleteEndpoint(i)}
          onSetDefault={() => {
            setDefaultProvider(ep.name);
            setDefaultModel(ep.models[0] ?? null);
          }}
          onDefaultModel={(m) => setDefaultModel(m)}
          onNameChange={(newName) => {
            const oldName = ep.name;
            if (newName === oldName) return;
            setApiKeys((keys) => {
              const next = { ...keys };
              const existing = next[oldName] ?? "";
              delete next[oldName];
              if (existing) next[newName] = existing;
              return next;
            });
            if (defaultProvider === oldName) setDefaultProvider(newName);
            updateEndpoint(i, { name: newName });
          }}
        />
      ))}

      <button
        type="button"
        onClick={addEndpoint}
        className="flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-muted)] transition-colors hover:border-[color:var(--accent-color)]/50 hover:text-[color:var(--accent-color)]"
      >
        <Plus className="h-3.5 w-3.5" />
        Add endpoint
      </button>

      <div className="space-y-2">
        {saveError && (
          <button
            type="button"
            onClick={() => setShowErrorDialog(true)}
            title="Click to see the full error"
            className="block w-full cursor-pointer rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-left text-xs text-red-300"
          >
            <span className="line-clamp-2 break-words">{saveError}</span>
            <span className="mt-0.5 block text-[0.7rem] text-red-300/70 underline">
              Show full error…
            </span>
          </button>
        )}
        {savedOk && (
          <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
            {savedOk.swapped
              ? "Saved — the live provider was re-synced."
              : "Endpoints saved."}
          </div>
        )}
      </div>

      <ErrorDialog
        open={showErrorDialog && saveError != null}
        onClose={() => setShowErrorDialog(false)}
        message={saveError ?? ""}
      />
    </div>
  );
});
