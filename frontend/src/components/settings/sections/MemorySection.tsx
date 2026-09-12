// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useState } from "react";
import { Brush, Database, RefreshCw, SearchCode } from "lucide-react";
import { fmtPct } from "../../../lib/format";
import {
  errMsg,
  codegraphRebuildIndex,
  getSettings,
  maintenanceCleanup,
  maintenanceRebuildIndex,
  maintenanceRebuildSearch,
  memoryDebugOverview,
  memoryIndexStatus,
  onMaintenanceEvent,
  onReindexEvent,
  saveSettings,
  type MaintenanceOp,
  type MemoryDebugOverview,
  type MemoryIndexStatus,
} from "../../../lib/tauri";
import type { SettingsSectionHandle } from "../types";
import {
  applyMaintenanceEvent,
  idleOp,
  subscribeUntilDisposed,
  type OpState,
} from "./maintenanceUi";

/** The four action cards in this section: three memory ops + the code index. */
type CardOp = MaintenanceOp | "reindex";

/** The editable `[memory]` retrieval knobs (the section's draft state). */
interface MemoryKnobs {
  decayHalfLifeDays: number;
  perQueryCap: number;
  derivedPerClassCap: number;
}

/** Dirty-tracking serialization for the knobs draft (the ChatSection pattern). */
const serializeKnobs = (k: MemoryKnobs) => JSON.stringify(k);

/**
 * Memory & Search section — the manual care tools for the memory store and
 * the code search index, each a button with a live progress bar:
 *
 * - **Clean up memory**: consolidates finished sessions that still hold raw
 *   working-tier events into episodic summaries (plus distilled facts when an
 *   LLM endpoint is configured), deletes the raw rows, and compacts the
 *   database. Live agents' sessions are skipped (they consolidate at their
 *   own session end).
 * - **Rebuild semantic search**: re-embeds every memory with the active
 *   embedding model, rebuilds the FTS5 index, and compacts the database —
 *   the recovery path when stored vectors are stale/mixed or the index has
 *   drifted. (Memories, not code.)
 * - **Rebuild code search index**: re-parses the project's source files and
 *   rebuilds the symbols + full-text content index behind the agent's code
 *   search tools — the recovery path when code search looks stale (e.g.
 *   after a large import with the watcher off).
 * - **Rebuild derived index**: re-scans `.coding/plans`, `.coding/reviews`,
 *   and the pending backlog into budgeted digest memories (the Phase 2
 *   derived index) — authored memories are never touched. The card shows
 *   authored/derived counts + the last-built timestamp.
 *
 * All run in the backend as background tasks; this section renders the
 * `memory://maintenance` and `codegraph://maintenance` event streams
 * (started → progress → done/failed) via the pure {@link applyMaintenanceEvent}
 * mapping. The header shows per-tier counts + the active embedding model,
 * and warns when stored vectors don't match the active model (rebuild
 * recommended). The retrieval scale knobs (decay half-life, per-query cap,
 * derived per-class cap) edit the `[memory]` config section via the draft/
 * snapshot pattern — applied live on Save (no restart).
 */
export const MemorySection = forwardRef<
  SettingsSectionHandle,
  {
    active: boolean;
    onDirtyChange?: (dirty: boolean) => void;
  }
