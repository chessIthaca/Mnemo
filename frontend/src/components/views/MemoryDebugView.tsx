// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Brain, RefreshCw, Search, AlertTriangle, CheckCircle2, XCircle } from "lucide-react";
import { fmtPct } from "../../lib/format";
import {
  memoryDebugOverview,
  memoryDebugList,
  memoryDebugRecall,
  memoryDebugResolveLink,
  memoryDebugBacklinks,
  errMsg,
} from "../../lib/tauri";
import type {
  MemoryDebugOverview,
  MemoryDebugWire,
  ScoredMemoryDebugWire,
  ResolvedLink,
} from "../../lib/tauri";

/**
 * The Memory debug tab — surfaces the memory system's internals for debugging
 * (embedder status + model, stored vector fingerprints, per-tier counts, a
 * recall test box, and a browsable memory list). Mirrors the Trace tab's
 * architecture (polls a small IPC module over the shared `MemoryStore`).
 *
 * Three panels, top-to-bottom:
 * 1. Overview — embedder status badge, active model + dim, configured model,
 *    stored fingerprints with a match/mismatch indicator, per-tier counts.
 * 2. Recall test — a query box that runs a live recall and shows the scored
 *    results (tier + title + score + snippet).
 * 3. Memory browser — a tier dropdown that lists memories; clicking a row
 *    expands its full content + data JSON.
 */

/** Poll interval for the overview + list (ms). */
const POLL_MS = 2000;

/** The tier options for the browser dropdown. */
const TIERS = ["all", "working", "episodic", "semantic", "procedural"] as const;
type TierFilter = (typeof TIERS)[number];

/** Tier badge color by tier name. */
function tierColor(tier: string): string {
  switch (tier) {
    case "working":
      return "text-slate-400";
    case "episodic":
      return "text-blue-400";
    case "semantic":
      return "text-green-400";
    case "procedural":
      return "text-purple-400";
    default:
      return "text-slate-400";
  }
}

/** Format a unix timestamp (seconds) as a relative time (e.g. "3d ago"). */
function fmtRelative(epochSec: number): string {
  const now = Math.floor(Date.now() / 1000);
  const elapsed = now - epochSec;
  if (elapsed < 60) return "just now";
  if (elapsed < 3600) return `${Math.floor(elapsed / 60)}m ago`;
  if (elapsed < 86400) return `${Math.floor(elapsed / 3600)}h ago`;
  if (elapsed < 2592000) return `${Math.floor(elapsed / 86400)}d ago`;
  return new Date(epochSec * 1000).toLocaleDateString();
}

/** Render the embedder status as a colored badge + label. */
function StatusBadge({ status }: { status: MemoryDebugOverview["status"] }) {
  if (status && typeof status === "object" && "downloading" in status) {
    const d = (status as { downloading: { model: string; progress: number } }).downloading;
    return (
      <span className="flex items-center gap-1 text-cyan-400">
        <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-cyan-400" />
        Downloading {d.model} {fmtPct(d.progress * 100)}%
      </span>
    );
  }
  const s = status as string;
  if (s === "ready") {
    return (
      <span className="flex items-center gap-1 text-green-400">
        <span className="inline-block h-2 w-2 rounded-full bg-green-400" />
        Ready
      </span>
    );
  }
  if (s === "failed" || s === "fallback") {
    return (
      <span className="flex items-center gap-1 text-amber-400">
        <AlertTriangle className="h-3 w-3" />
        {s === "failed" ? "Failed" : "Fallback"}
      </span>
    );
  }
  if (s === "checking" || s === "pulling") {
    return (
      <span className="flex items-center gap-1 text-cyan-400">
        <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-cyan-400" />
        {s === "checking" ? "Checking" : "Pulling"}
      </span>
    );
  }
  return <span className="text-slate-500">{s || "unknown"}</span>;
}

