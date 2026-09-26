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

/** The six boolean levers ([general.optimizer]) — the knobs ride separately. */
const BOOL_FIELDS = [
  "delta_reads",
  "compress_output",
  "archive",
  "compaction_survival",
  "quality_score",
  "lean_output_nudge",
] as const;
type BoolField = (typeof BOOL_FIELDS)[number];

/** The numeric optimizer knobs (edited as numbers). */
type Knobs = {
  archive_min_chars: number;
  compress_min_chars: number;
  lean_output_fill_pct: number;
  nudge_cooldown_requests: number;
};

/** The lever rows: field, title, and the one-line explanation under it. */
const LEVERS: { field: BoolField; label: string; hint: string }[] = [
  {
    field: "delta_reads",
    label: "Delta & skeleton re-reads",
    hint: "read_files serves a signature skeleton (or a diff) instead of the full file on a re-read.",
  },
  {
    field: "compress_output",
    label: "Command-output compression",
    hint: "Large shell output from known command families collapses to error/warning lines + counts.",
  },
  {
    field: "archive",
    label: "Archive & expand",
    hint: "Tool results over the size threshold below are archived in full, with an expandable preview.",
  },
  {
    field: "compaction_survival",
    label: "Compaction survival",
    hint: "A pre-compaction checkpoint, a must-preserve decisions block, and a post-compaction digest.",
  },
  {
    field: "quality_score",
    label: "Context quality score",
    hint: "Shows the S–F context-quality grade in the ctx hover popup.",
  },
  {
    field: "lean_output_nudge",
    label: "Lean-output nudge",
    hint: "A cache-safe note nudging concise visible output past the fill threshold below.",
  },
];

/** The numeric knob rows: field, label, and the input's step. */
const KNOBS: { field: keyof Knobs; label: string; step: number }[] = [
  { field: "archive_min_chars", label: "Archive min. size (chars)", step: 1000 },
  { field: "compress_min_chars", label: "Compress min. size (chars)", step: 500 },
  { field: "lean_output_fill_pct", label: "Nudge at fill (%)", step: 1 },
  { field: "nudge_cooldown_requests", label: "Nudge cooldown (requests)", step: 1 },
];

/**
 * Savings section — the token-optimizer levers ([general.optimizer]: opt-in,
 * off by default) plus the [[pricing]] rows the Stats/Dashboard cost estimates
 * use, so one place covers what a token costs and what Mnemo saves.
 */
