// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// The Git tab — a graphical view over the repository's branches and commit
// history, with a per-branch "Merge to main" action.
//
// Layout: a header (current branch, counts, refresh, transient merge
// result), an SVG commit DAG (main's first-parent spine on lane 0,
// predominant in the accent color; feature branches on side lanes — the
// lane math lives in the pure `gitLanes` module), a selected-commit detail
// row, and a branch list offering the confirmation-gated merge.
//
// Graph affordances:
// - BASE indication: each branch's fork point (nearest main ancestor,
//   `computeBranchBases`) shows as a muted "⟋ <branch>" pill on the base
//   commit row and an "off <sha>" label in the branch list.
// - SPINE COLLAPSE: main is limited to its newest ~20 commits
//   (`SPINE_COLLAPSE_LIMIT`) with an expand/collapse control; side-branch
//   commits always stay visible.
// - LIVE REFRESH: successful agent git mutations (git tool commit/merge/
//   checkout/branch, shell git verbs, create_plan's branch fork — counted
//   by `countGitMutations` over every agent's transcript) trigger a
//   debounced reload, so the graph updates as the agent works.
// - SOLID TIPS: the tip commit of every branch (and main) renders as a
//   filled solid circle in its lane color.
//
// The merge offer reuses the merge_to_main skill: confirming the dialog
// enters the skill on the active agent with a branch-specific prompt (see
// [`mergePrompt`]), so the agent itself drives checkout/merge/conflict
// resolution/build verification/branch cleanup — with git merge staying
// approval-gated inside the skill. Offered only while the active agent's
// workflow is Complete or Planning (same gate as the status-bar button).

import { useEffect, useMemo, useRef, useState } from "react";
import { GitBranch, GitMerge, RefreshCw } from "lucide-react";

import { useAgentStore } from "../../hooks/useAgentStore";
import {
  errMsg,
  enterSkill,
  gitHistory,
  type GitCommitInfo,
  type GitHistory,
} from "../../lib/tauri";
import { MergeToMainDialog } from "../layout/MergeToMainDialog";
import {
  computeBranchBases,
  computeLanes,
  countGitMutationsCached,
  laneColor,
  shortDate,
  SPINE_COLLAPSE_LIMIT,
  visibleCommits,
} from "./gitLanes";

/** Vertical distance between commit rows (px). */
const ROW_H = 26;
/** Horizontal distance between lanes (px). */
const LANE_W = 28;
/** Left margin before lane 0 (px). */
const MARGIN_X = 14;
/** Width of the row label column after the lanes (px). */
const LABEL_W = 320;
/** Max subject chars shown per row before ellipsis. */
const SUBJECT_CAP = 48;
/** Horizontal reserve for the right-aligned date label ("Aug 20" + gap). */
const DATE_RESERVE = 46;

/**
 * The branch-specific goal sent to the merge_to_main skill via enterSkill —
 * mirrors the skill file's own steps, but names the target branch so the
 * agent can merge ANY branch, not just the current one. Pure (exported for
 * tests): no store, no IPC.
 */
export function mergePrompt(branch: string): string {
  return `Merge branch '${branch}' into main and clean up. Steps:
1. git status — if the working tree is dirty, commit uncommitted work on the current branch with a clear message (stash only if it belongs to another branch).
2. If the current branch is not '${branch}', git checkout ${branch} first.
3. git checkout main, then sync main with origin FIRST via shell (the git tool has no fetch/pull): git fetch origin + git pull --no-rebase (nothing new or no remote → continue; conflicts here too: resolve with file_edit taking the UNION of both sides, then git add + git commit to conclude the pull merge — never drop a side's change).
4. git merge --no-ff ${branch} — use the git tool, never shell: the git tool forces the core-operation approval prompt, shell does not.
5. If there are merge conflicts, resolve them with file_edit taking the UNION of both sides' fixes, then git add + git commit — never drop a side's change; never use -X ours / -X theirs.
6. VERIFY THE MERGED TREE BUILDS before cleanup: cd frontend; npm run build AND cd src-tauri; cargo build (the root cargo build never compiles the bin crate). After the merge also run root cargo test and frontend npm test, unpiped, reading $LASTEXITCODE. If anything fails, fix with file_edit and re-run until green.
7. Once main has the merge commit AND everything is green, delete the branch with git branch -d ${branch} (never -D).
8. Call skill_end to return to Planning. Do NOT push.`;
}

