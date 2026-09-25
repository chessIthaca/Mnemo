// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useState } from "react";
import { Download, Check } from "lucide-react";
import { fmtPct } from "../../../lib/format";
import {
  getSettings,
  saveSettings,
  errMsg,
  getClassifierStatus,
  onClassifierStatus,
  listLayaCheckpoints,
  setupLayaRuntime,
} from "../../../lib/tauri";
import type {
  ClassifierStatusWire,
  LayaCheckpointInfo,
} from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import {
  serializeClassifier,
  type ClassifierDraft,
  type SettingsSectionHandle,
} from "../types";

const inputCls =
  "w-full rounded-lg border border-border bg-bg-primary px-3 py-1.5 text-sm text-[color:var(--text-primary)] outline-none focus:border-[color:var(--accent-color)]";

type LayaMode = "external" | "managed";

/** Short chip label for a status (the downloading payload collapses to a %). */
function statusLabel(status: ClassifierStatusWire): string {
  if (typeof status === "object") {
    if ("downloading" in status) {
      return `downloading ${status.downloading.label} (${fmtPct(
        status.downloading.progress * 100,
      )}%)`;
    }
    return `fine-tuning ${status.finetuning.label}`;
  }
  return status;
}

/** One-line explanation of each live classifier status. */
function statusHint(status: ClassifierStatusWire, managed: boolean): string {
  if (typeof status === "object") {
    if ("downloading" in status) {
      return `Downloading ${status.downloading.label} — ${fmtPct(
        status.downloading.progress * 100,
      )}%. The managed runtime + checkpoint total ~0.8–1 GB on disk.`;
    }
    return `Fine-tuning ${status.finetuning.label} on the logged failure classifications — the sidecar keeps serving; the checkpoint hot-swaps when the run finishes.`;
  }
  switch (status) {
    case "ready":
      return "Ready — the classifier answers typed questions over HTTP.";
    case "failed":
      return managed
        ? "Failed — the sidecar did not start or the last call got no answer; see the log under the app config dir (laya/server-<pid>.log)."
        : "Failed — the last call got no answer; check that laya-serve is running at the endpoint.";
    case "installing":
      return "Installing — preparing the managed Laya runtime (uv + virtualenv + laya[serve])…";
    case "starting":
      return "Starting — launching the local laya-serve sidecar (the first model load can take a minute)…";
    default:
      return "Disabled — no classifier backend exists; the app behaves exactly as before.";
  }
}

/**
 * Classifier section — the opt-in Laya "System 1" classifier.
 *
 * Laya (https://huggingface.co/convaiinnovations/laya) is a fast, calibrated
 * text classifier, not a generator: the app asks it typed questions
 * (choice / score / yes-no) and reads back calibrated probabilities over
 * HTTP (`POST /v1/systemone`).
 *
 * Two modes:
 * - **Managed (recommended)**: Mnemo downloads a self-contained runtime
 *   (uv + virtualenv + `laya[serve]` + the checkpoint) right here in the
 *   section — no command line, no Python prerequisites — and automatically
 *   starts, monitors, and stops the local `laya-serve` sidecar on
 *   127.0.0.1 while the app runs.
 * - **External endpoint (advanced)**: the user runs `laya-serve` themselves
 *   and points Mnemo at its base URL.
 *
 * Disabled by default — an absent or disabled configuration changes nothing:
 * no classifier calls, no startup cost, no new failure modes.
 */
