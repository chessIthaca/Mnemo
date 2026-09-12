// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import { Copy, Download, Upload } from "lucide-react";
import {
  getSettings,
  saveSettings,
  saveEndpoints,
  getApiKeys,
  errMsg,
} from "../../../lib/tauri";
import type { EndpointEditable, PricingEntry } from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import {
  buildExportBundle,
  importEndpointEditable,
  validateImportBundle,
} from "../types";
import type { SettingsSectionHandle } from "../types";
import { fmtPct } from "../../../lib/format";

/**
 * The dirty predicate for a numeric settings knob (reviews I4 + F3,
 * 2026-08-18): a knob counts as changed only when it holds a VALID value
 * (`Number.isFinite` and `>= min`) that differs from the saved snapshot by
 * more than `eps` (float sliders need an epsilon; integer inputs pass 0).
 * A cleared field (`Number("") === 0`) or stray text (`NaN`, where
 * `NaN !== NaN` is always true) cannot be saved, so it must not count as
 * dirty — otherwise the section latches dirty forever. Pure — extracted so
 * the vitest suite can pin the exact table.
 */
export function numericKnobDirty(value: number, snap: number, min: number, eps = 0): boolean {
  return Number.isFinite(value) && value >= min && Math.abs(value - snap) > eps;
}

/**
 * Advanced section — context summarize rate, trace memory limits, config
 * path, projects list, export/import of a JSON settings bundle.
 */
