// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useCallback, useEffect, useRef, useState } from "react";
import { useAgentStore } from "../../hooks/useAgentStore";
import { errMsg, getProjectStats, getSavingsStats } from "../../lib/tauri";
import type { PricingEntry, ProjectStats, SavingsStats } from "../../lib/tauri";
import { fmtTokens } from "../../lib/format";

/**
 * Right-panel Dashboard view (backlog 652ae094): what Mnemo's context-economy
 * levers actually saved, per project.
 *
 * The metered figures come from the `savings_events` ledger the optimizer
 * levers write (backlog e4a50d22) — deliberately with NO dollar value on them,
 * because the ledger counts tokens, not money. The one dollar figure, in the
 * cache section, is ESTIMATED from the `[[pricing]]` table at this project's
 * own realized input/cached rates (see `rateEstimate`); it is always rendered
 * apart from the metered totals and never summed into them.
 *
 * Token counts share `fmtTokens` with the Stats view (under 10K comma-grouped,
 * 10K–1M "12.3K", ≥1M "1.2M").
 */

/** How often the Dashboard re-reads the savings ledger while its tab is open —
 *  the same 2 s cadence `PlanProgress` polls its plan file at. */
const POLL_MS = 2000;

/** Format an integer count (events/requests) with US comma grouping. Mirrors
 *  StatsView's helper so the two views read identically. */
function fmtInt(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

/** Format a USD figure as `$X,XXX.XX`; sub-cent keeps 4 decimals, and an
 *  exact $0 renders "—" (no pricing entry) rather than a misleading "$0.00".
 *  Mirrors StatsView's helper. */
function fmtCost(usd: number): string {
  if (usd === 0) return "—";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

/** Format a unix timestamp (seconds) as a local date/time. */
function fmtTime(epochSec: number): string {
  return new Date(epochSec * 1000).toLocaleString();
}

/** Format the ledger's epoch-DAY bucket (`created_at / 86400`) as a date. */
function fmtDay(epochDay: number): string {
  return new Date(epochDay * 86_400_000).toLocaleDateString();
}

/**
 * This project's realized prompt rates, in dollars per token: what an uncached
 * input token actually cost, and what a cached one did. Derived from the
 * per-model request totals joined to the `[[pricing]]` table, so the Dashboard's
 * estimate is priced at the rates this project really paid instead of assuming
 * one particular model.
 */
function rateEstimate(project: ProjectStats, pricing: PricingEntry[]) {
  let uncachedCost = 0;
  let cachedCost = 0;
  let uncachedTokens = 0;
  let cachedTokens = 0;
  let matched = 0;
  for (const mb of project.per_model) {
    const p = pricing.find((e) => e.model === mb.model);
    if (!p) continue;
    matched += 1;
    const uncached = Math.max(0, mb.prompt_tokens - mb.cached_tokens);
    uncachedCost += (uncached * p.input_per_1m) / 1_000_000;
    cachedCost += (mb.cached_tokens * p.cached_per_1m) / 1_000_000;
    uncachedTokens += uncached;
    cachedTokens += mb.cached_tokens;
  }
  return {
    inputPerToken: uncachedTokens > 0 ? uncachedCost / uncachedTokens : 0,
    cachedPerToken: cachedTokens > 0 ? cachedCost / cachedTokens : 0,
    // How many of the project's models the pricing table actually covers. Zero
    // with a NON-empty table means every figure below is "—" and the note must
    // say so rather than claim a derivation.
    matched,
  };
}

/**
 * The Dashboard view — per-project token savings (backlog 652ae094).
 */
export function DashboardView() {
  const pricing = useAgentStore((s) => s.pricing);
  const [savings, setSavings] = useState<SavingsStats | null>(null);
  const [project, setProject] = useState<ProjectStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      // The ledger drives every metered figure; the project stats only supply
      // the realized rates the ESTIMATED dollar figure is priced at.
      const [ledger, proj] = await Promise.all([getSavingsStats(), getProjectStats()]);
      setSavings(ledger);
      setProject(proj);
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Live update while the tab is open (user report 2027-01-25): the ledger
  // grows as the agent runs, so the mount-once fetch left the view stale until
  // the tab was closed and reopened. Poll on PlanProgress's cadence, skip a
  // tick while the window is hidden or a fetch is still in flight, and refresh
  // at once when the app regains focus. Closing the panel or switching tabs
  // unmounts this view (App.tsx gates RightPanel on `rightPanelVisible`, and
  // RightPanel renders only the active tab's component), so the interval is
  // bounded to this tab being open.
  const inFlight = useRef(false);
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      if (cancelled || inFlight.current || document.hidden) return;
      inFlight.current = true;
      try {
        await refresh();
      } finally {
        inFlight.current = false;
      }
    };
    void tick();
    const interval = setInterval(() => void tick(), POLL_MS);
    const onFocus = () => void tick();
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      cancelled = true;
      clearInterval(interval);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount-once poll; refresh intentionally omitted
  }, []);

  if (loading) {
    return <div className="p-3 text-xs text-slate-500">Loading savings…</div>;
  }
  if (error) {
    return <div className="p-3 text-xs text-red-400">{error}</div>;
  }
  if (!savings || !project) {
    return <div className="p-3 text-xs text-slate-500">No savings data yet.</div>;
  }
  return <DashboardBody savings={savings} project={project} pricing={pricing} />;
}

