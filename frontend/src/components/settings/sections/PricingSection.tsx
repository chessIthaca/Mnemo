// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { getSettings, saveSettings, errMsg } from "../../../lib/tauri";
import type { PricingEntry } from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import type { SettingsSectionHandle } from "../types";
import { ModelCombobox } from "./ModelCombobox";
import { buildModelOptions, type ModelOption } from "./modelOptions";

/**
 * Pricing section — [[pricing]] rows for Stats cost estimates.
 */
export const PricingSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function PricingSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const setPricingStore = useAgentStore((s) => s.setPricing);
  const [rows, setRows] = useState<PricingEntry[]>([]);
  const [snapshot, setSnapshot] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      const p = (s.pricing ?? []).map((r) => ({ ...r }));
      setRows(p);
      setSnapshot(JSON.stringify(p));
      setPricingStore(p);
      // The model combobox's dropdown options: every model the configured
      // endpoints serve (grouped/deduped, annotated with endpoint names).
      setModelOptions(buildModelOptions(s.endpoints));
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
  }, [active]);

  const dirty = snapshot !== "" && JSON.stringify(rows) !== snapshot;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  function updateRow(i: number, patch: Partial<PricingEntry>) {
    setRows((rs) => rs.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  }

  async function handleSave(): Promise<boolean> {
    setSaving(true);
    setError(null);
    setOk(false);
    try {
      const cleaned = rows
        .map((r) => ({
          model: r.model.trim(),
          input_per_1m: Number(r.input_per_1m) || 0,
          output_per_1m: Number(r.output_per_1m) || 0,
          cached_per_1m: Number(r.cached_per_1m) || 0,
        }))
        .filter((r) => r.model);
      await saveSettings({ pricing: cleaned });
      setRows(cleaned);
      setSnapshot(JSON.stringify(cleaned));
      setPricingStore(cleaned);
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

  if (loading) {
    return (
      <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
        Loading pricing…
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Pricing
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Dollars per 1M tokens — used by the Stats view for cost estimates.
          Cached rate is typically ~50% of input.
        </p>
      </div>

      {rows.length === 0 && (
        <div className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-[color:var(--text-muted)]">
          No pricing rows. Add one for each model you care about.
        </div>
      )}

      <div className="space-y-2">
        {rows.map((r, i) => (
          <div
            key={i}
            className="grid grid-cols-[1fr_4.5rem_4.5rem_4.5rem_auto] items-end gap-1.5 rounded-lg border border-border bg-bg-primary p-2"
          >
            <div className="space-y-0.5">
              <label className="text-[0.65rem] text-[color:var(--text-muted)]">Model</label>
              <ModelCombobox
                value={r.model}
                onChange={(v) => updateRow(i, { model: v })}
                options={modelOptions}
              />
            </div>
            <div className="space-y-0.5">
              <label className="text-[0.65rem] text-[color:var(--text-muted)]">In $/M</label>
              <input
                type="number"
                min={0}
                step="0.01"
                value={r.input_per_1m}
                onChange={(e) => updateRow(i, { input_per_1m: Number(e.target.value) })}
                className="w-full rounded border border-border bg-bg-secondary px-1.5 py-1 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
            <div className="space-y-0.5">
              <label className="text-[0.65rem] text-[color:var(--text-muted)]">Out $/M</label>
              <input
                type="number"
                min={0}
                step="0.01"
                value={r.output_per_1m}
                onChange={(e) => updateRow(i, { output_per_1m: Number(e.target.value) })}
                className="w-full rounded border border-border bg-bg-secondary px-1.5 py-1 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
            <div className="space-y-0.5">
              <label className="text-[0.65rem] text-[color:var(--text-muted)]">Cache $/M</label>
              <input
                type="number"
                min={0}
                step="0.01"
                value={r.cached_per_1m}
                onChange={(e) => updateRow(i, { cached_per_1m: Number(e.target.value) })}
                className="w-full rounded border border-border bg-bg-secondary px-1.5 py-1 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
            <button
              type="button"
              onClick={() => setRows((rs) => rs.filter((_, idx) => idx !== i))}
              aria-label="Remove pricing row"
              className="mb-0.5 rounded p-1 text-[color:var(--text-muted)] hover:bg-red-500/10 hover:text-red-400"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>
        ))}
      </div>

      <button
        type="button"
        onClick={() =>
          setRows((rs) => [
            ...rs,
            { model: "", input_per_1m: 0, output_per_1m: 0, cached_per_1m: 0 },
          ])
        }
        className="flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-border px-3 py-2 text-xs text-[color:var(--text-muted)] hover:border-[color:var(--accent-color)]/50 hover:text-[color:var(--accent-color)]"
      >
        <Plus className="h-3.5 w-3.5" />
        Add pricing row
      </button>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Pricing saved.
        </div>
      )}
    </div>
  );
});
