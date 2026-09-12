// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useRef, useState, useCallback } from "react";
import { BarChart3, Coins, Cpu, Hash } from "lucide-react";
import { useAgentStore, aggregateTokPerSec } from "../../hooks/useAgentStore";
import {
  getSessionStats,
  getProjectStats,
  getSessionList,
  errMsg,
} from "../../lib/tauri";
import type {
  SessionStats,
  ProjectStats,
  SessionSummary,
  PricingEntry,
  ModelBreakdown,
} from "../../lib/tauri";
import { fmtTokens, fmtRate } from "../../lib/format";

/**
 * Right-panel Stats view — shows per-session + per-project token usage,
 * timing (tok/sec), cost estimates (from the [[pricing]] table), and a
 * per-model + per-day breakdown. The session card auto-refreshes from the
 * active agent's session in real time (re-fetches on every tokenUsage bump).
 *
 * Token counts and tok/s rates share the hybrid format from `lib/format`
 * (under 10K comma-grouped raw, 10K–1M "12.3K", ≥1M "1.2M", at most 1
 * decimal); request counts use comma-grouped integers, and costs use
 * `$X,XXX.XX` (sub-cent costs keep 4 decimals so a $0.0023 call isn't
 * rounded to $0.00).
 */

/** Format an integer count (requests/sessions) with US comma grouping. */
function fmtInt(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

/** Format a USD cost as `$X,XXX.XX` (e.g. "$6,000.00"). Sub-cent costs keep
 *  4 decimals so a $0.0023 call shows "$0.0023" instead of "$0.00". A cost of
 *  exactly $0 (no pricing entry, or genuinely zero) renders "—" so unpriced
 *  models don't clutter the table with "$0.0000". */
function fmtCost(usd: number): string {
  if (usd === 0) return "—";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

/** Format a unix timestamp (seconds) as a local date/time. */
function fmtTime(epochSec: number): string {
  return new Date(epochSec * 1000).toLocaleString();
}

/** Compute cost for a model breakdown using the pricing table. */
function modelCost(mb: ModelBreakdown, pricing: PricingEntry[]): number {
  const p = pricing.find((e) => e.model === mb.model);
  if (!p) return 0;
  // cached_tokens ⊆ prompt_tokens — billed at the cached rate; the rest at
  // the input rate. Reasoning tokens are completion tokens (output rate).
  const uncachedInput = mb.prompt_tokens - mb.cached_tokens;
  const output = mb.completion_tokens; // reasoning ⊆ completion
  return (
    (uncachedInput * p.input_per_1m) / 1_000_000 +
    (mb.cached_tokens * p.cached_per_1m) / 1_000_000 +
    (output * p.output_per_1m) / 1_000_000
  );
}

/** Per-model breakdown as a compact, horizontally-scrollable table with a
 *  totals row. Columns: Model | Endpoint | Reqs | Prompt | Compl | Reason |
 *  Cached | In/s | Out/s | Cost. One row per (model, endpoint) — the same
 *  model served by two endpoints renders two rows with their own rates;
 *  pre-endpoint rows (blank endpoint) group into one row per model. The
 *  totals row sums every numeric column across all rows and recomputes the
 *  aggregate In/s + Out/s from the summed timing totals (so a split where
 *  each row was slow doesn't understate the combined rate). */
function ModelTable({
  per_model,
  pricing,
}: {
  per_model: ModelBreakdown[];
  pricing: PricingEntry[];
}) {
  // Aggregate totals across all models (recomputed, not just summed rates).
  const tot = per_model.reduce(
    (acc, mb) => ({
      reqs: acc.reqs + mb.request_count,
      prompt: acc.prompt + mb.prompt_tokens,
      completion: acc.completion + mb.completion_tokens,
      reasoning: acc.reasoning + mb.reasoning_tokens,
      cached: acc.cached + mb.cached_tokens,
      ttft: acc.ttft + mb.ttft_ms_total,
      gen: acc.gen + mb.generation_ms_total,
      cost: acc.cost + modelCost(mb, pricing),
    }),
    { reqs: 0, prompt: 0, completion: 0, reasoning: 0, cached: 0, ttft: 0, gen: 0, cost: 0 },
  );
  const totIn = aggregateTokPerSec(tot.prompt, tot.ttft);
  const totOut = aggregateTokPerSec(tot.completion, tot.gen);

  const th = "text-right font-normal px-1 py-0.5 whitespace-nowrap";
  const td = "text-right font-mono tabular-nums px-1 py-0.5 text-slate-300 whitespace-nowrap";

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-[0.75em]">
        <thead className="text-slate-500">
          <tr>
            <th className="text-left font-normal px-1 py-0.5">Model</th>
            <th className="text-left font-normal px-1 py-0.5">Endpoint</th>
            <th className={th}>Reqs</th>
            <th className={th}>Prompt</th>
            <th className={th}>Compl</th>
            <th className={th}>Reason</th>
            <th className={th}>Cached</th>
            <th className={th}>In/s</th>
            <th className={th}>Out/s</th>
            <th className={th}>Cost</th>
          </tr>
        </thead>
        <tbody>
          {per_model.map((mb) => {
            const inRate = aggregateTokPerSec(mb.prompt_tokens, mb.ttft_ms_total);
            const outRate = aggregateTokPerSec(mb.completion_tokens, mb.generation_ms_total);
            // Key: (model, endpoint) pairs are unique per the SQL GROUP BY,
            // but naive concatenation can collide ("glm" + "5.3" vs
            // "glm5.3" + NULL) — the JSON form is collision-free.
            return (
              <tr key={JSON.stringify([mb.model, mb.endpoint])} className="border-t border-border/40">
                <td
                  className="text-left px-1 py-0.5 text-slate-300 max-w-[10em] truncate"
                  title={mb.model}
                >
                  {mb.model}
                </td>
                <td
                  className="text-left px-1 py-0.5 text-slate-400 max-w-[8em] truncate"
                  title={mb.endpoint ?? ""}
                >
                  {mb.endpoint ?? ""}
                </td>
                <td className={td}>{fmtInt(mb.request_count)}</td>
                <td className={td}>{fmtTokens(mb.prompt_tokens)}</td>
                <td className={td}>{fmtTokens(mb.completion_tokens)}</td>
                <td className={td}>{fmtTokens(mb.reasoning_tokens)}</td>
                <td className={td}>{fmtTokens(mb.cached_tokens)}</td>
                <td className={td}>{inRate !== null ? fmtRate(inRate) : "—"}</td>
                <td className={td}>{outRate !== null ? fmtRate(outRate) : "—"}</td>
                <td className={td}>{fmtCost(modelCost(mb, pricing))}</td>
              </tr>
            );
          })}
        </tbody>
        <tfoot>
          <tr className="border-t border-border font-semibold">
            <td colSpan={2} className="text-left px-1 py-0.5 text-slate-200">
              Total
            </td>
            <td className={td + " text-slate-200"}>{fmtInt(tot.reqs)}</td>
            <td className={td + " text-slate-200"}>{fmtTokens(tot.prompt)}</td>
            <td className={td + " text-slate-200"}>{fmtTokens(tot.completion)}</td>
            <td className={td + " text-slate-200"}>{fmtTokens(tot.reasoning)}</td>
            <td className={td + " text-slate-200"}>{fmtTokens(tot.cached)}</td>
            <td className={td + " text-slate-200"}>{totIn !== null ? fmtRate(totIn) : "—"}</td>
            <td className={td + " text-slate-200"}>{totOut !== null ? fmtRate(totOut) : "—"}</td>
            <td className={td + " text-slate-200"}>{fmtCost(tot.cost)}</td>
          </tr>
        </tfoot>
      </table>
    </div>
  );
}

/** A labeled stat row (icon + label + value). */
function StatRow({
  icon,
  label,
  value,
  title,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  title?: string;
}) {
  return (
    <div
      className="flex items-center justify-between border-b border-border/50 py-1 text-[0.85em] last:border-b-0"
      title={title}
    >
      <span className="flex items-center gap-1.5 text-slate-400">
        <span className="text-slate-500">{icon}</span>
        {label}
      </span>
      <span className="font-mono tabular-nums text-slate-200">{value}</span>
    </div>
  );
}

/** The session stats card — shows the active agent's current session. */
function SessionCard({ stats, pricing }: { stats: SessionStats | null; pricing: PricingEntry[] }) {
  if (!stats) {
    return (
      <div className="rounded border border-border bg-bg-tertiary p-2 text-[0.8em] text-slate-500">
        No active session yet. Send a prompt to start one.
      </div>
    );
  }
  const avgTtft = stats.timed_requests > 0 ? stats.ttft_ms_total / stats.timed_requests : null;
  const avgGen = stats.timed_requests > 0 ? stats.generation_ms_total / stats.timed_requests : null;
  const avgInputRate = aggregateTokPerSec(stats.prompt_tokens, stats.ttft_ms_total);
  const avgOutputRate = aggregateTokPerSec(stats.completion_tokens, stats.generation_ms_total);
  const cost = stats.per_model.reduce((sum, mb) => sum + modelCost(mb, pricing), 0);
  return (
    <div className="rounded border border-border bg-bg-tertiary p-2">
      <div className="mb-1.5 flex items-center gap-1.5 text-[0.8em] font-semibold text-slate-300">
        <Cpu className="h-3.5 w-3.5 text-cyan-400" />
        Session
      </div>
      <StatRow
        icon={<Hash className="h-3 w-3" />}
        label="Requests"
        value={fmtInt(stats.request_count)}
      />
      <StatRow
        icon={<span className="text-slate-500">↑</span>}
        label="Prompt"
        value={fmtTokens(stats.prompt_tokens)}
        title="Input tokens (includes cached)"
      />
      <StatRow
        icon={<span className="text-slate-500">↓</span>}
        label="Completion"
        value={fmtTokens(stats.completion_tokens)}
        title="Output tokens (includes reasoning)"
      />
      <StatRow
        icon={<span className="text-purple-400">🧠</span>}
        label="Reasoning"
        value={fmtTokens(stats.reasoning_tokens)}
      />
      <StatRow
        icon={<span className="text-amber-400">⚡</span>}
        label="Cached (est.)"
        value={fmtTokens(stats.cached_tokens)}
        title="Cached prompt tokens — API value when reported, else a client heuristic"
      />
      {avgInputRate !== null && (
        <StatRow
          icon={<span className="text-slate-500">↑</span>}
          label="In tok/s"
          value={`${fmtRate(avgInputRate)}`}
          title={`Input tok/sec = prompt / total TTFT (${avgTtft?.toFixed(0)}ms avg per req)`}
        />
      )}
      {avgOutputRate !== null && (
        <StatRow
          icon={<span className="text-slate-500">↓</span>}
          label="Out tok/s"
          value={`${fmtRate(avgOutputRate)}`}
          title={`Output tok/sec = completion / total generation (${avgGen?.toFixed(0)}ms avg per req)`}
        />
      )}
      {cost > 0 && (
        <StatRow
          icon={<Coins className="h-3 w-3" />}
          label="Cost"
          value={fmtCost(cost)}
          title="Estimated from [[pricing]] in endpoints.toml"
        />
      )}
      <div className="mt-1 text-[0.7em] text-slate-600">
        Started {fmtTime(stats.created_at)}
      </div>
      {/* Per-model breakdown */}
      {stats.per_model.length > 0 && (
        <div className="mt-2 border-t border-border pt-1.5">
          <div className="mb-1 text-[0.7em] font-medium uppercase tracking-wide text-slate-500">
            Per model
          </div>
          <ModelTable per_model={stats.per_model} pricing={pricing} />
        </div>
      )}
    </div>
  );
}

/** The project stats card — cumulative totals across all sessions. */
function ProjectCard({ stats, pricing }: { stats: ProjectStats | null; pricing: PricingEntry[] }) {
  if (!stats) return null;
  const cost = stats.per_model.reduce((sum, mb) => sum + modelCost(mb, pricing), 0);
  const avgInputRate = aggregateTokPerSec(stats.prompt_tokens, stats.ttft_ms_total);
  const avgOutputRate = aggregateTokPerSec(stats.completion_tokens, stats.generation_ms_total);
  return (
    <div className="rounded border border-border bg-bg-tertiary p-2">
      <div className="mb-1.5 flex items-center gap-1.5 text-[0.8em] font-semibold text-slate-300">
        <BarChart3 className="h-3.5 w-3.5 text-green-400" />
        Project (all sessions)
      </div>
      <StatRow icon={<Hash className="h-3 w-3" />} label="Sessions" value={fmtInt(stats.session_count)} />
      <StatRow icon={<Hash className="h-3 w-3" />} label="Requests" value={fmtInt(stats.request_count)} />
      <StatRow
        icon={<span className="text-slate-500">↑</span>}
        label="Prompt"
        value={fmtTokens(stats.prompt_tokens)}
      />
      <StatRow
        icon={<span className="text-slate-500">↓</span>}
        label="Completion"
        value={fmtTokens(stats.completion_tokens)}
      />
      <StatRow
        icon={<span className="text-purple-400">🧠</span>}
        label="Reasoning"
        value={fmtTokens(stats.reasoning_tokens)}
      />
      <StatRow
        icon={<span className="text-amber-400">⚡</span>}
        label="Cached (est.)"
        value={fmtTokens(stats.cached_tokens)}
      />
      {avgInputRate !== null && (
        <StatRow icon={<span className="text-slate-500">↑</span>} label="In tok/s" value={fmtRate(avgInputRate)} />
      )}
      {avgOutputRate !== null && (
        <StatRow icon={<span className="text-slate-500">↓</span>} label="Out tok/s" value={fmtRate(avgOutputRate)} />
      )}
      {cost > 0 && (
        <StatRow icon={<Coins className="h-3 w-3" />} label="Cost" value={fmtCost(cost)} />
      )}
      {/* Per-model breakdown */}
      {stats.per_model.length > 0 && (
        <div className="mt-2 border-t border-border pt-1.5">
          <div className="mb-1 text-[0.7em] font-medium uppercase tracking-wide text-slate-500">
            Per model
          </div>
          <ModelTable per_model={stats.per_model} pricing={pricing} />
        </div>
      )}
      {/* Per-day breakdown */}
      {stats.per_day.length > 0 && (
        <div className="mt-2 border-t border-border pt-1.5">
          <div className="mb-1 text-[0.7em] font-medium uppercase tracking-wide text-slate-500">
            Per day
          </div>
          {stats.per_day.map((d) => (
            <div key={d.day} className="flex items-center justify-between py-0.5 text-[0.78em]">
              <span className="text-slate-400">
                {new Date(d.day * 1000).toLocaleDateString()}
              </span>
              <span className="font-mono text-slate-500">
                {d.request_count} reqs · {fmtTokens(d.prompt_tokens + d.completion_tokens)} tok
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** The session list — all sessions with quick token totals. */
function SessionList({ sessions }: { sessions: SessionSummary[] }) {
  if (sessions.length === 0) return null;
  return (
    <div className="rounded border border-border bg-bg-tertiary p-2">
      <div className="mb-1.5 text-[0.8em] font-semibold text-slate-300">Sessions</div>
      <div className="max-h-48 overflow-y-auto">
        {sessions.map((s) => (
          <div
            key={s.session_id}
            className="flex items-center justify-between border-b border-border/50 py-1 text-[0.78em] last:border-b-0"
          >
            <span className="text-slate-400">{fmtTime(s.created_at)}</span>
            <span className="font-mono text-slate-500">
              {s.request_count}r · {fmtTokens(s.prompt_tokens)}↑ · {fmtTokens(s.completion_tokens)}↓
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

export function StatsView() {
  const activeAgent = useAgentStore((s) => s.activeAgent);
  const pricing = useAgentStore((s) => s.pricing);
  const tokenUsage = useAgentStore((s) =>
    s.activeAgent !== null ? s.agents[s.activeAgent]?.tokenUsage : undefined,
  );
  const [sessionStats, setSessionStats] = useState<SessionStats | null>(null);
  const [projectStats, setProjectStats] = useState<ProjectStats | null>(null);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [error, setError] = useState<string | null>(null);

  const refreshAll = useCallback(async () => {
    // Full refresh — project stats + session list (both change rarely) + the
    // active agent's session stats. Runs on mount and when the active agent
    // changes. A failure in either aggregate fetch is a real error (the memory
    // store is unavailable) → single banner. The session fetch failing just
    // means "no session yet" — a legitimate state, not an error.
    const [proj, list, sess] = await Promise.all([
      getProjectStats().catch((e) => {
        setError(errMsg(e));
        return null;
      }),
      getSessionList().catch((e) => {
        setError(errMsg(e));
        return [] as SessionSummary[];
      }),
      activeAgent !== null
        ? getSessionStats(activeAgent).catch(() => null)
        : Promise.resolve(null),
    ]);
    if (proj) setProjectStats(proj);
    setSessions(list);
    setSessionStats(sess);
  }, [activeAgent]);

  const refreshSession = useCallback(async () => {
    // Real-time, cheap refresh — only the active agent's session stats. The
    // project card + session list are aggregates over the whole store and
    // don't change per token bump, so they're NOT re-fetched here.
    if (activeAgent === null) return;
    try {
      const s = await getSessionStats(activeAgent);
      setSessionStats(s);
      setError(null);
    } catch {
      // No session yet — keep whatever we had (or null).
    }
  }, [activeAgent]);

  // Initial load + refresh when the active agent changes.
  useEffect(() => {
    void refreshAll();
  }, [refreshAll]);

  // Real-time refresh: re-fetch only the active agent's session stats whenever
  // its tokenUsage changes (each Usage event bumps it). The deps are the
  // *trigger*, not the full closure set — `refreshSession` is stable per
  // activeAgent and intentionally omitted. When the AGENT itself changed,
  // `refreshAll` (above) already fetched the new agent's stats, so skip the
  // duplicate fetch (the double-fetch-on-agent-switch regression).
  const lastUsageAgentRef = useRef<number | null>(null);
  useEffect(() => {
    if (lastUsageAgentRef.current !== activeAgent) {
      lastUsageAgentRef.current = activeAgent;
      return;
    }
    void refreshSession();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- fire only on tokenUsage fields; refreshSession/activeAgent intentionally omitted
  }, [tokenUsage?.prompt, tokenUsage?.completion, tokenUsage?.cached]);

  return (
    <div className="h-full overflow-y-auto p-2">
      <div className="space-y-2">
        {error && (
          <div className="rounded border border-red-600/40 bg-red-950/20 p-2 text-[0.8em] text-red-400">
            {error}
          </div>
        )}
        <SessionCard stats={sessionStats} pricing={pricing} />
        <ProjectCard stats={projectStats} pricing={pricing} />
        <SessionList sessions={sessions} />
        {pricing.length === 0 && (
          <div className="rounded border border-border p-2 text-[0.78em] text-slate-500">
            <Coins className="mr-1 inline h-3 w-3" />
            No [[pricing]] entries in endpoints.toml — cost estimates are unavailable.
            Add a <code className="text-slate-400">[[pricing]]</code> table keyed by model
            name with <code className="text-slate-400">input_per_1m</code>,{" "}
            <code className="text-slate-400">output_per_1m</code>, and{" "}
            <code className="text-slate-400">cached_per_1m</code> (in $ per 1M tokens).
          </div>
        )}
      </div>
    </div>
  );
}