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
  listBundledEmbeddingModels,
  downloadBundledModel,
  onEmbedderStatus,
} from "../../../lib/tauri";
import type { BundledModelInfo } from "../../../lib/tauri";
import { useAgentStore } from "../../../hooks/useAgentStore";
import type { SettingsSectionHandle } from "../types";

/**
 * Embeddings section — pick a bundled in-process embedding model for memory
 * recall. The model runs via `fastembed` (ONNX Runtime) entirely on the user's
 * machine — no Ollama, no cloud, no API key. It downloads on first use and
 * caches under the app data dir; a progress bar shows the download.
 *
 * When a model is selected + installed, recall ranks by genuine semantic
 * similarity. When none is selected (or the model fails to load), recall
 * degrades to the built-in HashEmbedder (keyword-overlap only) — it never
 * fails. Switching models re-embeds all existing memories in the background
 * (and the same check runs at startup, so memories committed to git that travel
 * to a machine running a different model are auto re-embedded).
 */
export const EmbeddingSection = forwardRef<SettingsSectionHandle, {
  active: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}>(function EmbeddingSection({ active, onDirtyChange }, ref) {
  const bumpConfigVersion = useAgentStore((s) => s.bumpConfigVersion);
  const [models, setModels] = useState<BundledModelInfo[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [snapshot, setSnapshot] = useState<string>("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);
  // Download progress: the model id being downloaded + a 0–1 fraction.
  const [downloading, setDownloading] = useState<string | null>(null);
  const [progress, setProgress] = useState(0);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const s = await getSettings();
      // "hash" is the persisted sentinel for keyword-only (the backend's
      // clear flag saves it so an absent key can't re-resolve to the default
      // model) — map it to the null selection so the "None (keyword-only)"
      // radio shows checked. Case-insensitive, matching the backend contract
      // (a hand-edited "HASH" config is honored as the opt-out everywhere).
      const HASH: string = "hash";
      const raw = s.general.bundled_embedding_model ?? null;
      const sel = raw?.toLowerCase() === HASH ? null : raw;
      setSelected(sel);
      setSnapshot(JSON.stringify(sel));
      const catalog = await listBundledEmbeddingModels();
      setModels(catalog);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (active) void load();
  }, [active]);

  // Subscribe to embedder status while active so the download progress bar
  // updates live. The backend emits `embedder://status` with a `downloading`
  // payload (carrying model + progress) during a download, then a terminal
  // `ready`/`failed` string when it completes.
  useEffect(() => {
    if (!active) return;
    let unlisten: (() => void) | null = null;
    onEmbedderStatus((status) => {
      // The status is a string for unit variants ("ready", "failed", …) or
      // an object for the downloading variant: {"downloading": {model, progress}}.
      if (typeof status === "string") {
        // Terminal status — if we were downloading, refresh the catalog so the
        // model shows as installed, then clear the progress bar.
        if (downloading !== null) {
          void listBundledEmbeddingModels().then((catalog) => setModels(catalog));
        }
        setDownloading(null);
        setProgress(0);
      } else if (status && typeof status === "object" && "downloading" in status) {
        const d = (status as { downloading: { model: string; progress: number } }).downloading;
        setDownloading(d.model);
        setProgress(d.progress);
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [active, downloading]);

  const dirty = snapshot !== "" && JSON.stringify(selected) !== snapshot;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  async function handleDownload(modelId: string) {
    setError(null);
    setDownloading(modelId);
    setProgress(0);
    try {
      // downloadBundledModel is fire-and-forget (returns immediately); the
      // progress bar is driven by the onEmbedderStatus listener until a
      // Ready/Failed event arrives. Do NOT clear the bar here — the listener
      // owns the download lifecycle.
      await downloadBundledModel(modelId);
    } catch (e) {
      setError(errMsg(e));
      setDownloading(null);
      setProgress(0);
    }
    // The catalog refresh + bar clear happen in the onEmbedderStatus effect
    // when a terminal status (ready/failed) arrives.
  }

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({
    save: async () => {
      setSaving(true);
      setError(null);
      setOk(false);
      try {
        if (selected) {
          await saveSettings({ bundled_embedding_model: selected });
        } else {
          await saveSettings({ clear_bundled_embedding_model: true });
        }
        setSnapshot(JSON.stringify(selected));
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
        Loading embedding settings…
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Embeddings
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          A bundled in-process model ranks memory recall by genuine semantic
          similarity (e.g. a query about "authentication" finds memories about
          "login tokens" even with no shared keywords). It runs entirely on
          your machine — no Ollama, no cloud, no API key. The model downloads
          on first use and caches locally. If a model fails to load, recall
          silently degrades to keyword-only — it never fails.
        </p>
      </div>

      <div className="space-y-2">
        {models.map((m) => {
          const isSel = selected === m.id;
          const isDl = downloading === m.id;
          return (
            <div
              key={m.id}
              className={`rounded-lg border p-3 transition-colors ${
                isSel
                  ? "border-[color:var(--accent-color)] bg-[color:var(--accent-color)]/5"
                  : "border-border bg-bg-primary"
              }`}
            >
              <label className="flex cursor-pointer items-start gap-2">
                <input
                  type="radio"
                  name="bundled-embed-model"
                  checked={isSel}
                  onChange={() => setSelected(m.id)}
                  className="mt-0.5 h-3.5 w-3.5 accent-[color:var(--accent-color)]"
                />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium text-[color:var(--text-primary)]">
                      {m.name}
                    </span>
                    {m.installed ? (
                      <span className="flex items-center gap-0.5 rounded bg-emerald-950/40 px-1.5 py-0.5 text-[0.65rem] text-emerald-400">
                        <Check className="h-3 w-3" /> installed
                      </span>
                    ) : (
                      <span className="rounded bg-bg-tertiary px-1.5 py-0.5 text-[0.65rem] text-[color:var(--text-muted)]">
                        {m.size_mb} MB
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 text-[0.7rem] text-[color:var(--text-muted)]">
                    <code>{m.id}</code> · {m.dim}-dim
                  </div>
                </div>
              </label>
              {isDl && (
                <div className="mt-2">
                  <div className="mb-1 flex items-center justify-between text-[0.7rem] text-[color:var(--text-muted)]">
                    <span>Downloading…</span>
                    <span>{fmtPct(progress * 100)}%</span>
                  </div>
                  <div className="h-1.5 w-full overflow-hidden rounded-full bg-bg-tertiary">
                    <div
                      className="h-full rounded-full bg-[color:var(--accent-color)] transition-[width] duration-300"
                      style={{ width: `${Math.round(progress * 100)}%` }}
                    />
                  </div>
                </div>
              )}
              {!m.installed && !isDl && (
                <button
                  type="button"
                  onClick={() => void handleDownload(m.id)}
                  className="mt-2 flex items-center gap-1.5 rounded-lg border border-border px-2.5 py-1 text-xs text-[color:var(--text-primary)] hover:border-[color:var(--accent-color)]/50"
                >
                  <Download className="h-3.5 w-3.5" />
                  Download ({m.size_mb} MB)
                </button>
              )}
            </div>
          );
        })}
      </div>

      <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
        <input
          type="radio"
          name="bundled-embed-model"
          checked={selected === null}
          onChange={() => setSelected(null)}
          className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
        />
        <span>
          None (keyword-only)
          <span className="ml-1 text-[0.7rem] text-[color:var(--text-muted)]">
            — offline, no download, no semantic matching
          </span>
        </span>
      </label>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Embedding settings saved.
        </div>
      )}
      {saving && (
        <div className="text-xs text-[color:var(--text-muted)]">Saving…</div>
      )}
    </div>
  );
});