>(function MemorySection({ active, onDirtyChange }, ref) {
  const [overview, setOverview] = useState<MemoryDebugOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [cleanup, setCleanup] = useState<OpState>(idleOp());
  const [rebuild, setRebuild] = useState<OpState>(idleOp());
  const [reindex, setReindex] = useState<OpState>(idleOp());
  const [index, setIndex] = useState<OpState>(idleOp());
  const [indexStatus, setIndexStatus] = useState<MemoryIndexStatus | null>(null);
  // The [memory] retrieval knobs — draft/snapshot like ChatSection: edits
  // are local until Save, which patches config.toml via save_settings (the
  // backend pushes them into the live store snapshot — no restart).
  const [knobs, setKnobs] = useState<MemoryKnobs | null>(null);
  const [knobSnapshot, setKnobSnapshot] = useState("");
  const [budgetsLine, setBudgetsLine] = useState<string | null>(null);
  const [savingKnobs, setSavingKnobs] = useState(false);
  const [knobError, setKnobError] = useState<string | null>(null);
  const [knobsSaved, setKnobsSaved] = useState(false);
  // A start-command failure (no store wired / op already running) — distinct
  // from an op's own failure, which arrives as a `failed` event.
  const [startError, setStartError] = useState<string | null>(null);
  const busy =
    cleanup.running || rebuild.running || reindex.running || index.running;

  async function load() {
    setLoading(true);
    setLoadError(null);
    try {
      const [ov, status, settings] = await Promise.all([
        memoryDebugOverview(),
        memoryIndexStatus(),
        getSettings(),
      ]);
      setOverview(ov);
      setIndexStatus(status);
      const k: MemoryKnobs = {
        decayHalfLifeDays: settings.memory.decay_half_life_days,
        perQueryCap: settings.memory.per_query_cap,
        derivedPerClassCap: settings.memory.derived_per_class_cap,
      };
      setKnobs(k);
      setKnobSnapshot(serializeKnobs(k));
      setBudgetsLine(
        `PLAN ${settings.memory.plan_budget} · BUG ${settings.memory.bug_budget} · SPEC ${settings.memory.spec_budget} · DECISION ${settings.memory.decision_budget} · REVIEW ${settings.memory.review_budget} chars`,
      );
    } catch (e) {
      setLoadError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  // Load the overview when the section becomes active, and again after an op
  // terminates (success OR failure — counts + the fingerprint-mismatch
  // warning should reflect the post-op state either way).
  useEffect(() => {
    if (active) void load();
  }, [active, cleanup.summary, cleanup.error, rebuild.summary, rebuild.error, index.summary, index.error]);

  // Subscribe for the whole mounted lifetime (not just while visible): an op
  // started here keeps streaming while the user browses other sections, and
  // the terminal done/failed event always lands. subscribeUntilDisposed
  // guards the resolve-after-unmount window so no listener leaks (review B1).
  useEffect(() => {
    return subscribeUntilDisposed(onMaintenanceEvent, (event) => {
      // A fresh start clears any stale start-command error (e.g. a rejected
      // double-click) so it doesn't linger beside the running bar (C2).
      if (event.type === "started") setStartError(null);
      const setter: (fn: (prev: OpState) => OpState) => void =
        event.op === "cleanup"
          ? setCleanup
          : event.op === "rebuild"
            ? setRebuild
            : setIndex;
      setter((prev) => applyMaintenanceEvent(prev, event));
    });
  }, []);

  // The code-index rebuild stream — the same fold, routed to the third card.
  useEffect(() => {
    return subscribeUntilDisposed(onReindexEvent, (event) => {
      if (event.type === "started") setStartError(null);
      setReindex((prev) => applyMaintenanceEvent(prev, event));
    });
  }, []);

  // The knobs draft is dirty when it differs from the last-loaded/saved
  // snapshot — reported to the dialog shell (confirm-discard on close).
  const knobsDirty = knobs !== null && serializeKnobs(knobs) !== knobSnapshot;
  useEffect(() => {
    onDirtyChange?.(knobsDirty);
  }, [knobsDirty, onDirtyChange]);

  // Expose save to the dialog shell (OK button): persists the [memory]
  // knobs via save_settings (the ops cards persist nothing — they stream).
  useImperativeHandle(ref, () => ({
    save: async () => {
      if (!knobs || !knobsDirty) return true;
      setSavingKnobs(true);
      setKnobError(null);
      setKnobsSaved(false);
      try {
        await saveSettings({
          memory: {
            decay_half_life_days: knobs.decayHalfLifeDays,
            per_query_cap: knobs.perQueryCap,
            derived_per_class_cap: knobs.derivedPerClassCap,
          },
        });
        setKnobSnapshot(serializeKnobs(knobs));
        setKnobsSaved(true);
        window.setTimeout(() => setKnobsSaved(false), 2500);
        return true;
      } catch (e) {
        setKnobError(errMsg(e));
        return false;
      } finally {
        setSavingKnobs(false);
      }
    },
  }));

  async function start(op: CardOp) {
    setStartError(null);
    try {
      if (op === "cleanup") {
        await maintenanceCleanup();
      } else if (op === "rebuild") {
        await maintenanceRebuildSearch();
      } else if (op === "index") {
        await maintenanceRebuildIndex();
      } else {
        await codegraphRebuildIndex();
      }
      // Progress + results arrive via the event subscription; do NOT reset
      // the bar here — the `started` event owns the lifecycle.
    } catch (e) {
      setStartError(errMsg(e));
    }
  }

  /** One action card: description + button + live progress bar + result. */
  function renderOp(
    op: CardOp,
    state: OpState,
    title: string,
    description: string,
    buttonLabel: string,
    Icon: typeof Brush,
  ) {
    // total = 0 → label-only phase (reindexing/vacuuming): indeterminate bar.
    // 1-decimal precision; rendered via fmtPct (backlog 9042b47c).
    const pct =
      state.total > 0
        ? Math.round((state.done / state.total) * 1000) / 10
        : null;
    return (
      <div className="rounded-lg border border-border bg-bg-primary p-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex items-center gap-2 text-sm font-medium text-[color:var(--text-primary)]">
              <Icon className="h-4 w-4 shrink-0" aria-hidden />
              {title}
            </div>
            <p className="mt-1 text-xs text-[color:var(--text-muted)]">
              {description}
            </p>
          </div>
          <button
            type="button"
            onClick={() => void start(op)}
            disabled={busy}
            className="shrink-0 rounded-lg border border-border px-2.5 py-1 text-xs text-[color:var(--text-primary)] hover:border-[color:var(--accent-color)]/50 disabled:opacity-40"
          >
            {state.running ? "Running…" : buttonLabel}
          </button>
        </div>
        {state.running && (
          <div className="mt-2" aria-live="polite">
            <div className="mb-1 flex items-center justify-between text-[0.7rem] text-[color:var(--text-muted)]">
              <span className="capitalize">{state.phase || "working"}…</span>
              <span>{pct === null ? "" : `${fmtPct(pct)}%`}</span>
            </div>
            <div className="h-1.5 w-full overflow-hidden rounded-full bg-bg-tertiary">
              <div
                className={`h-full rounded-full bg-[color:var(--accent-color)] transition-[width] duration-300${
                  pct === null ? " w-full animate-pulse" : ""
                }`}
                style={pct === null ? undefined : { width: `${pct}%` }}
              />
            </div>
          </div>
        )}
        {state.summary && (
          <div className="mt-2 rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
            {state.summary}
          </div>
        )}
        {state.error && (
          <div className="mt-2 rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {state.error}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Memory & Search
        </h3>
        <p className="text-xs text-[color:var(--text-muted)]">
          Manual care for the project memory store and the code search index.
          All tools run in the background — watch the progress bar; the agent
          keeps working meanwhile.
        </p>
      </div>

      {loading ? (
        <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
          Loading memory overview…
        </div>
      ) : loadError ? (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {loadError}
        </div>
      ) : (
        overview && (
          <div className="space-y-1 rounded-lg border border-border bg-bg-primary p-3 text-xs text-[color:var(--text-muted)]">
            <div>
              working {overview.counts.working} · episodic{" "}
              {overview.counts.episodic} · semantic {overview.counts.semantic}{" "}
              · procedural {overview.counts.procedural} · total{" "}
              {overview.counts.total}
            </div>
            <div>
              embedding model: <code>{overview.model_id}</code>
              {overview.dim > 0 ? ` (${overview.dim}-dim)` : ""}
            </div>
            {overview.counts.total > 0 && !overview.fingerprint_matches && (
              <div className="rounded bg-amber-500/10 px-2 py-1 text-[0.7rem] text-amber-300">
                Stored vectors don&apos;t match the active embedding model —
                semantic recall is degraded. Rebuild recommended.
              </div>
            )}
          </div>
        )
      )}

      {startError && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {startError}
        </div>
      )}

      {renderOp(
        "cleanup",
        cleanup,
        "Clean up memory",
        "Consolidates finished sessions that still hold raw working events into summaries (+ distilled facts when an LLM endpoint is configured), removes the raw rows, and compacts the database. Live agents' sessions are left alone.",
        "Clean up now",
        Brush,
      )}
      {renderOp(
        "rebuild",
        rebuild,
        "Rebuild semantic search",
        "Re-embeds every memory with the active embedding model, rebuilds the full-text index, and compacts the database — the fix for stale or mismatched search results.",
        "Rebuild now",
        RefreshCw,
      )}
      {renderOp(
        "index",
        index,
        "Rebuild derived index",
        "Re-scans the on-disk truth — .coding/plans, .coding/reviews, .coding/knowledge/** (typed SPEC/DECISION/BUG/HOW records), and the pending backlog — into budgeted digest memories the agent can recall. The DBs are caches; the files are the truth, so deleting .coding/memory.db also forces this rebuild. Authored memories are never touched." +
          (indexStatus
            ? ` Currently: ${indexStatus.authored_count} authored · ${indexStatus.derived_count} derived${
                indexStatus.last_built_at
                  ? ` · last built ${new Date(indexStatus.last_built_at * 1000).toLocaleString()}`
                  : " · never built"
              }.`
            : ""),
        "Rebuild now",
        Database,
      )}
      {renderOp(
        "reindex",
        reindex,
        "Rebuild code search index",
        "Re-parses the project's source files and rebuilds the symbols + full-text content index behind the agent's code search — the fix when code search looks stale (e.g. after a large import with the watcher off). Memories are not touched.",
        "Rebuild now",
        SearchCode,
      )}

      {knobs && (
        <div className="rounded-lg border border-border bg-bg-primary p-3">
          <div className="text-sm font-medium text-[color:var(--text-primary)]">
            Retrieval scale knobs
          </div>
          <p className="mt-1 text-xs text-[color:var(--text-muted)]">
            Tuned for large memory corpora — applied live on Save (no restart).
            Digest budgets are fixed per record type: {budgetsLine}.
          </p>
          <div className="mt-2 grid grid-cols-3 gap-3">
            <div className="space-y-1">
              <label
                className="text-xs text-[color:var(--text-primary)]"
                htmlFor="mem-decay"
              >
                Decay half-life (days)
              </label>
              <input
                id="mem-decay"
                type="number"
                min={0.1}
                max={365}
                step={0.1}
                value={knobs.decayHalfLifeDays}
                onChange={(e) => {
                  const v = Number.parseFloat(e.target.value);
                  if (Number.isFinite(v)) {
                    setKnobs((prev) =>
                      prev ? { ...prev, decayHalfLifeDays: v } : prev,
                    );
                  }
                }}
                className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
            <div className="space-y-1">
              <label
                className="text-xs text-[color:var(--text-primary)]"
                htmlFor="mem-query-cap"
              >
                Per-query result cap
              </label>
              <input
                id="mem-query-cap"
                type="number"
                min={1}
                max={500}
                step={1}
                value={knobs.perQueryCap}
                onChange={(e) => {
                  const v = Number.parseInt(e.target.value, 10);
                  if (Number.isFinite(v)) {
                    setKnobs((prev) =>
                      prev ? { ...prev, perQueryCap: v } : prev,
                    );
                  }
                }}
                className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
            <div className="space-y-1">
              <label
                className="text-xs text-[color:var(--text-primary)]"
                htmlFor="mem-class-cap"
              >
                Derived per-class cap
              </label>
              <input
                id="mem-class-cap"
                type="number"
                min={1}
                max={100}
                step={1}
                value={knobs.derivedPerClassCap}
                onChange={(e) => {
                  const v = Number.parseInt(e.target.value, 10);
                  if (Number.isFinite(v)) {
                    setKnobs((prev) =>
                      prev ? { ...prev, derivedPerClassCap: v } : prev,
                    );
                  }
                }}
                className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </div>
          </div>
          {knobError && (
            <div className="mt-2 rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
              {knobError}
            </div>
          )}
          {knobsSaved && (
            <div className="mt-2 text-xs text-emerald-300">Knobs saved.</div>
          )}
          {savingKnobs && (
            <div className="mt-2 text-xs text-[color:var(--text-muted)]">
              Saving…
            </div>
          )}
        </div>
      )}
    </div>
  );
});
