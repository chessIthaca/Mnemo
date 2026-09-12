// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useState } from "react";
import { Shield, ShieldAlert, ShieldCheck } from "lucide-react";
import { getSettings, saveSettings, getSafetyRules, saveSafetyRules, errMsg } from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import type { SafetyMode } from "../../../lib/types";
import { SafetyToggleDialog } from "../../layout/SafetyToggleDialog";
import type { SettingsSectionHandle } from "../types";

const MODES: {
  id: SafetyMode;
  label: string;
  description: string;
  icon: typeof Shield;
  danger?: boolean;
}[] = [
  {
    id: "approve-each-action",
    label: "Approve each action",
    description: "Every mutating tool call (writes, shell, git) asks for approval.",
    icon: Shield,
  },
  {
    id: "auto-read-approve-writes",
    label: "Auto-read, approve writes",
    description: "Reads run freely; writes and other mutations still need approval.",
    icon: ShieldCheck,
  },
  {
    id: "auto-approve-project",
    label: "Auto-approve project",
    description:
      "In-project file edits and read-only git (status/diff/log/branch list) auto-run. Shell, mutating git (commit/checkout/stash/branch create|delete), merge/push, and unscoped tools still prompt.",
    icon: ShieldCheck,
  },
  {
    id: "autonomous",
    label: "Autonomous",
    description:
      "No approval prompts. Use only for trusted work. Core ops (merge/push) still always gate.",
    icon: ShieldAlert,
    danger: true,
  },
];

/**
 * Safety section — runtime + persisted safety mode, plus safety.toml editor.
 */
export const SafetySection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  /** Report dirty so the shell can confirm discard on close. */
  onDirtyChange?: (dirty: boolean) => void;
}>(function SafetySection({ active, onDirtyChange }, ref) {
  const setSafetyModeStore = useAgentStore((s) => s.setSafetyMode);
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);

  const [mode, setMode] = useState<SafetyMode>("approve-each-action");
  const [modeSnap, setModeSnap] = useState<SafetyMode>("approve-each-action");
  const [rules, setRules] = useState("");
  const [rulesSnap, setRulesSnap] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  const [confirmAuto, setConfirmAuto] = useState(false);
  const [pendingMode, setPendingMode] = useState<SafetyMode | null>(null);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const [s, rawRules] = await Promise.all([getSettings(), getSafetyRules()]);
      const m = (s.general.safety as SafetyMode) || "approve-each-action";
      setMode(m);
      setModeSnap(m);
      setRules(rawRules);
      setRulesSnap(rawRules);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
  }, [active]);

  const dirty = mode !== modeSnap || rules !== rulesSnap;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  function pickMode(next: SafetyMode) {
    if (next === "autonomous" && mode !== "autonomous") {
      setPendingMode(next);
      setConfirmAuto(true);
      return;
    }
    setMode(next);
  }

  async function handleSave(): Promise<boolean> {
    setSaving(true);
    setError(null);
    setOk(false);
    try {
      if (mode !== modeSnap) {
        await saveSettings({ safety: mode });
        setSafetyModeStore(mode);
        setModeSnap(mode);
      }
      if (rules !== rulesSnap) {
        await saveSafetyRules(rules);
        setRulesSnap(rules);
      }
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
        Loading safety settings…
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Safety mode
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Controls which tool calls require human approval. Status bar quick-switch
          stays available; this panel also persists the default to config.toml.
          Bookkeeping tools (memory + plan lifecycle) never prompt.
        </p>
      </div>

      <div className="space-y-2" role="radiogroup" aria-label="Safety mode">
        {MODES.map((m) => {
          const Icon = m.icon;
          const activeMode = mode === m.id;
          return (
            <button
              key={m.id}
              type="button"
              role="radio"
              aria-checked={activeMode}
              onClick={() => pickMode(m.id)}
              className={`flex w-full items-start gap-3 rounded-lg border px-3 py-2.5 text-left transition-colors ${
                activeMode
                  ? m.danger
                    ? "border-red-500/60 bg-red-500/10"
                    : "border-[color:var(--accent-color)]/50 bg-[color:var(--accent-color)]/10"
                  : "border-border bg-bg-primary hover:bg-bg-tertiary/40"
              }`}
            >
              <Icon
                className={`mt-0.5 h-4 w-4 shrink-0 ${
                  m.danger
                    ? "text-red-400"
                    : activeMode
                      ? "text-[color:var(--accent-color)]"
                      : "text-[color:var(--text-muted)]"
                }`}
              />
              <span className="min-w-0">
                <span
                  className={`block text-sm font-medium ${
                    m.danger ? "text-red-300" : "text-[color:var(--text-primary)]"
                  }`}
                >
                  {m.label}
                </span>
                <span className="mt-0.5 block text-xs text-[color:var(--text-muted)]">
                  {m.description}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
            Safety rules (safety.toml)
          </h3>
        </div>
        <p className="text-xs text-[color:var(--text-muted)]">
          Patterns that auto-approve matching tool calls. Invalid TOML is rejected on save.
        </p>
        <textarea
          value={rules}
          onChange={(e) => setRules(e.target.value)}
          spellCheck={false}
          rows={12}
          className="w-full resize-y rounded-lg border border-border bg-bg-primary px-3 py-2 font-mono text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
          placeholder={"# safety.toml\n# [[rule]]\n# tool = \"shell\"\n# ..."}
        />
      </div>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Safety settings saved.
        </div>
      )}

      <SafetyToggleDialog
        open={confirmAuto}
        onCancel={() => {
          setConfirmAuto(false);
          setPendingMode(null);
        }}
        onConfirm={() => {
          if (pendingMode) setMode(pendingMode);
          setConfirmAuto(false);
          setPendingMode(null);
        }}
      />
    </div>
  );
});
