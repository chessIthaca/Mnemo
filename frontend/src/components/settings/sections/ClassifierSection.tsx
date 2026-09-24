// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useState } from "react";
import {
  getSettings,
  saveSettings,
  errMsg,
  getClassifierStatus,
  onClassifierStatus,
} from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import {
  serializeClassifier,
  type ClassifierDraft,
  type SettingsSectionHandle,
} from "../types";

const inputCls =
  "w-full rounded-lg border border-border bg-bg-primary px-3 py-1.5 text-sm text-[color:var(--text-primary)] outline-none focus:border-[color:var(--accent-color)]";

/** One-line explanation of each live classifier status. */
function statusHint(status: string): string {
  switch (status) {
    case "ready":
      return "Ready — the classifier answers typed questions over HTTP.";
    case "failed":
      return "Failed — the last call got no answer; check that laya-serve is running at the endpoint.";
    default:
      return "Disabled — no classifier backend exists; the app behaves exactly as before.";
  }
}

/**
 * Classifier section — the opt-in Laya "System 1" classifier.
 *
 * Laya (https://huggingface.co/convaiinnovations/laya) is a fast, calibrated
 * text classifier, not a generator: the app asks it typed questions
 * (choice / score / yes-no) and reads back calibrated probabilities. It runs
 * outside the app as a `laya-serve` sidecar; Mnemo only talks to it over HTTP
 * (`POST /v1/systemone`).
 *
 * Disabled by default — an absent or disabled configuration changes nothing:
 * no classifier calls, no startup cost, no new failure modes. Enable it here
 * and point it at a running instance.
 */
export const ClassifierSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function ClassifierSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const [enabled, setEnabled] = useState(false);
  const [endpoint, setEndpoint] = useState("");
  const [snapshot, setSnapshot] = useState<string>("");
  const [status, setStatus] = useState<string>("disabled");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      const laya = s.general.laya;
      const next: ClassifierDraft = {
        enabled: laya?.enabled ?? false,
        endpoint: laya?.endpoint ?? "",
      };
      setEnabled(next.enabled);
      setEndpoint(next.endpoint);
      setSnapshot(serializeClassifier(next));
      // Read the live status directly (not from the startup snapshot) so the
      // section is correct on mount and refreshes after a save-driven rewire.
      setStatus(await getClassifierStatus());
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
  }, [active]);

  // Subscribe to classifier status changes while active — the backend emits
  // `classifier://status` at startup and after a settings save rewires the
  // backend (enable/disable/endpoint changes take effect without a restart).
  useEffect(() => {
    if (!active) return;
    let unlisten: (() => void) | null = null;
    onClassifierStatus((next) => setStatus(next)).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [active]);

  const draft: ClassifierDraft = { enabled, endpoint };
  const dirty = snapshot !== "" && serializeClassifier(draft) !== snapshot;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({
    save: async () => {
      setSaving(true);
      setError(null);
      setOk(false);
      try {
        // A blank endpoint clears the stored URL server-side; the enable flag
        // rides along so toggling takes effect without a restart.
        await saveSettings({
          laya_enabled: enabled,
          laya_endpoint: endpoint,
        });
        setSnapshot(serializeClassifier(draft));
        // The save rewires the live backend + status; pick it up right away.
        setStatus(await getClassifierStatus());
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
        Loading classifier settings…
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Laya classifier
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          An optional "System 1" decision service: a fast, calibrated text
          classifier (not a generator) that answers typed questions — choice,
          score, or yes/no — with probabilities. It runs outside the app as a{" "}
          <code>laya-serve</code> sidecar; Mnemo only talks to it over HTTP.
          Disabled by default: nothing is called, and the app behaves exactly
          as before.
        </p>
      </div>

      <label className="flex cursor-pointer items-start gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
          className="mt-0.5 h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        <span>
          Enable the Laya classifier
          <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
            — consult the endpoint below for cheap, calibrated decisions
          </span>
        </span>
      </label>

      <label className="block space-y-1">
        <span className="text-xs text-[color:var(--text-muted)]">
          laya-serve endpoint URL
        </span>
        <input
          type="text"
          value={endpoint}
          onChange={(e) => setEndpoint(e.target.value)}
          className={inputCls}
          placeholder="http://127.0.0.1:8000"
        />
        <span className="text-[0.7rem] text-[color:var(--text-muted)]">
          Base URL of a running instance; the app posts to{" "}
          <code>/v1/systemone</code>. A blank URL means "not configured" — an
          enabled classifier then stays disabled.
        </span>
      </label>

      <div className="rounded-lg border border-border bg-bg-primary p-3 text-xs">
        <span className="text-[color:var(--text-muted)]">Status: </span>
        <span className="font-medium text-[color:var(--text-primary)]">
          {status}
        </span>
        {status === "ready" && endpoint.trim() !== "" && (
          <span className="text-[color:var(--text-muted)]">
            {" "}
            @ {endpoint.trim()}
          </span>
        )}
        <div className="mt-1 text-[0.7rem] text-[color:var(--text-muted)]">
          {statusHint(status)}
        </div>
      </div>

      <div className="space-y-1 rounded-lg border border-border bg-bg-tertiary/40 p-3 text-[0.7rem] text-[color:var(--text-muted)]">
        <div className="font-medium text-[color:var(--text-primary)]">
          Install + run laya-serve yourself
        </div>
        <div>
          <code>pip install "laya[serve]"</code>, then <code>laya-serve</code> —
          it listens on <code>http://127.0.0.1:8000</code> by default. Mnemo
          never installs, downloads, or starts it for you.
        </div>
        <div>
          Base Laya checkpoints are near-chance on custom tasks until
          fine-tuned — enable it for experiments and logging, and gate any real
          decision on a confidence threshold.
        </div>
      </div>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Classifier settings saved.
        </div>
      )}
      {saving && (
        <div className="text-xs text-[color:var(--text-muted)]">Saving…</div>
      )}
    </div>
  );
});