/** Row Y for the i-th commit (commits are newest-first). */
function rowY(i: number): number {
  return i * ROW_H + ROW_H / 2;
}

/** Lane X for a lane index. */
function laneX(lane: number): number {
  return MARGIN_X + lane * LANE_W;
}

/** Truncate a subject for the row label to `cap` chars (defaults to the
 *  row budget; callers shrink it to fit around pills + the date). */
function trunc(s: string, cap: number = SUBJECT_CAP): string {
  return s.length > cap ? `${s.slice(0, cap - 1)}…` : s;
}

/**
 * The Git tab body. Degrades gracefully: outside a repository (or any git
 * failure) the error message renders as a notice with a retry, never a
 * crash.
 */
export function GitView() {
  const [history, setHistory] = useState<GitHistory | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [mergeTarget, setMergeTarget] = useState<string | null>(null);
  const [merging, setMerging] = useState(false);
  const [mergeResult, setMergeResult] = useState<{ ok: boolean; text: string } | null>(null);
  /** Whether main's spine is fully shown (true) or collapsed to the newest
   *  SPINE_COLLAPSE_LIMIT commits (false). */
  const [expanded, setExpanded] = useState(false);
  const resultTimer = useRef<number | null>(null);
  const refreshTimer = useRef<number | null>(null);

  const activeAgent = useAgentStore((s) => s.activeAgent);
  const workflowState = useAgentStore((s) =>
    s.activeAgent != null ? s.workflowStates[s.activeAgent] : undefined,
  );
  /** Same gate as the status-bar merge button: the skill exists only in
   *  Complete/Planning, and there must be an agent to enter it on. */
  const canMerge =
    activeAgent !== null && (workflowState === "complete" || workflowState === "planning");

  /** Load (or reload) the dataset. Errors surface as the notice state. */
  async function refresh() {
    setRefreshing(true);
    try {
      setHistory(await gitHistory());
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setRefreshing(false);
    }
  }

  useEffect(() => {
    void refresh();
    return () => {
      if (resultTimer.current !== null) window.clearTimeout(resultTimer.current);
      if (refreshTimer.current !== null) window.clearTimeout(refreshTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- load once on mount
  }, []);

  /** Confirm the dialog → enter the merge skill with the branch prompt. */
  async function handleMergeConfirm() {
    if (activeAgent === null || mergeTarget === null) return;
    setMerging(true);
    try {
      await enterSkill(activeAgent, "merge_to_main", mergePrompt(mergeTarget));
      setMergeResult({
        ok: true,
        text: `Merge skill started — the agent is driving the merge of ${mergeTarget}.`,
      });
      setMergeTarget(null);
      // The DAG goes stale while the agent merges; reload shortly after
      // (tracked so an unmount can cancel it).
      if (refreshTimer.current !== null) window.clearTimeout(refreshTimer.current);
      refreshTimer.current = window.setTimeout(() => void refresh(), 4000);
    } catch (e) {
      setMergeResult({ ok: false, text: errMsg(e) });
    } finally {
      setMerging(false);
      if (resultTimer.current !== null) window.clearTimeout(resultTimer.current);
      resultTimer.current = window.setTimeout(() => setMergeResult(null), 6000);
    }
  }

  // Lane layout — recomputed only when the dataset changes.
  const layout = useMemo(
    () => (history ? computeLanes(history.commits, new Set(history.main_spine)) : null),
    [history],
  );
  const laneBySha = useMemo(() => {
    const m = new Map<string, { lane: number; isSpine: boolean }>();
    const spine = new Set(history?.main_spine ?? []);
    history?.commits.forEach((c, i) => {
      const lc = layout?.commits[i];
      if (lc) m.set(c.sha, { lane: lc.lane, isSpine: spine.has(c.sha) });
    });
    return m;
  }, [history, layout]);
  /** Branches pointing at each sha (for the tip pills), in listed order. */
  const branchesAt = useMemo(() => {
    const m = new Map<string, string[]>();
    history?.branches.forEach((b) => {
      const list = m.get(b.tip_sha) ?? [];
      list.push(b.name);
      m.set(b.tip_sha, list);
    });
    return m;
  }, [history]);
  /** main's spine as a set (shared by the view slice + base math). */
  const spineSet = useMemo(() => new Set(history?.main_spine ?? []), [history]);
  /** The render-time slice: collapsed main (newest SPINE_COLLAPSE_LIMIT
   *  spine commits) + every side commit, or everything when expanded. */
  const view = useMemo(
    () => (history ? visibleCommits(history.commits, spineSet, expanded) : null),
    [history, spineSet, expanded],
  );
  /** sha → index within the visible slice (connector geometry). */
  const visibleIdx = useMemo(() => {
    const m = new Map<string, number>();
    view?.visible.forEach((c, i) => m.set(c.sha, i));
    return m;
  }, [view]);
  /** Tips of every branch (incl. main) — rendered as solid filled dots. */
  const tipShas = useMemo(
    () => new Set(history?.branches.map((b) => b.tip_sha) ?? []),
    [history],
  );
  const commitBySha = useMemo(() => {
    const m = new Map<string, GitCommitInfo>();
    history?.commits.forEach((c) => m.set(c.sha, c));
    return m;
  }, [history]);
  /** branch → its base (fork point on main). */
  const bases = useMemo(
    () =>
      history
        ? computeBranchBases(history.commits, spineSet, history.branches)
        : new Map<string, string>(),
    [history, spineSet],
  );
  /** base sha → OPEN branch names based there (the "⟋ name" DAG pills). */
  const openBasePillsAt = useMemo(() => {
    const m = new Map<string, string[]>();
    history?.branches.forEach((b) => {
      if (b.is_main || b.merged_into_main) return;
      const base = bases.get(b.name);
      if (!base) return;
      const list = m.get(base) ?? [];
      list.push(b.name);
      m.set(base, list);
    });
    return m;
  }, [history, bases]);

  // Live refresh: count successful git-mutation tool calls across ALL agents
  // (subagents merge/commit too) and reload the dataset when the count moves.
  // The selector returns a primitive so unrelated store changes don't
  // re-render the tab; countGitMutationsCached memoizes per transcript
  // identity (review L4) so the per-notify reduce is O(agents), not
  // O(all transcripts); the 600ms debounce coalesces commit→checkout bursts.
  const gitMutations = useAgentStore((s) =>
    Object.values(s.agents).reduce((n, a) => n + countGitMutationsCached(a.transcript), 0),
  );
  const lastMutations = useRef(gitMutations);
  useEffect(() => {
    if (gitMutations === lastMutations.current) return;
    lastMutations.current = gitMutations;
    if (refreshTimer.current !== null) window.clearTimeout(refreshTimer.current);
    refreshTimer.current = window.setTimeout(() => void refresh(), 600);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only the counter drives the reload
  }, [gitMutations]);

  const selectedCommit: GitCommitInfo | undefined = useMemo(
    () => history?.commits.find((c) => c.sha === selected),
    [history, selected],
  );

  // ── Error / loading states ─────────────────────────────────────────────
  if (error) {
    return (
      <div className="flex h-full flex-col gap-2 p-4 text-[0.875em] text-slate-400">
        <GitBranch className="h-5 w-5 text-slate-500" />
        <p className="text-red-400">{error}</p>
        <button
          onClick={() => void refresh()}
          className="w-fit rounded bg-bg-tertiary px-2 py-1 text-xs hover:text-slate-200"
        >
          Retry
        </button>
      </div>
    );
  }
  if (!history || !layout) {
    return (
      <div className="flex h-full items-center justify-center text-[0.875em] text-slate-500">
        <GitBranch className="mr-2 h-4 w-4 animate-pulse" /> loading git history…
      </div>
    );
  }

  // ── Graph geometry ─────────────────────────────────────────────────────
  const labelX = MARGIN_X + layout.laneCount * LANE_W + 10;
  const svgW = labelX + LABEL_W;
  const openBranches = history.branches.filter((b) => !b.is_main && !b.merged_into_main);
  /** Whether the expand/collapse control row renders (adds one row height). */
  const hasExpandRow =
    view !== null &&
    (view.hiddenSpineCount > 0 || (expanded && spineSet.size > SPINE_COLLAPSE_LIMIT));
  const svgH = (view!.visible.length + (hasExpandRow ? 1 : 0)) * ROW_H + 10;

  return (
    <div className="flex h-full flex-col">
      {/* Header: current branch + counts + refresh + transient merge result. */}
      <div className="flex items-center gap-2 border-b border-border px-3 py-2 text-[0.8em]">
        <GitBranch className="h-3.5 w-3.5 shrink-0 text-purple-400" />
        <span className="font-medium text-purple-400">{history.current_branch}</span>
        <span className="text-slate-500">
          · {view!.visible.length}
          {view!.hiddenSpineCount > 0 ? ` of ${history.commits.length}` : ""} commits ·{" "}
          {openBranches.length} open
        </span>
        <span className="ml-auto flex items-center gap-2 text-slate-500">
          <span className="flex items-center gap-1">
            <span className="inline-block h-2 w-2 rounded-full bg-[color:var(--accent-color)]" />
            main
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block h-2 w-2 rounded-full bg-[#a78bfa]" />
            branches
          </span>
        </span>
        <button
          onClick={() => void refresh()}
          aria-label="Refresh git history"
          title="Refresh"
          className="text-slate-500 transition-colors hover:text-slate-300"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${refreshing ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Transient merge result (6s, StatusBar pattern). */}
      {mergeResult && (
        <div
          className={`truncate border-b border-border px-3 py-1 text-[0.75em] ${
            mergeResult.ok ? "text-green-400" : "text-red-400"
          }`}
          title={mergeResult.text}
        >
          {mergeResult.text}
        </div>
      )}

      {/* The DAG. */}
      <div className="min-h-0 flex-1 overflow-auto">
        <svg width={svgW} height={svgH} role="img" aria-label="Commit history graph">
          {/* Connectors first (under the dots). Each edge takes the PARENT's
              lane color — a merge edge visually belongs to the branch lane it
              flows into. Parents outside the visible slice (beyond the cap OR
              hidden by the spine collapse) get a clipped downward stub. */}
          {view!.visible.map((c, i) => {
            const from = laneBySha.get(c.sha);
            if (!from) return null;
            return c.parents.map((p, pi) => {
              const pj = visibleIdx.get(p) ?? -1;
              const key = `${p}-${i}-${pi}`;
              if (pj === -1) {
                // Parent not visible: stub toward the bottom, clipped.
                return (
                  <path
                    key={key}
                    d={`M ${laneX(from.lane)} ${rowY(i)} L ${laneX(from.lane)} ${svgH}`}
                    stroke={laneColor(from.lane, from.isSpine)}
                    strokeWidth={from.isSpine ? 2.5 : 1.5}
                    fill="none"
                    strokeDasharray="2 2"
                    opacity={0.5}
                  />
                );
              }
              const to = laneBySha.get(p);
              if (!to) return null;
              const x1 = laneX(from.lane);
              const y1 = rowY(i);
              const x2 = laneX(to.lane);
              const y2 = rowY(pj);
              const mid = (y1 + y2) / 2;
              return (
                <path
                  key={key}
                  d={`M ${x1} ${y1} C ${x1} ${mid}, ${x2} ${mid}, ${x2} ${y2}`}
                  stroke={laneColor(to.lane, to.isSpine)}
                  strokeWidth={to.isSpine ? 2.5 : 2}
                  fill="none"
                />
              );
            });
          })}
          {/* Commit rows: dots + (pills) + subject + date. */}
          {view!.visible.map((c, i) => {
            const info = laneBySha.get(c.sha);
            if (!info) return null;
            const isSel = selected === c.sha;
            const isTip = tipShas.has(c.sha);
            const tips = branchesAt.get(c.sha) ?? [];
            const baseNames = openBasePillsAt.get(c.sha) ?? [];
            const color = laneColor(info.lane, info.isSpine);
            let pillX = labelX;
            /** One pill at the running x offset. `muted` renders the base
             *  marker style (slate text, no border) vs. the tip-pill style. */
            const pill = (label: string, muted: boolean, key: string) => {
              const w = label.length * 6.2 + 16;
              const el = (
                <g key={key} transform={`translate(${pillX}, ${rowY(i) - 8})`}>
                  <rect
                    width={w}
                    height={16}
                    rx={4}
                    fill="var(--bg-tertiary)"
                    stroke={!muted && info.isSpine ? color : "transparent"}
                    strokeWidth={!muted && info.isSpine ? 1.2 : 0}
                  />
                  <text x={8} y={11.5} fontSize={10} fill={muted ? "#94a3b8" : info.isSpine ? color : "#cbd5e1"}>
                    {label}
                  </text>
                </g>
              );
              pillX += w + 4;
              return el;
            };
            const pills = [
              ...tips.map((name) => pill(name, false, `t-${name}`)),
              // Base markers: which OPEN branches fork at this commit.
              ...baseNames.map((name) => pill(`⟋ ${name}`, true, `b-${name}`)),
            ];
            // Fit the subject between the pills and the right-aligned date
            // (pills scale with branch-name length) — ~6px/char at font 10.
            const subjectX = pillX + 2;
            const subjectCap = Math.min(
              SUBJECT_CAP,
              Math.max(8, Math.floor((svgW - DATE_RESERVE - subjectX) / 6)),
            );
            return (
              <g
                key={c.sha}
                onClick={() => setSelected(isSel ? null : c.sha)}
                style={{ cursor: "pointer" }}
              >
                {isSel && <circle cx={laneX(info.lane)} cy={rowY(i)} r={9} fill={color} opacity={0.25} />}
                {/* Branch/main TIP commits render as filled solid circles in
                    their lane color; spine commits stay filled, side commits
                    stay hollow rings. */}
                {isTip ? (
                  <circle
                    cx={laneX(info.lane)}
                    cy={rowY(i)}
                    r={5.5}
                    fill={color}
                    stroke={color}
                    strokeWidth={0}
                  />
                ) : (
                  <circle
                    cx={laneX(info.lane)}
                    cy={rowY(i)}
                    r={info.isSpine ? 5.5 : 4.5}
                    fill={info.isSpine ? color : "var(--bg-secondary)"}
                    stroke={color}
                    strokeWidth={info.isSpine ? 0 : 2}
                  />
                )}
                {pills}
                <text
                  x={subjectX}
                  y={rowY(i) + 3.5}
                  fontSize={10}
                  fill={info.isSpine ? "#e2e8f0" : "#94a3b8"}
                >
                  {trunc(c.subject, subjectCap)}
                </text>
                <text
                  x={svgW - 6}
                  y={rowY(i) + 3.5}
                  fontSize={9}
                  textAnchor="end"
                  fill="#64748b"
                >
                  {shortDate(c.timestamp)}
                </text>
              </g>
            );
          })}
          {/* Expand/collapse main's spine: collapsed hides everything older
              than the newest SPINE_COLLAPSE_LIMIT spine commits. */}
          {hasExpandRow && (
            <g
              onClick={() => setExpanded((e) => !e)}
              style={{ cursor: "pointer" }}
            >
              <rect
                x={labelX - 4}
                y={rowY(view!.visible.length) - 10}
                width={280}
                height={16}
                fill="transparent"
              />
              <text
                x={labelX}
                y={rowY(view!.visible.length) + 3.5}
                fontSize={10}
                fill="#38bdf8"
              >
                {view!.hiddenSpineCount > 0
                  ? `▸ show ${view!.hiddenSpineCount} more on main (expand)`
                  : `▾ collapse main to newest ${SPINE_COLLAPSE_LIMIT}`}
              </text>
            </g>
          )}
        </svg>
      </div>

      {/* Selected-commit detail row. */}
      {selectedCommit && (
        <div className="border-t border-border px-3 py-1.5 text-[0.75em] text-slate-400">
          <span className="font-mono text-[color:var(--accent-color)]">
            {selectedCommit.short_sha}
          </span>{" "}
          <span className="text-slate-300">{selectedCommit.subject}</span>{" "}
          <span className="text-slate-500">
            — {selectedCommit.author},{" "}
            {new Date(selectedCommit.timestamp * 1000).toLocaleString()}
          </span>
          {selectedCommit.refs.length > 0 && (
            <span className="ml-2 text-purple-400">{selectedCommit.refs.join(", ")}</span>
          )}
        </div>
      )}

      {/* Branch list with the merge offers. */}
      <div className="max-h-[38%] shrink-0 overflow-y-auto border-t border-border">
        {history.branches.map((b) => (
          <div
            key={b.name}
            className="flex items-center gap-2 px-3 py-1.5 text-[0.8em] hover:bg-bg-tertiary"
          >
            <GitBranch
              className={`h-3 w-3 shrink-0 ${b.is_main ? "text-[color:var(--accent-color)]" : "text-slate-500"}`}
            />
            <span
              className={
                b.is_main
                  ? "font-medium text-[color:var(--accent-color)]"
                  : b.is_current
                    ? "font-medium text-purple-400"
                    : "text-slate-300"
              }
            >
              {b.name}
            </span>
            <span className="font-mono text-[0.85em] text-slate-500">
              {b.tip_sha.slice(0, 7)}
            </span>
            {/* The branch's BASE (fork point on main), when resolvable. */}
            {bases.has(b.name) && (
              <span
                className="text-[0.85em] text-slate-500"
                title={`branched off ${
                  commitBySha.get(bases.get(b.name)!)?.subject ?? "main"
                }`}
              >
                off {commitBySha.get(bases.get(b.name)!)?.short_sha ?? bases.get(b.name)!.slice(0, 7)}
              </span>
            )}
            {b.upstream_track && (
              <span className="text-[0.85em] text-slate-500">{b.upstream_track}</span>
            )}
            <span className="ml-auto flex items-center gap-2">
              <span
                className={`text-[0.85em] ${b.is_main ? "text-slate-500" : b.merged_into_main ? "text-slate-500" : "text-amber-400/80"}`}
              >
                {b.is_main ? "—" : b.merged_into_main ? "✓ merged" : "open"}
              </span>
              {!b.is_main && (
                <button
                  onClick={() => setMergeTarget(b.name)}
                  disabled={!canMerge}
                  title={
                    canMerge
                      ? `Merge '${b.name}' into main via the merge_to_main skill (confirmation-gated)`
                      : "Merging is available once the active agent's workflow is Complete or Planning (no plan mid-flight)"
                  }
                  className={`flex items-center gap-1 rounded px-1.5 py-0.5 text-[0.85em] font-medium transition-colors ${
                    canMerge
                      ? "text-cyan-400 hover:bg-bg-tertiary"
                      : "cursor-not-allowed text-cyan-400/40"
                  }`}
                >
                  <GitMerge className="h-3 w-3" />
                  Merge
                </button>
              )}
            </span>
          </div>
        ))}
      </div>

      {/* The merge confirmation gate (the same dialog the status bar uses). */}
      <MergeToMainDialog
        open={mergeTarget !== null}
        sourceBranch={mergeTarget ?? ""}
        merging={merging}
        onConfirm={() => void handleMergeConfirm()}
        onCancel={() => setMergeTarget(null)}
      />
    </div>
  );
}