/**
 * The Dashboard's presentational body, exported so the node-env render test can
 * pin it with fixtures — the fetch wrapper above resolves through `useEffect`,
 * which never runs under `renderToStaticMarkup`.
 *
 * `savings` and `project` are both non-optional: the wrapper only renders this
 * once both loaded.
 */
export function DashboardBody({
  savings,
  project,
  pricing,
}: {
  savings: SavingsStats;
  project: ProjectStats;
  pricing: PricingEntry[];
}) {
  const rates = rateEstimate(project, pricing);
  // What the avoided context tokens would have cost as uncached input.
  const estimatedSaved = Math.max(0, savings.saved_tokens_total) * rates.inputPerToken;
  // What caching saved: the cached share of the prompt served below the input
  // rate rather than at it.
  const cacheSavedTokens = savings.cache.cached_tokens;
  const cacheSaved =
    cacheSavedTokens * Math.max(0, rates.inputPerToken - rates.cachedPerToken);
  const hitRate =
    savings.cache.cached_not_null_requests > 0 && savings.cache.prompt_tokens > 0
      ? (cacheSavedTokens / savings.cache.prompt_tokens) * 100
      : 0;
  const maxDay = savings.per_day.reduce((m, d) => Math.max(m, d.saved_tokens), 0);
  const card = "rounded border border-slate-700 bg-slate-800/40 p-2";
  const th = "text-right font-normal px-1 py-0.5 whitespace-nowrap";
  const td = "text-right font-mono tabular-nums px-1 py-0.5 text-slate-300 whitespace-nowrap";

  return (
    <div className="space-y-2 p-2 text-xs" data-testid="dashboard-view">
      <div className="grid grid-cols-2 gap-2">
        <div className={card}>
          <div className="text-slate-500">tokens saved</div>
          <div className="font-mono text-base text-slate-100">
            {fmtTokens(savings.saved_tokens_total)}
          </div>
          <div className="text-slate-600">metered from the savings ledger</div>
        </div>
        <div className={card}>
          <div className="text-slate-500">events</div>
          <div className="font-mono text-base text-slate-100">{fmtInt(savings.event_count)}</div>
          <div className="text-slate-600">optimization events recorded</div>
        </div>
      </div>

      {savings.event_count === 0 ? (
        <div className={card}>
          <div className="text-slate-400">No savings recorded yet.</div>
          <div className="mt-1 text-slate-600">
            Turn on context-economy levers in Settings → Savings (or{" "}
            <code>[general.optimizer]</code> in <code>config.toml</code>), and Mnemo
            will meter what they save here.
          </div>
        </div>
      ) : null}

      <div className={card} data-testid="dashboard-per-kind">
        <div className="mb-1 text-slate-500">by kind</div>
        <table className="w-full">
          <thead className="text-slate-500">
            <tr>
              <th className="text-left font-normal px-1 py-0.5">kind</th>
              <th className={th}>events</th>
              <th className={th}>tokens saved</th>
            </tr>
          </thead>
          <tbody>
            {savings.per_kind.map((k) => (
              <tr key={k.kind}>
                <td className="px-1 py-0.5 font-mono text-slate-300">{k.kind}</td>
                <td className={td}>{fmtInt(k.event_count)}</td>
                <td className={td}>{fmtTokens(k.saved_tokens)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className={card} data-testid="dashboard-per-day">
        <div className="mb-1 text-slate-500">per day</div>
        {savings.per_day.length === 0 ? (
          <div className="text-slate-600">no daily data yet</div>
        ) : (
          <div className="space-y-0.5">
            {savings.per_day.map((d) => (
              <div key={d.day} className="flex items-center gap-2">
                <span className="w-20 shrink-0 text-slate-500">{fmtDay(d.day)}</span>
                <span className="h-2 grow rounded bg-slate-700/40">
                  <span
                    className="block h-2 rounded bg-emerald-500/60"
                    style={{
                      width:
                        maxDay > 0 ? `${Math.max(0, (d.saved_tokens / maxDay) * 100)}%` : "0%",
                    }}
                  />
                </span>
                <span className="w-16 shrink-0 text-right font-mono tabular-nums text-slate-300">
                  {fmtTokens(d.saved_tokens)}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className={card} data-testid="dashboard-recent">
        <div className="mb-1 text-slate-500">recent events</div>
        {savings.recent.length === 0 ? (
          <div className="text-slate-600">nothing recorded yet</div>
        ) : (
          <div className="space-y-0.5">
            {savings.recent.map((e) => (
              <div key={e.id} className="flex items-baseline gap-2">
                <span className="w-32 shrink-0 text-slate-500">{fmtTime(e.created_at)}</span>
                <span className="shrink-0 font-mono text-slate-300">{e.kind}</span>
                <span className="grow truncate text-slate-500" title={e.detail ?? undefined}>
                  {e.detail ?? "—"}
                </span>
                <span className="shrink-0 font-mono tabular-nums text-slate-400">
                  {fmtTokens(e.tokens_before)} → {fmtTokens(e.tokens_after)}
                </span>
                <span className="w-16 shrink-0 text-right font-mono tabular-nums text-emerald-300">
                  {fmtTokens(e.tokens_saved)}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className={card}>
        <div className="mb-1 text-slate-500">prompt cache</div>
        <div className="flex justify-between">
          <span className="text-slate-500">prompt tokens</span>
          <span className="font-mono tabular-nums text-slate-300">
            {fmtTokens(savings.cache.prompt_tokens)}
          </span>
        </div>
        <div className="flex justify-between">
          <span className="text-slate-500">cached tokens</span>
          <span className="font-mono tabular-nums text-slate-300">{fmtTokens(cacheSavedTokens)}</span>
        </div>
        <div className="flex justify-between">
          <span className="text-slate-500">hit rate</span>
          <span className="font-mono tabular-nums text-slate-300">{hitRate.toFixed(1)}%</span>
        </div>
        <div className="flex justify-between">
          <span className="text-slate-500">requests reporting cache</span>
          <span className="font-mono tabular-nums text-slate-300">
            {fmtInt(savings.cache.cached_not_null_requests)} / {fmtInt(savings.cache.request_count)}
          </span>
        </div>
        <div className="mt-1 border-t border-slate-700 pt-1">
          <div className="flex justify-between">
            <span className="text-slate-500">
              estimated savings <span className="text-amber-400/80">ESTIMATED</span>
            </span>
            <span className="font-mono tabular-nums text-amber-300">{fmtCost(estimatedSaved)}</span>
          </div>
          <div className="flex justify-between">
            <span className="text-slate-500">
              estimated cache savings <span className="text-amber-400/80">ESTIMATED</span>
            </span>
            <span className="font-mono tabular-nums text-amber-300">{fmtCost(cacheSaved)}</span>
          </div>
          <div className="mt-0.5 text-slate-600">
            {pricing.length === 0
              ? "No [[pricing]] entries in endpoints.toml — dollar estimates are unavailable."
              : rates.matched === 0
                ? "No [[pricing]] entries for the models this project used — dollar estimates are unavailable."
                : "Priced at this project's realized input/cached rates; never part of the metered totals above."}
          </div>
        </div>
      </div>
    </div>
  );
}