/** The overview panel — embedder status, model, fingerprints, counts. */
function OverviewPanel({ overview }: { overview: MemoryDebugOverview | null }) {
  if (!overview) return null;
  const noStore = overview.model_id === "<none>";
  const c = overview.counts;
  return (
    <div className="rounded border border-border bg-bg-tertiary p-2">
      <div className="mb-1.5 flex items-center gap-1.5 text-[0.8em] font-semibold text-slate-300">
        <Brain className="h-3.5 w-3.5 text-cyan-400" />
        Embedder
      </div>
      {noStore ? (
        <div className="text-[0.78em] text-slate-500">
          Memory store not initialized.
        </div>
      ) : (
        <>
          <div className="flex items-center justify-between border-b border-border/50 py-1 text-[0.85em]">
            <span className="text-slate-400">Status</span>
            <StatusBadge status={overview.status} />
          </div>
          <div className="flex items-center justify-between border-b border-border/50 py-1 text-[0.85em]">
            <span className="text-slate-400">Model</span>
            <span className="font-mono text-slate-200">{overview.model_id}</span>
          </div>
          <div className="flex items-center justify-between border-b border-border/50 py-1 text-[0.85em]">
            <span className="text-slate-400">Dimension</span>
            <span className="font-mono text-slate-200">{overview.dim}</span>
          </div>
          <div className="flex items-center justify-between border-b border-border/50 py-1 text-[0.85em]">
            <span className="text-slate-400">Configured</span>
            <span className="font-mono text-slate-200">
              {overview.configured_model ?? "(none / hash)"}
            </span>
          </div>
          {/* Fingerprint match indicator */}
          <div className="flex items-center justify-between border-b border-border/50 py-1 text-[0.85em]">
            <span className="text-slate-400">Fingerprints</span>
            {overview.fingerprint_matches ? (
              <span className="flex items-center gap-1 text-green-400">
                <CheckCircle2 className="h-3 w-3" />
                Match
              </span>
            ) : (
              <span className="flex items-center gap-1 text-red-400" title="Stored vectors are in a different space — recall is degraded. A re-embed is pending or needed.">
                <XCircle className="h-3 w-3" />
                Mismatch
              </span>
            )}
          </div>
          {overview.fingerprints.length > 0 && (
            <div className="mt-1 space-y-0.5">
              {overview.fingerprints.map(([m, d], i) => (
                <div
                  key={`${m}/${d}/${i}`}
                  className={`flex items-center justify-between text-[0.72em] ${
                    m === overview.model_id && d === overview.dim
                      ? "text-green-400"
                      : "text-slate-500"
                  }`}
                >
                  <span className="font-mono">{m}</span>
                  <span className="font-mono">{d}d</span>
                </div>
              ))}
            </div>
          )}
        </>
      )}
      {/* Per-tier counts */}
      <div className="mt-2 border-t border-border pt-1.5">
        <div className="mb-1 text-[0.7em] font-medium uppercase tracking-wide text-slate-500">
          Counts
        </div>
        <div className="grid grid-cols-2 gap-1 text-[0.78em]">
          <CountRow label="Working" n={c.working} color="text-slate-400" />
          <CountRow label="Episodic" n={c.episodic} color="text-blue-400" />
          <CountRow label="Semantic" n={c.semantic} color="text-green-400" />
          <CountRow label="Procedural" n={c.procedural} color="text-purple-400" />
        </div>
        <div className="mt-1 flex items-center justify-between border-t border-border/50 pt-1 text-[0.78em] font-semibold text-slate-300">
          <span>Total</span>
          <span className="font-mono">{c.total}</span>
        </div>
      </div>
    </div>
  );
}

/** A single count row (label + number). */
function CountRow({
  label,
  n,
  color,
}: {
  label: string;
  n: number;
  color: string;
}) {
  return (
    <div className="flex items-center justify-between">
      <span className={color}>{label}</span>
      <span className="font-mono text-slate-300">{n}</span>
    </div>
  );
}