export const AdvancedSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function AdvancedSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const setPricingStore = useAgentStore((s) => s.setPricing);
  const [fillRate, setFillRate] = useState(0.5);
  const [fillSnap, setFillSnap] = useState(0.5);
  // Proxy cache ceiling in tokens — `null` is the cleared input (guard
  // disabled) and saves as 0, which the backend maps to None.
  const [cacheCeiling, setCacheCeiling] = useState<number | null>(340_000);
  const [cacheCeilingSnap, setCacheCeilingSnap] = useState<number | null>(340_000);
  const [traceBudget, setTraceBudget] = useState(16);
  const [traceBudgetSnap, setTraceBudgetSnap] = useState(16);
  const [reqCap, setReqCap] = useState(256);
  const [reqCapSnap, setReqCapSnap] = useState(256);
  const [skipDirs, setSkipDirs] = useState("");
  const [skipDirsSnap, setSkipDirsSnap] = useState("");
  const [inspectEnabled, setInspectEnabled] = useState(false);
  const [inspectSnap, setInspectSnap] = useState(false);
  const [autoCompact, setAutoCompact] = useState(false);
  const [autoCompactSnap, setAutoCompactSnap] = useState(false);
  const [configDir, setConfigDir] = useState("");
  const [projects, setProjects] = useState<{ name: string; path: string }[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  const [copied, setCopied] = useState(false);
  const [includeKeys, setIncludeKeys] = useState(false);
  const [importing, setImporting] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      const rate = s.context.summarize_at_fill_rate;
      setFillRate(rate);
      setFillSnap(rate);
      const ceiling = s.context.proxy_cache_ceiling_tokens ?? null;
      setCacheCeiling(ceiling);
      setCacheCeilingSnap(ceiling);
      const budget = s.trace?.memory_budget_mb ?? 16;
      setTraceBudget(budget);
      setTraceBudgetSnap(budget);
      const cap = s.trace?.request_body_cap_kb ?? 256;
      setReqCap(cap);
      setReqCapSnap(cap);
      const dirs = (s.markdown?.skip_dirs ?? []).join(", ");
      setSkipDirs(dirs);
      setSkipDirsSnap(dirs);
      setInspectEnabled(s.general.enable_browser_inspection);
      setInspectSnap(s.general.enable_browser_inspection);
      setAutoCompact(s.general.auto_compact_on_plan_complete);
      setAutoCompactSnap(s.general.auto_compact_on_plan_complete);
      setConfigDir(s.config_dir);
      setProjects(s.projects ?? []);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
  }, [active]);

  // A numeric knob only counts as dirty when it holds a VALID value that
  // differs from the saved one — a cleared field (`Number("") === 0`) or
  // stray text (`NaN`, where `NaN !== NaN` is always true) cannot be saved,
  // so it must not latch the dirty flag forever (reviews I4 + F3,
  // 2026-08-18). `fillRate` is a range input (always valid) but gets the
  // same gate for uniformity.
  const budgetValid = Number.isFinite(traceBudget) && traceBudget >= 1;
  const capValid = Number.isFinite(reqCap) && reqCap >= 1;
  // The ceiling accepts a sane cliff value (>= 65_536, matching the backend
  // validator) or a cleared field (guard disabled, saved as 0). Anything
  // else — stray text, sub-floor values — must not latch dirty (reviews
  // I4 + F3 pattern).
  const cacheCeilingValid =
    cacheCeiling === null ||
    (Number.isFinite(cacheCeiling) && (cacheCeiling === 0 || cacheCeiling >= 65_536));
  const cacheCeilingDirty = cacheCeilingValid && cacheCeiling !== cacheCeilingSnap;

  const dirty =
    numericKnobDirty(fillRate, fillSnap, 0.05, 1e-9) ||
    numericKnobDirty(traceBudget, traceBudgetSnap, 1) ||
    numericKnobDirty(reqCap, reqCapSnap, 1) ||
    cacheCeilingDirty ||
    skipDirs !== skipDirsSnap ||
    inspectEnabled !== inspectSnap ||
    autoCompact !== autoCompactSnap;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  async function handleSave(): Promise<boolean> {
    setSaving(true);
    setError(null);
    setOk(false);
    try {
      const patch: Parameters<typeof saveSettings>[0] = {};
      if (numericKnobDirty(fillRate, fillSnap, 0.05, 1e-9)) {
        patch.summarize_at_fill_rate = fillRate;
      }
      if (cacheCeilingDirty) {
        // Backend maps 0 to None (guard disabled); the cleared input (null)
        // also saves as 0.
        patch.proxy_cache_ceiling_tokens = cacheCeiling ?? 0;
      }
      if (numericKnobDirty(traceBudget, traceBudgetSnap, 1)) {
        patch.trace_memory_budget_mb = Math.round(traceBudget);
      }
      if (numericKnobDirty(reqCap, reqCapSnap, 1)) {
        patch.trace_request_body_cap_kb = Math.round(reqCap);
      }
      if (skipDirs !== skipDirsSnap) {
        patch.skip_dirs = skipDirs
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean);
      }
      if (inspectEnabled !== inspectSnap) {
        patch.enable_browser_inspection = inspectEnabled;
      }
      if (autoCompact !== autoCompactSnap) {
        patch.auto_compact_on_plan_complete = autoCompact;
      }
      await saveSettings(patch);
      setFillSnap(fillRate);
      setCacheCeilingSnap(cacheCeilingValid ? cacheCeiling : cacheCeilingSnap);
      if (!cacheCeilingValid) setCacheCeiling(cacheCeilingSnap);
      // Revert an invalid (cleared/NaN) entry to the saved value instead of
      // latching it into the snapshot — an invalid value can never be saved,
      // so the UI must not pretend it was (review I4, 2026-08-18).
      setTraceBudgetSnap(budgetValid ? traceBudget : traceBudgetSnap);
      if (!budgetValid) setTraceBudget(traceBudgetSnap);
      setReqCapSnap(capValid ? reqCap : reqCapSnap);
      if (!capValid) setReqCap(reqCapSnap);
      setSkipDirsSnap(skipDirs);
      setInspectSnap(inspectEnabled);
      setAutoCompactSnap(autoCompact);
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
  }

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({ save: handleSave }));

  async function copyPath() {
    try {
      await navigator.clipboard.writeText(configDir);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* ignore */
    }
  }

  async function handleExport() {
    setError(null);
    try {
      const s = await getSettings();
      let apiKeys: Record<string, string> | undefined;
      if (includeKeys) {
        apiKeys = await getApiKeys();
      }
      const bundle = buildExportBundle(s, { apiKeys, includeKeys });
      const blob = new Blob([JSON.stringify(bundle, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `mnemo-settings-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      setOk(true);
      window.setTimeout(() => setOk(false), 2500);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  async function handleImportFile(file: File) {
    setImporting(true);
    setError(null);
    setOk(false);
    try {
      const text = await file.text();
      const raw = JSON.parse(text) as unknown;
      const verr = validateImportBundle(raw);
      if (verr) throw new Error(verr);
      const o = raw as Record<string, unknown>;

      // General / ui / context / vision / pricing
      const patch: Parameters<typeof saveSettings>[0] = {};
      const general = o.general as Record<string, unknown> | undefined;
      if (general) {
        if (typeof general.safety === "string") patch.safety = general.safety;
        if (typeof general.default_provider === "string") {
          patch.default_provider = general.default_provider;
        }
        if (typeof general.default_model === "string") {
          patch.default_model = general.default_model;
        }
        if (general.vision_model && typeof general.vision_model === "object") {
          const vm = general.vision_model as { endpoint?: string; model?: string };
          if (vm.endpoint && vm.model) {
            patch.vision_model = { endpoint: vm.endpoint, model: vm.model };
          }
        } else if (general.vision_model === null) {
          patch.clear_vision_model = true;
        }
      }
      const context = o.context as
        | { summarize_at_fill_rate?: number; proxy_cache_ceiling_tokens?: number | null }
        | undefined;
      if (context && typeof context.summarize_at_fill_rate === "number") {
        patch.summarize_at_fill_rate = context.summarize_at_fill_rate;
      }
      if (context && context.proxy_cache_ceiling_tokens !== undefined) {
        // Preserve "disabled" bundles: null imports as 0 (backend None).
        patch.proxy_cache_ceiling_tokens = context.proxy_cache_ceiling_tokens ?? 0;
      }
      const ui = o.ui as { theme?: string; show_token_usage?: boolean } | undefined;
      if (ui) {
        if (ui.theme) patch.theme = ui.theme;
        if (typeof ui.show_token_usage === "boolean") {
          patch.show_token_usage = ui.show_token_usage;
        }
      }
      if (Array.isArray(o.pricing)) {
        patch.pricing = o.pricing as PricingEntry[];
      }

      // Endpoints first when present (so vision/default provider validation passes).
      // Note: two-step import is not fully atomic — if saveSettings fails after
      // saveEndpoints, endpoints/keys may already be updated. Surface that clearly.
      let endpointsImported = false;
      if (Array.isArray(o.endpoints) && o.endpoints.length > 0) {
        // The mapping lives in types.ts (importEndpointEditable) so the
        // per-model `model_configs` survive the export/import round trip
        // (review 2026-08-22 M1 — previously dropped here).
        const eps: EndpointEditable[] = (
          o.endpoints as Array<Record<string, unknown>>
        ).map(importEndpointEditable);
        const keys =
          o.api_keys && typeof o.api_keys === "object"
            ? (o.api_keys as Record<string, string>)
            : {};
        const dp =
          typeof general?.default_provider === "string"
            ? general.default_provider
            : null;
        const dm =
          typeof general?.default_model === "string" ? general.default_model : null;
        await saveEndpoints(eps, keys, dp, dm);
        endpointsImported = true;
      }

      try {
        if (Object.keys(patch).length > 0) {
          await saveSettings(patch);
        }
      } catch (e) {
        if (endpointsImported) {
          throw new Error(
            `Partial import: endpoints/keys were saved, but other settings failed: ${errMsg(e)}`,
          );
        }
        throw e;
      }

      if (ui?.theme === "dark" || ui?.theme === "light" || ui?.theme === "system") {
        useAgentStore.getState().setTheme(ui.theme);
      }
      if (typeof ui?.show_token_usage === "boolean") {
        useAgentStore.getState().setShowTokenUsage(ui.show_token_usage);
      }
      if (Array.isArray(o.pricing)) {
        setPricingStore(o.pricing as PricingEntry[]);
      }

      bumpConfigVersion();
      await load();
      setOk(true);
      window.setTimeout(() => setOk(false), 3000);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setImporting(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  }

  if (loading) {
    return (
      <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
        Loading advanced settings…
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Context
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          When conversation fill reaches this fraction of the model context window,
          earlier turns are summarized to free space. The Run-All auto-compact
          below follows this same dial — it fires only when the post-item
          context is at/above this threshold.
        </p>
      </div>

      <div className="space-y-2">
        <label className="text-sm text-[color:var(--text-primary)]" htmlFor="fill-rate">
          Summarize at fill rate{" "}
          <span className="text-[color:var(--text-muted)]">
            ({fmtPct(fillRate * 100)}%)
          </span>
        </label>
        <input
          id="fill-rate"
          type="range"
          min={0.05}
          max={0.95}
          step={0.05}
          value={fillRate}
          onChange={(e) => setFillRate(Number(e.target.value))}
          className="w-full accent-[color:var(--accent-color)]"
        />
        <div className="flex justify-between text-[0.65rem] text-[color:var(--text-muted)]">
          <span>5% (aggressive)</span>
          <span>50% default</span>
          <span>95% (late)</span>
        </div>
      </div>

      <div className="space-y-2">
        <label
          className="text-sm text-[color:var(--text-primary)]"
          htmlFor="cache-ceiling"
        >
          Proxy cache ceiling{" "}
          <span className="text-[color:var(--text-muted)]">
            {cacheCeiling === null
              ? "(disabled)"
              : `(${cacheCeiling.toLocaleString()} tok)`}
          </span>
        </label>
        <input
          id="cache-ceiling"
          type="number"
          min={0}
          step={1000}
          value={cacheCeiling ?? ""}
          placeholder="disabled"
          onChange={(e) => {
            const t = e.target.value.trim();
            setCacheCeiling(t === "" ? null : Number(t));
          }}
          className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
        <p className="text-xs text-[color:var(--text-muted)]">
          Summarize before the ~340K-token cliff where LiteLLM-class proxies
          drop whole-conversation prefix caching. Empty disables the guard;
          minimum 65,536 when set.
        </p>
      </div>

      <div className="space-y-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Trace memory
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Bounds the in-memory LLM trace log (the Trace tab's data). The
          budget caps total raw-response bytes across the ring — oldest
          payloads are evicted first, their rows stay. The request cap
          truncates oversized request bodies while keeping their JSON
          structure.
        </p>
        <div className="grid grid-cols-2 gap-3">
          <div className="space-y-1">
            <label
              className="text-sm text-[color:var(--text-primary)]"
              htmlFor="trace-budget"
            >
              Memory budget (MiB)
            </label>
            <input
              id="trace-budget"
              type="number"
              min={1}
              max={512}
              step={1}
              value={traceBudget}
              onChange={(e) => setTraceBudget(Number(e.target.value))}
              className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
            />
          </div>
          <div className="space-y-1">
            <label
              className="text-sm text-[color:var(--text-primary)]"
              htmlFor="trace-req-cap"
            >
              Request body cap (KiB)
            </label>
            <input
              id="trace-req-cap"
              type="number"
              min={16}
              max={8192}
              step={16}
              value={reqCap}
              onChange={(e) => setReqCap(Number(e.target.value))}
              className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
            />
          </div>
        </div>
        <p className="text-[0.65rem] text-[color:var(--text-muted)]">
          Takes effect immediately — no restart needed.
        </p>
      </div>

      <div className="space-y-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          File viewer skip directories
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Comma-separated directory names never shown in the Files tab's file
          tree (e.g. <code>.git, node_modules, target, dist</code>). Matching
          is by directory name, so a nested <code>target</code> is skipped too.
        </p>
        <input
          type="text"
          value={skipDirs}
          onChange={(e) => setSkipDirs(e.target.value)}
          placeholder=".git, node_modules, target, dist"
          className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
      </div>

      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Config directory
        </h3>
        <div className="flex items-center gap-2">
          <code className="min-w-0 flex-1 truncate rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)]">
            {configDir || "—"}
          </code>
          <button
            type="button"
            onClick={() => void copyPath()}
            className="flex shrink-0 items-center gap-1 rounded-lg border border-border px-2 py-2 text-xs text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
            title="Copy path"
            aria-label="Copy config directory path"
          >
            <Copy className="h-3.5 w-3.5" />
            {copied ? "Copied" : "Copy"}
          </button>
        </div>
        <p className="text-xs text-[color:var(--text-muted)]">
          Contains config.toml, endpoints.toml, keys.toml, projects.toml.
        </p>
      </div>

      <div className="space-y-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Agent browser inspection
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Exposes an unauthenticated debug port (localhost:9222 — next free
          port when multiple instances run) so the agent's{" "}
          <code>browser_*</code> tools can inspect the Browser tab. Any local
          process could run arbitrary JavaScript in the app's webview — enable
          only on a machine you fully control. Requires restart to take effect.
        </p>
        <label className="flex items-center gap-2 text-xs text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={inspectEnabled}
            onChange={(e) => setInspectEnabled(e.target.checked)}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Enable agent browser inspection (restart required)
        </label>
      </div>

      <div className="space-y-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Run-All auto-compact
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          During a Run-All loop, compact the main agent's context after each
          completed plan — before the next item is dispatched — so every item
          starts with a clean summarized context instead of exhausting the
          window mid-item overnight. This checkbox only enables the feature:
          it fires when the post-item context is at/above the effective
          Summarize-at fill-rate threshold (the same dial, proxy-cache cap
          included) — below it the summarization call is skipped and the next
          item dispatches directly. A failed or timed-out compaction logs
          and proceeds; the loop never stalls on it. Only Run-All loops are
          affected — interactive plan completions never compact.
        </p>
        <label className="flex items-center gap-2 text-xs text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={autoCompact}
            onChange={(e) => setAutoCompact(e.target.checked)}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Auto-compact between Run-All items
        </label>
      </div>

      <div className="space-y-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Export / import
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Download a JSON bundle of settings (endpoints, vision, pricing,
          UI). API keys are omitted unless you opt in.
        </p>
        <label className="flex items-center gap-2 text-xs text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={includeKeys}
            onChange={(e) => setIncludeKeys(e.target.checked)}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Include API keys in export (sensitive)
        </label>
        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => void handleExport()}
            className="flex items-center gap-1.5 rounded-lg border border-border px-3 py-1.5 text-xs text-[color:var(--text-primary)] hover:border-[color:var(--accent-color)]/50"
          >
            <Download className="h-3.5 w-3.5" />
            Export JSON
          </button>
          <button
            type="button"
            onClick={() => fileRef.current?.click()}
            disabled={importing}
            className="flex items-center gap-1.5 rounded-lg border border-border px-3 py-1.5 text-xs text-[color:var(--text-primary)] hover:border-[color:var(--accent-color)]/50 disabled:opacity-40"
          >
            <Upload className="h-3.5 w-3.5" />
            {importing ? "Importing…" : "Import JSON"}
          </button>
          <input
            ref={fileRef}
            type="file"
            accept="application/json,.json"
            className="hidden"
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) void handleImportFile(f);
            }}
          />
        </div>
      </div>

      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Known projects
        </h3>
        {projects.length === 0 ? (
          <p className="text-xs text-[color:var(--text-muted)]">No projects registered.</p>
        ) : (
          <ul className="space-y-1">
            {projects.map((p) => (
              <li
                key={p.name}
                className="rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs"
              >
                <span className="font-medium text-[color:var(--text-primary)]">{p.name}</span>
                <span className="mt-0.5 block truncate text-[color:var(--text-muted)]">
                  {p.path}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Done.
        </div>
      )}
    </div>
  );
});
