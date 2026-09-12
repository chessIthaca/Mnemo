// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useState } from "react";
import { getSettings, saveSettings, errMsg } from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import type { SettingsSectionHandle } from "../types";

/**
 * Git section — the configurable list of git subcommands that are *core
 * operations* (always force the interactive approval prompt, even in
 * Autonomous mode or under a matching safety rule). Defaults to
 * `merge, push`; the user can add (e.g. `checkout`) or remove entries.
 *
 * The list is stored in `config.toml` under `[git].core_operations` and is
 * pushed live to every GitTool on save (no registry rebuild), so a change
 * takes effect on the next git tool call.
 */
export const GitSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function GitSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const [coreOps, setCoreOps] = useState("");
  const [coreOpsSnap, setCoreOpsSnap] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      const ops = (s.git?.core_operations ?? []).join(", ");
      setCoreOps(ops);
      setCoreOpsSnap(ops);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
  }, [active]);

  const dirty = coreOps !== coreOpsSnap;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  async function handleSave(): Promise<boolean> {
    setSaving(true);
    setError(null);
    setOk(false);
    try {
      const patch: Parameters<typeof saveSettings>[0] = {};
      if (coreOps !== coreOpsSnap) {
        const ops = coreOps
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean);
        // Warn before saving a list that omits merge or push: those are the
        // historical core operations (landing commits on main / pushing to a
        // remote), and removing them lets git run those without approval in
        // Autonomous mode or under a matching safety rule. The user has full
        // control (this is an explicit "whether and which" setting), but a
        // confirmation prevents an accidental clear.
        const lower = ops.map((s) => s.toLowerCase());
        const missing = ["merge", "push"].filter((c) => !lower.includes(c));
        if (missing.length > 0) {
          const ok = window.confirm(
            `${missing.join(" and ")} ${missing.length === 1 ? "is" : "are"} not in the ` +
              `core-operations list. Without ${missing.join("/")}, git can run ` +
              `${missing.join("/")} without approval in Autonomous mode or under a ` +
              `matching safety rule. Save anyway?`,
          );
          if (!ok) {
            return false;
          }
        }
        patch.core_operations = ops;
      }
      await saveSettings(patch);
      // Re-derive the display from the backend-normalized form (trim +
      // lowercase) so the input reflects what was actually stored, not the
      // raw (possibly mixed-case) text the user typed.
      const normalized = (patch.core_operations ?? [])
        .join(", ");
      setCoreOps(normalized);
      setCoreOpsSnap(normalized);
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
        Loading git settings…
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Core operations
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Comma-separated git subcommands that <strong>always require
          approval</strong> — even in Autonomous mode or under a matching safety
          rule. These are operations that land commits on a shared branch or
          publish to a remote. Defaults to <code>merge, push</code>.
        </p>
      </div>

      <div className="space-y-2">
        <label
          className="text-sm text-[color:var(--text-primary)]"
          htmlFor="git-core-ops"
        >
          Git subcommands that always require approval
        </label>
        <input
          id="git-core-ops"
          type="text"
          value={coreOps}
          onChange={(e) => setCoreOps(e.target.value)}
          placeholder="merge, push"
          className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
        <p className="text-[0.7rem] text-[color:var(--text-muted)]">
          Entries are matched case-insensitively against the git tool's{" "}
          <code>subcommand</code> argument. Changes take effect on the next git
          call (no restart needed).
        </p>
      </div>

      {saving && (
        <div className="rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-muted)]">
          Saving…
        </div>
      )}
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