/** A memory row in the browser list — click to expand detail. */
function MemoryRow({ m }: { m: MemoryDebugWire }) {
  const [expanded, setExpanded] = useState(false);
  // Knowledge rows carry their `[[wiki-link]]` list in `data.links` (the
  // indexer stores it) — surfaced as clickable chips in the expanded view.
  const links = useMemo(() => {
    if (!m.data || typeof m.data !== "object") return [];
    const raw = (m.data as Record<string, unknown>).links;
    if (!Array.isArray(raw)) return [];
    return raw.filter(
      (l): l is { target: string; kind?: string } =>
        !!l && typeof l === "object" && typeof (l as { target?: unknown }).target === "string",
    );
  }, [m.data]);
  return (
    <div className="border-b border-border/40">
      <button
        onClick={() => setExpanded((v) => !v)}
        className="flex w-full items-start gap-1.5 px-2 py-1 text-left text-[0.78em] transition-colors hover:bg-bg-tertiary"
      >
        <span className={`mt-0.5 shrink-0 font-medium uppercase ${tierColor(m.tier)}`}>
          {m.tier.slice(0, 3)}
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <span className="truncate text-slate-200">{m.title}</span>
            <span className="shrink-0 font-mono text-[0.85em] text-slate-600">
              {fmtRelative(m.last_accessed_at)}
            </span>
          </div>
          <div className="truncate text-[0.85em] text-slate-500">{m.content}</div>
        </div>
        <span className="shrink-0 font-mono text-[0.85em] text-slate-600">
          ×{m.access_count}
        </span>
      </button>
      {expanded && (
        <div className="space-y-1 px-2 pb-2 text-[0.75em]">
          <div className="flex flex-wrap gap-x-3 gap-y-0.5 text-slate-500">
            <span>strength: <span className="font-mono text-slate-400">{m.strength.toFixed(2)}</span></span>
            <span>created: <span className="font-mono text-slate-400">{fmtRelative(m.created_at)}</span></span>
            <span>id: <span className="font-mono text-slate-600">{m.id.slice(0, 8)}</span></span>
            {m.source_session_ids.length > 0 && (
              <span>sessions: <span className="font-mono text-slate-600">{m.source_session_ids.length}</span></span>
            )}
          </div>
          <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-bg-secondary p-1.5 text-[0.85em] text-slate-300">
            {m.content}
          </pre>
          {m.data != null && (
            <pre className="max-h-32 overflow-auto whitespace-pre-wrap break-words rounded bg-bg-secondary p-1.5 text-[0.85em] text-slate-400">
              {JSON.stringify(m.data, null, 2)}
            </pre>
          )}
          {links.length > 0 && (
            <div className="flex flex-wrap gap-1 pt-1">
              {links.map((l, i) => (
                <LinkChip key={i} target={l.target} />
              ))}
            </div>
          )}
          {m.data != null &&
            typeof m.data === "object" &&
            typeof (m.data as Record<string, unknown>).rel_path === "string" && (
              <Backlinks rel={String((m.data as Record<string, unknown>).rel_path)} />
            )}
        </div>
      )}
    </div>
  );
}

/** One `[[wiki-link]]` chip — resolves the target on click (a point lookup +
 *  id math in the backend) and reveals the file / focuses the record. */
function LinkChip({ target }: { target: string }) {
  const [info, setInfo] = useState<ResolvedLink | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const click = async () => {
    try {
      const r = await memoryDebugResolveLink(target);
      setErr(null);
      setInfo(r);
    } catch (e) {
      setErr(errMsg(e));
    }
  };
  return (
    <span className="inline-flex items-center gap-1">
      <button
        onClick={() => void click()}
        className="rounded border border-cyan-500/30 bg-cyan-500/10 px-1.5 py-0.5 font-mono text-[0.85em] text-cyan-300 transition-colors hover:bg-cyan-500/20"
        title={`Resolve [[${target}]]`}
      >
        [[{target}]]
      </button>
      {err && <span className="text-red-400">{err}</span>}
      {info && (
        <span className="text-slate-400">
          {info.title ? (
            <>
              → <span className="text-slate-300">{info.title}</span>
              <span className="text-slate-600"> ({info.kind})</span>
            </>
          ) : (
            <>
              → <span className="text-slate-500">{info.rel_path ?? info.target}</span>
              <span className="text-slate-600"> ({info.kind})</span>
            </>
          )}
        </span>
      )}
    </span>
  );
}