export const ClassifierSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function ClassifierSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const [mode, setMode] = useState<LayaMode>("managed");
  const [enabled, setEnabled] = useState(false);
  const [endpoint, setEndpoint] = useState("");
  const [checkpoint, setCheckpoint] = useState("english");
  // The auto-typing opt-in (`[general.laya] auto_type_memories`) — a
  // separate toggle from the classifier enable flag.
  const [autoType, setAutoType] = useState(false);
  // The failure-triage opt-in (`[general.laya] failure_triage`), its kNN
  // overlay (`[general.laya] failure_triage_knn`), and the startup
  // fine-tune opt-in (`[general.laya] auto_finetune`, managed mode only) —
  // separate toggles, same opt-in convention.
  const [failureTriage, setFailureTriage] = useState(false);
  const [failureTriageKnn, setFailureTriageKnn] = useState(false);
  const [autoFinetune, setAutoFinetune] = useState(false);
  const [catalog, setCatalog] = useState<LayaCheckpointInfo[]>([]);
  const [snapshot, setSnapshot] = useState<string>("");
  const [status, setStatus] = useState<ClassifierStatusWire>("disabled");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  // Download progress: the 0–1 fraction from a `downloading` status payload.
  const [progress, setProgress] = useState(0);
  // A setup is in flight (installing / downloading / starting phases).
  const busy = status === "installing" || status === "starting" ||
    typeof status === "object";

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      const laya = s.general.laya;
      const next: ClassifierDraft = {
        enabled: laya?.enabled ?? false,
        mode: laya?.mode ?? "managed",
        checkpoint: laya?.checkpoint ?? "english",
        endpoint: laya?.endpoint ?? "",
        autoTypeMemories: laya?.auto_type_memories ?? false,
        failureTriage: laya?.failure_triage ?? false,
        failureTriageKnn: laya?.failure_triage_knn ?? false,
        autoFinetune: laya?.auto_finetune ?? false,
      };
      setEnabled(next.enabled);
      setMode(next.mode);
      setCheckpoint(next.checkpoint);
      setEndpoint(next.endpoint);
      setAutoType(next.autoTypeMemories);
      setFailureTriage(next.failureTriage);
      setFailureTriageKnn(next.failureTriageKnn);
      setAutoFinetune(next.autoFinetune);
      setSnapshot(serializeClassifier(next));
      setCatalog(await listLayaCheckpoints());
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
  // `classifier://status` at startup, through the managed-runtime setup and
  // start phases, and after a settings save rewires the backend.
  useEffect(() => {
    if (!active) return;
    let unlisten: (() => void) | null = null;
    onClassifierStatus((next) => {
      setStatus(next);
      if (typeof next === "object") {
        // Download progress rides the downloading payload only — the
        // fine-tune state carries no progress (the checkpoint hot-swaps
        // when the run finishes).
        if ("downloading" in next) {
          setProgress(next.downloading.progress);
        }
      } else {
        // Terminal status — refresh the catalog so a finished setup shows
        // the checkpoint as installed.
        setProgress(0);
        void listLayaCheckpoints().then((c) => setCatalog(c));
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [active]);

  const draft: ClassifierDraft = {
    enabled,
    mode,
    checkpoint,
    endpoint,
    autoTypeMemories: autoType,
    failureTriage,
    failureTriageKnn,
    autoFinetune,
  };
  const dirty = snapshot !== "" && serializeClassifier(draft) !== snapshot;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  async function handleDownload(checkpointId: string) {
    setError(null);
    try {
      // setupLayaRuntime is fire-and-forget (returns immediately); the
      // progress UI is driven by the onClassifierStatus listener through the
      // installing / downloading / starting phases until ready/failed.
      await setupLayaRuntime(checkpointId);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({
    save: async () => {
      setSaving(true);
      setError(null);
      setOk(false);
      try {
        // The mode + enable flag + checkpoint + the four Laya opt-ins ride
        // along so toggling takes effect without a restart (the backend
        // rewires the live classifier — flipping the auto-type,
        // failure-triage and kNN-overlay flags — and starts/stops the
        // managed sidecar). A blank endpoint clears the stored URL
        // server-side; managed mode never persists one.
        await saveSettings({
          laya_enabled: enabled,
          laya_auto_type_memories: autoType,
          laya_failure_triage: failureTriage,
          laya_failure_triage_knn: failureTriageKnn,
          laya_auto_finetune: autoFinetune,
          laya_mode: mode,
          ...(mode === "managed" ? { laya_checkpoint: checkpoint } : {}),
          laya_endpoint: mode === "managed" ? "" : endpoint,
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
          score, or yes/no — with probabilities. Managed mode downloads and
          runs it for you; disabled by default: nothing is called, and the
          app behaves exactly as before.
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
            — consult it for cheap, calibrated decisions
          </span>
        </span>
      </label>

      <label className="flex cursor-pointer items-start gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="checkbox"
          checked={autoType}
          onChange={(e) => setAutoType(e.target.checked)}
          className="mt-0.5 h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        <span>
          Auto-type memory records
          <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
            — a confident classifier corrects the record's SPEC / DECISION /
            BUG / PLAN / HOW / REVIEW prefix on write; low confidence keeps
            yours. Enable only against a fine-tuned checkpoint — base models
            mis-classify.
          </span>
        </span>
      </label>

      <label className="flex cursor-pointer items-start gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="checkbox"
          checked={failureTriage}
          onChange={(e) => setFailureTriage(e.target.checked)}
          className="mt-0.5 h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        <span>
          Classify failures to steer retries
          <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
            — at every failure site the error is classified
            (transient / permanent / needs_user / flaky_test) and a confident
            class steers the harness: read-only calls auto-retry once without
            a model roundtrip, other classes get targeted guidance, and
            needs-user/permanent provider errors skip the retry ladder. Every
            classified failure is logged with its true outcome so the startup
            fine-tune can learn from it. Enable only against a fine-tuned
            checkpoint — base models mis-classify.
          </span>
        </span>
      </label>

      <label className="flex cursor-pointer items-start gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="checkbox"
          checked={failureTriageKnn}
          onChange={(e) => setFailureTriageKnn(e.target.checked)}
          className="mt-0.5 h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        <span>
          Learn from logged failures between fine-tunes (kNN)
          <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
            — a local overlay over the failure-triage training log: the most
            similar logged failures vote on the class (the vote share is the
            confidence), so a resolved disposition is reusable on the very
            next similar failure — no retraining, and no laya-serve needed (it
            rides the memory embedder). Only takes effect while “Classify
            failures” is on; below-threshold votes fall back to the
            classifier / the pre-classifier rules.
          </span>
        </span>
      </label>

      <label className="flex cursor-pointer items-start gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="checkbox"
          checked={autoFinetune}
          onChange={(e) => setAutoFinetune(e.target.checked)}
          className="mt-0.5 h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        <span>
          Fine-tune on startup from logged failures
          <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
            — managed mode only: when enough new classified failures have
            accrued since the last fine-tune, the checkpoint is retrained in
            the background and hot-swapped. Never blocks startup.
          </span>
        </span>
      </label>

      <div className="space-y-2">
        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="radio"
            name="laya-mode"
            checked={mode === "managed"}
            onChange={() => setMode("managed")}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          <span>
            Managed — recommended
            <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
              — Mnemo downloads and runs laya-serve for you (127.0.0.1)
            </span>
          </span>
        </label>
        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="radio"
            name="laya-mode"
            checked={mode === "external"}
            onChange={() => setMode("external")}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          <span>
            External endpoint (advanced)
            <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
              — you run laya-serve yourself
            </span>
          </span>
        </label>
      </div>

      {mode === "managed" && (
        <div className="space-y-2">
          {catalog.map((c) => {
            const isSel = checkpoint === c.id;
            return (
              <div
                key={c.id}
                className={`rounded-lg border p-3 transition-colors ${
                  isSel
                    ? "border-[color:var(--accent-color)] bg-[color:var(--accent-color)]/5"
                    : "border-border bg-bg-primary"
                }`}
              >
                <label className="flex cursor-pointer items-start gap-2">
                  <input
                    type="radio"
                    name="laya-checkpoint"
                    checked={isSel}
                    onChange={() => setCheckpoint(c.id)}
                    className="mt-0.5 h-3.5 w-3.5 accent-[color:var(--accent-color)]"
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium text-[color:var(--text-primary)]">
                        {c.name}
                      </span>
                      {c.installed ? (
                        <span className="flex items-center gap-0.5 rounded bg-emerald-950/40 px-1.5 py-0.5 text-[0.65rem] text-emerald-400">
                          <Check className="h-3 w-3" /> installed
                        </span>
                      ) : (
                        <span className="rounded bg-bg-tertiary px-1.5 py-0.5 text-[0.65rem] text-[color:var(--text-muted)]">
                          ~{c.size_mb} MB
                        </span>
                      )}
                    </div>
                    <div className="mt-0.5 text-[0.7rem] text-[color:var(--text-muted)]">
                      <code>{c.id}</code> checkpoint
                    </div>
                  </div>
                </label>
                {!c.installed && !busy && (
                  <button
                    type="button"
                    onClick={() => void handleDownload(c.id)}
                    className="mt-2 flex items-center gap-1.5 rounded-lg border border-border px-2.5 py-1 text-xs text-[color:var(--text-primary)] hover:border-[color:var(--accent-color)]/50"
                  >
                    <Download className="h-3.5 w-3.5" />
                    Download (~{c.size_mb} MB)
                  </button>
                )}
              </div>
            );
          })}
          <div className="rounded-lg border border-border bg-bg-tertiary/40 p-3 text-[0.7rem] text-[color:var(--text-muted)]">
            The managed runtime is self-contained but heavy: uv + virtualenv +
            laya[serve] (~0.8–1 GB) plus the checkpoint (~650–810 MB), all
            under the app config dir. No Python needs to be preinstalled.
          </div>
        </div>
      )}

      {mode === "external" && (
        <>
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
              <code>/v1/systemone</code>. A blank URL means "not configured" —
              an enabled classifier then stays disabled.
            </span>
          </label>

          <div className="space-y-1 rounded-lg border border-border bg-bg-tertiary/40 p-3 text-[0.7rem] text-[color:var(--text-muted)]">
            <div className="font-medium text-[color:var(--text-primary)]">
              Install + run laya-serve yourself
            </div>
            <div>
              <code>pip install "laya[serve]"</code>, then{" "}
              <code>laya-serve</code> — it listens on{" "}
              <code>http://127.0.0.1:8000</code> by default.
            </div>
          </div>
        </>
      )}

      <div className="rounded-lg border border-border bg-bg-primary p-3 text-xs">
        <span className="text-[color:var(--text-muted)]">Status: </span>
        <span className="font-medium text-[color:var(--text-primary)]">
          {statusLabel(status)}
        </span>
        {status === "ready" && mode === "external" && endpoint.trim() !== "" && (
          <span className="text-[color:var(--text-muted)]">
            {" "}
            @ {endpoint.trim()}
          </span>
        )}
        {typeof status === "object" && (
          <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-bg-tertiary">
            <div
              className="h-full rounded-full bg-[color:var(--accent-color)] transition-[width] duration-300"
              style={{ width: `${Math.round(progress * 100)}%` }}
            />
          </div>
        )}
        <div className="mt-1 text-[0.7rem] text-[color:var(--text-muted)]">
          {statusHint(status, mode === "managed")}
        </div>
      </div>

      <div className="rounded-lg border border-border bg-bg-tertiary/40 p-3 text-[0.7rem] text-[color:var(--text-muted)]">
        Base Laya checkpoints are near-chance on custom tasks until fine-tuned
        — enable it for experiments and logging, and gate any real decision
        on a confidence threshold.
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