export const SavingsSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function SavingsSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const setPricingStore = useAgentStore((s) => s.setPricing);
  const [rows, setRows] = useState<PricingEntry[]>([]);
  const [snapshot, setSnapshot] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);
  // Token-optimizer levers: the six flags, the four knobs, and the
  // extra-command list as newline-separated text (converted on load/save).
  const [flags, setFlags] = useState<Record<BoolField, boolean> | null>(null);
  const [knobs, setKnobs] = useState<Knobs | null>(null);
  const [extraCommands, setExtraCommands] = useState("");

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      const p = (s.pricing ?? []).map((r) => ({ ...r }));
      setRows(p);
      setPricingStore(p);
      // The optimizer block: six flags + four knobs + the extra-command list
      // (edited as newline-separated text).
      const o = s.general.optimizer;
      const f = {} as Record<BoolField, boolean>;
      for (const k of BOOL_FIELDS) f[k] = o[k];
      const k = {
        archive_min_chars: o.archive_min_chars,
        compress_min_chars: o.compress_min_chars,
        lean_output_fill_pct: o.lean_output_fill_pct,
        nudge_cooldown_requests: o.nudge_cooldown_requests,
      };
      const extra = (o.compress_extra_commands ?? []).join("\n");
      setFlags(f);
      setKnobs(k);
      setExtraCommands(extra);
      setSnapshot(JSON.stringify({ pricing: p, flags: f, knobs: k, extra }));
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

  // Both halves of the section are in the draft, so editing a lever dirties
  // the section exactly like editing a pricing row does.
  const dirty =
    snapshot !== "" &&
    JSON.stringify({ pricing: rows, flags, knobs, extra: extraCommands }) !== snapshot;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  function updateRow(i: number, patch: Partial<PricingEntry>) {
    setRows((rs) => rs.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  }

  async function handleSave(): Promise<boolean> {
    if (!flags || !knobs) return false;
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
      // Trimmed, blanks dropped — the backend trims again, so the section's
      // own snapshot stays stable across a save.
      const extra = extraCommands
        .split("\n")
        .map((c) => c.trim())
        .filter((c) => c);
      // The levers + knobs ride along with the pricing rows (the classifier
      // section's pattern) so a flip takes effect on the next tool call — the
      // backend rewrites the live optimizer mirror, no restart. The values are
      // the loaded ones unless the user edited them, and an all-default set
      // still leaves [general.optimizer] omitted from config.toml (the
      // omission is value-based, not provenance-based).
      await saveSettings({
        pricing: cleaned,
        optimizer_delta_reads: flags.delta_reads,
        optimizer_compress_output: flags.compress_output,
        optimizer_archive: flags.archive,
        optimizer_compaction_survival: flags.compaction_survival,
        optimizer_quality_score: flags.quality_score,
        optimizer_lean_output_nudge: flags.lean_output_nudge,
        optimizer_archive_min_chars: knobs.archive_min_chars,
        optimizer_compress_min_chars: knobs.compress_min_chars,
        optimizer_lean_output_fill_pct: knobs.lean_output_fill_pct,
        optimizer_nudge_cooldown_requests: knobs.nudge_cooldown_requests,
        optimizer_compress_extra_commands: extra,
      });
      setRows(cleaned);
      setExtraCommands(extra.join("\n"));
      setPricingStore(cleaned);
      setSnapshot(
        JSON.stringify({ pricing: cleaned, flags, knobs, extra: extra.join("\n") }),
      );
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
        Loading savings…
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Savings
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Token savings (the optimizer levers) and the model rates the Stats
          view uses for cost estimates — what a token costs, and what Mnemo
          saves.
        </p>
      </div>

      {flags && knobs && (
        <div className="space-y-3">
          <div className="space-y-0.5">
            <h4 className="text-xs font-semibold text-[color:var(--text-primary)]">
              Optimizer levers
            </h4>
            <p className="text-xs text-[color:var(--text-muted)]">
              All opt-in and off by default. The Dashboard meters what they
              save; a flip lands on the next tool call — no restart.
            </p>
          </div>

          <div className="space-y-1.5">
            {LEVERS.map((lever) => (
              <label
                key={lever.field}
                className="flex cursor-pointer items-start gap-2 rounded-lg border border-border bg-bg-primary p-2"
              >
                <input
                  type="checkbox"
                  checked={flags[lever.field]}
                  onChange={(e) =>
                    setFlags((f) =>
                      f ? { ...f, [lever.field]: e.target.checked } : f,
                    )
                  }
                  className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[color:var(--accent-color)]"
                />
                <span className="space-y-0.5">
                  <span className="block text-xs text-[color:var(--text-primary)]">
                    {lever.label}
                  </span>
                  <span className="block text-[0.65rem] text-[color:var(--text-muted)]">
                    {lever.hint}
                  </span>
                </span>
              </label>
            ))}
          </div>

          <div className="grid grid-cols-2 gap-2">
            {KNOBS.map((knob) => (
              <div key={knob.field} className="space-y-0.5">
                <label className="text-[0.65rem] text-[color:var(--text-muted)]">
                  {knob.label}
                </label>
                <input
                  type="number"
                  min={0}
                  step={knob.step}
                  value={knobs[knob.field]}
                  onChange={(e) =>
                    setKnobs((k) =>
                      k ? { ...k, [knob.field]: Number(e.target.value) || 0 } : k,
                    )
                  }
                  className="w-full rounded border border-border bg-bg-secondary px-1.5 py-1 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
                />
              </div>
            ))}
          </div>

          <div className="space-y-0.5">
            <label className="text-[0.65rem] text-[color:var(--text-muted)]">
              Extra compressible commands (one pattern per line)
            </label>
            <textarea
              rows={3}
              value={extraCommands}
              onChange={(e) => setExtraCommands(e.target.value)}
              className="w-full resize-y rounded border border-border bg-bg-secondary px-1.5 py-1 font-mono text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
            />
          </div>
        </div>
      )}

      <div className="space-y-0.5">
        <h4 className="text-xs font-semibold text-[color:var(--text-primary)]">
          Model rates
        </h4>
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
          Savings saved.
        </div>
      )}
    </div>
  );
});