/** The "referenced by" reverse surface: every memory whose links point at
 *  this record's file. */
function Backlinks({ rel }: { rel: string }) {
  const [rows, setRows] = useState<MemoryDebugWire[] | null>(null);
  useEffect(() => {
    let cancelled = false;
    memoryDebugBacklinks(rel)
      .then((r) => {
        if (!cancelled) setRows(r);
      })
      .catch(() => {
        if (!cancelled) setRows([]);
      });
    return () => {
      cancelled = true;
    };
  }, [rel]);
  if (rows === null) return null;
  if (rows.length === 0) return null;
  return (
    <div className="pt-1">
      <div className="mb-0.5 text-[0.85em] uppercase tracking-wide text-slate-600">
        Referenced by
      </div>
      {rows.map((r) => (
        <div key={r.id} className="truncate text-[0.85em] text-slate-400">
          <span className={`font-medium uppercase ${tierColor(r.tier)}`}>{r.tier.slice(0, 3)}</span>{" "}
          {r.title}
        </div>
      ))}
    </div>
  );
}

/** A scored recall result row. */
function RecallRow({ r }: { r: ScoredMemoryDebugWire }) {
  return (
    <div className="border-b border-border/40 px-2 py-1 text-[0.78em]">
      <div className="flex items-center gap-1.5">
        <span className={`shrink-0 font-medium uppercase ${tierColor(r.tier)}`}>
          {r.tier.slice(0, 3)}
        </span>
        <span className="truncate text-slate-200">{r.title}</span>
        <span className="ml-auto shrink-0 font-mono text-cyan-400">
          {r.score.toFixed(3)}
        </span>
      </div>
      <div className="truncate text-[0.85em] text-slate-500">{r.content}</div>
    </div>
  );
}

export function MemoryDebugView() {
  const [overview, setOverview] = useState<MemoryDebugOverview | null>(null);
  const [memories, setMemories] = useState<MemoryDebugWire[]>([]);
  const [tierFilter, setTierFilter] = useState<TierFilter>("all");
  const [recallQuery, setRecallQuery] = useState("");
  const [recallResults, setRecallResults] = useState<ScoredMemoryDebugWire[] | null>(null);
  const [recallLoading, setRecallLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The last-seen overview counts signature — each poll compares against it
  // to detect mid-session writes (null = no baseline yet).
  const lastCountsSig = useRef<string | null>(null);

  // Fetch the memory list — shared by the tier-filter effect, the manual
  // refresh button, and the overview tick's counts-change piggyback.
  const loadList = useCallback(
    async (isCancelled: () => boolean = () => false) => {
      try {
        const tier = tierFilter === "all" ? null : tierFilter;
        const list = await memoryDebugList(tier, 200);
        if (isCancelled()) return;
        setMemories(list);
        setError(null);
      } catch (e) {
        if (!isCancelled()) setError(errMsg(e));
      }
    },
    [tierFilter],
  );

  // Poll the overview every POLL_MS while mounted (the view only mounts when
  // the tab is active). Mirrors LlmTraceView's polling pattern.
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const ov = await memoryDebugOverview();
        if (cancelled) return;
        setOverview(ov);
        // Piggyback a list re-fetch when a tier count changed: records
        // written mid-session — by the agent or a consolidation — otherwise
        // do not appear until a manual refresh. The first poll only sets the
        // baseline (the tier-filter effect already fetched on mount).
        const sig = JSON.stringify(ov.counts);
        if (lastCountsSig.current !== null && lastCountsSig.current !== sig) {
          void loadList();
        }
        lastCountsSig.current = sig;
        setError(null);
      } catch (e) {
        if (!cancelled) setError(errMsg(e));
      }
    };
    void tick();
    const timer = setInterval(tick, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [loadList]);

  // Fetch the memory list whenever the tier filter changes.
  useEffect(() => {
    let cancelled = false;
    void loadList(() => cancelled);
    return () => {
      cancelled = true;
    };
  }, [loadList]);

  // Run a recall test.
  const runRecall = async () => {
    const q = recallQuery.trim();
    if (!q) return;
    setRecallLoading(true);
    setRecallResults(null);
    try {
      const results = await memoryDebugRecall(q, 10);
      setRecallResults(results);
      setError(null);
    } catch (e) {
      // Keep recallResults as null (not []) so the results box stays hidden —
      // an error is not "no matches". The error banner above conveys the failure.
      setError(errMsg(e));
      setRecallResults(null);
    } finally {
      setRecallLoading(false);
    }
  };

  const refreshList = async () => {
    await loadList();
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-2 py-1.5">
        <div className="flex items-center gap-1.5">
          <Brain className="h-3.5 w-3.5 text-cyan-400" />
          <span className="text-xs font-semibold text-slate-200">Memory</span>
        </div>
        <button
          onClick={refreshList}
          className="flex items-center gap-1 rounded bg-bg-tertiary px-1.5 py-0.5 text-[0.65em] font-medium text-slate-400 transition-colors hover:text-slate-200"
          title="Refresh the memory list"
        >
          <RefreshCw className="h-3 w-3" /> Refresh
        </button>
      </div>

      {error && (
        <div className="border-b border-red-600/40 bg-red-950/20 px-2 py-1 text-[0.7em] text-red-400">
          {error}
        </div>
      )}

      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto p-2">
        {/* Overview panel */}
        <OverviewPanel overview={overview} />

        {/* Recall test panel */}
        <div className="rounded border border-border bg-bg-tertiary p-2">
          <div className="mb-1.5 flex items-center gap-1.5 text-[0.8em] font-semibold text-slate-300">
            <Search className="h-3.5 w-3.5 text-cyan-400" />
            Recall test
          </div>
          <div className="flex gap-1.5">
            <input
              type="text"
              value={recallQuery}
              onChange={(e) => setRecallQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void runRecall();
              }}
              placeholder="Query the memory store…"
              className="min-w-0 flex-1 rounded border border-border bg-bg-secondary px-2 py-1 text-[0.8em] text-slate-200 placeholder:text-slate-600 focus:border-cyan-500/50 focus:outline-none"
            />
            <button
              onClick={() => void runRecall()}
              disabled={recallLoading || !recallQuery.trim()}
              className="flex items-center gap-1 rounded bg-cyan-500/20 px-2 py-1 text-[0.75em] font-medium text-cyan-300 transition-colors hover:bg-cyan-500/30 disabled:opacity-40"
            >
              {recallLoading ? "…" : "Recall"}
            </button>
          </div>
          {recallResults !== null && (
            <div className="mt-1.5 max-h-48 overflow-y-auto rounded border border-border/50">
              {recallResults.length === 0 ? (
                <div className="px-2 py-1.5 text-[0.78em] text-slate-500">
                  No matches.
                </div>
              ) : (
                recallResults.map((r) => (
                  <RecallRow key={r.id} r={r} />
                ))
              )}
            </div>
          )}
        </div>

        {/* Memory browser panel */}
        <div className="rounded border border-border bg-bg-tertiary p-2">
          <div className="mb-1.5 flex items-center justify-between">
            <span className="text-[0.8em] font-semibold text-slate-300">
              Memories
            </span>
            <select
              value={tierFilter}
              onChange={(e) => setTierFilter(e.target.value as TierFilter)}
              className="rounded border border-border bg-bg-secondary px-1.5 py-0.5 text-[0.72em] text-slate-300 focus:border-cyan-500/50 focus:outline-none"
            >
              {TIERS.map((t) => (
                <option key={t} value={t}>
                  {t === "all" ? "All tiers" : t.charAt(0).toUpperCase() + t.slice(1)}
                </option>
              ))}
            </select>
          </div>
          <div className="max-h-64 overflow-y-auto rounded border border-border/50">
            {memories.length === 0 ? (
              <div className="px-2 py-1.5 text-[0.78em] text-slate-500">
                No memories in this tier.
              </div>
            ) : (
              memories.map((m) => <MemoryRow key={m.id} m={m} />)
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
