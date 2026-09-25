// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useState } from "react";
import { GitCompare, FileCode, Check, X, RefreshCw } from "lucide-react";
import type { ReactNode } from "react";
import { useAgentStore } from "../../hooks/useAgentStore";
import { errMsg, gitDiffHead, gitInit } from "../../lib/tauri";
import type { ApprovalPreview } from "../../lib/types";
import type { LastDiff } from "../../hooks/agentState";
import {
  DiffView,
  NewFileView,
  UnifiedDiffView,
} from "../chat/DiffView";

interface FileEditArgs {
  path?: string;
  old_string?: string;
  new_string?: string;
  content?: string;
}

/** The single previewed path; a `multi_diff` carries no single `path`, so
 *  its FIRST changed file stands in (the Diff tab renders one file at a
 *  time). Empty when there is no preview at all. */
function previewPath(preview: ApprovalPreview | null | undefined): string {
  if (preview === null || preview === undefined) return "";
  return preview.kind === "multi_diff" ? (preview.paths[0] ?? "") : preview.path;
}

/** Whether a pending approval belongs to a file-editing tool whose Rust
 *  preview the Diff tab renders — `file_edit` / `file_write` / `file_append`
 *  / `multi_edit` (`multi_edit` carries the combined `multi_diff` preview;
 *  leaving it out made the tab say "No file diff for this tool" — review
 *  L1). Pure — pinned by test. */
export function isFileEditTool(toolName: string): boolean {
  return (
    toolName === "file_edit" ||
    toolName === "file_write" ||
    toolName === "file_append" ||
    toolName === "multi_edit"
  );
}

/**
 * Resolve path + body for a pending approval: prefer Rust `preview`, fall
 * back to parsing tool args (legacy / non-file tools). A `multi_diff` (the
 * `multi_edit` preview) renders its ONE combined diff through the same
 * unified-body path; the first changed file labels the header (review L1).
 * Pure — pinned by test.
 */
export function contentFromPreviewOrArgs(
  preview: ApprovalPreview | null | undefined,
  toolName: string,
  args: FileEditArgs
): {
  path: string;
  mode: "unified" | "lcs" | "new_file" | "none";
  unifiedDiff?: string;
  oldText?: string;
  newText?: string;
  content?: string;
} {
  if (preview?.kind === "diff" && typeof preview.diff === "string") {
    return {
      path: preview.path || args.path || "(unknown path)",
      mode: "unified",
      unifiedDiff: preview.diff,
    };
  }
  if (preview?.kind === "new_file" && typeof preview.content === "string") {
    return {
      path: preview.path || args.path || "(unknown path)",
      mode: "new_file",
      content: preview.content,
    };
  }
  if (preview?.kind === "multi_diff" && typeof preview.diff === "string") {
    // One combined diff covering every file the call changes, so no single
    // `path` applies: the FIRST changed file labels the header (the tab
    // renders one scope at a time; ApprovalPrompt mirrors this).
    return {
      path: preview.paths[0] || args.path || "(unknown path)",
      mode: "unified",
      unifiedDiff: preview.diff,
    };
  }
  // Args fallback.
  if (
    toolName === "file_edit" &&
    args.old_string !== undefined &&
    args.new_string !== undefined
  ) {
    return {
      path: args.path || "(unknown path)",
      mode: "lcs",
      oldText: args.old_string,
      newText: args.new_string,
    };
  }
  if (args.content !== undefined) {
    return {
      path: args.path || "(unknown path)",
      mode: "new_file",
      content: args.content,
    };
  }
  return {
    path: args.path || previewPath(preview) || "(unknown path)",
    mode: "none",
  };
}

/** Render the body of a stored diff entry (unified / lcs / new_file). */
function DiffEntryBody({ entry }: { entry: LastDiff }) {
  if (entry.unifiedDiff !== undefined) {
    return (
      <UnifiedDiffView diffText={entry.unifiedDiff} maxHeightClass="max-h-full" />
    );
  }
  if (
    entry.toolName === "file_edit" &&
    entry.oldString !== undefined &&
    entry.newString !== undefined
  ) {
    return (
      <DiffView
        oldText={entry.oldString}
        newText={entry.newString}
        maxHeightClass="max-h-full"
      />
    );
  }
  if (entry.content !== undefined) {
    return <NewFileView content={entry.content} maxHeightClass="max-h-full" />;
  }
  return (
    <div className="text-[0.75em] text-slate-500">No diff content available.</div>
  );
}

/** The diff body shared by a stored entry and the live pending approval. */
function DiffBody({ resolved }: { resolved: ReturnType<typeof contentFromPreviewOrArgs> }) {
  if (resolved.mode === "unified" && resolved.unifiedDiff !== undefined) {
    return (
      <UnifiedDiffView
        diffText={resolved.unifiedDiff}
        maxHeightClass="max-h-full"
      />
    );
  }
  if (
    resolved.mode === "lcs" &&
    resolved.oldText !== undefined &&
    resolved.newText !== undefined
  ) {
    return (
      <DiffView
        oldText={resolved.oldText}
        newText={resolved.newText}
        maxHeightClass="max-h-full"
      />
    );
  }
  if (resolved.mode === "new_file" && resolved.content !== undefined) {
    return (
      <NewFileView content={resolved.content} maxHeightClass="max-h-full" />
    );
  }
  return (
    <div className="text-[0.75em] text-slate-500">
      No diff content available.
    </div>
  );
}

/** The Diff tab's view modes: the working tree vs git HEAD (comprehensive),
 *  or the captured last-edit snapshots from the current plan. */
export type DiffViewMode = "git" | "edit";

/**
 * Whether the vs-Git fetch effect should run: only in git mode, and only
 * while no file approval owns the view (the pending approval's preview is
 * the authoritative diff while it is in flight; when it resolves,
 * `showPending` flips false and the effect re-runs). Pure — pinned by test.
 */
export function shouldFetchGitDiff(mode: DiffViewMode, showPending: boolean): boolean {
  return mode === "git" && !showPending;
}

/**
 * The render-visible value of a scope-keyed result: only when it was
 * produced for the CURRENT scope. A stale diff/error from the previous file
 * never renders under the new file's label while its refetch is in flight
 * (review B3) — a scope change hides the old value until the new fetch
 * resolves. Pure — pinned by test.
 */
export function scopedResult<T>(
  stored: { scope: string; value: T } | null,
  scopeKey: string,
): T | null {
  return stored !== null && stored.scope === scopeKey ? stored.value : null;
}

/**
 * Right-panel diff viewer with two modes (toggle in the dropdown row):
 *
 * - **vs Git** (default) — the comprehensive working-tree diff against git
 *   HEAD (staged + unstaged tracked changes), fetched per selected file or
 *   for the whole tree ("all changed files"). Because `planDiffs` keeps only
 *   the newest edit per path, this is the only view that shows the FULL
 *   accumulated change for a file edited several times. Untracked
 *   (never-committed) files are not included.
 * - **last edit** — the captured per-tool-call snapshots from the current
 *   top-level plan (`planDiffs`, one entry per path, newest first; resets
 *   when a new top-level plan starts).
 *
 * A pending `file_edit` / `file_write` / `file_append` / `multi_edit` approval
 * always takes precedence over both modes: the Rust `ApprovalPreview` (UI H3)
 * renders a real line-by-line diff (or the full new content; `multi_edit`
 * shows its ONE combined diff covering every changed file) so the client does
 * not re-run LCS on raw args; falls back to args when the preview is null.
 */
/** The dropdown's selected value: the explicit diff-path selection when set
 *  (deep-linked or picked — kept even when the path has no captured plan-diff
 *  entry, so the control labels what the body renders and never a different
 *  file's name), else the pending/fallback scope, else "all changed files"
 *  in git mode. Pure — extracted so the vitest suite can pin it. */
export function diffDropdownValue(opts: {
  allScope: boolean;
  selectedDiffPath: string | null;
  showPending: boolean;
  fallbackPath: string | null;
  mode: DiffViewMode;
}): string {
  if (opts.allScope) return "__all__";
  if (opts.selectedDiffPath !== null) return opts.selectedDiffPath;
  if (opts.showPending) return "";
  return opts.fallbackPath ?? (opts.mode === "git" ? "__all__" : "");
}

/** Whether a transient dropdown option is needed for the selected path: it's
 *  set but absent from the captured plan-diff entries — e.g. a file_edit
 *  chip deep-linked a file with no captured diff (review 2026-08-22 M1). */
export function needsTransientDiffOption(
  selectedDiffPath: string | null,
  entries: { path: string }[],
): boolean {
  return selectedDiffPath !== null && !entries.some((e) => e.path === selectedDiffPath);
}

/** Classify a git-diff error message into a UI-actionable kind. The backend
 *  canonicalizes the two common failure modes into short strings; this helper
 *  also tolerates git's raw phrasings ("bad revision" / "unknown revision")
 *  for robustness. Pure — pinned by test. */
export function classifyGitDiffError(
  msg: string,
): "not-a-repo" | "no-commits" | "other" {
  const lower = msg.toLowerCase();
  if (lower.includes("not a git repository")) return "not-a-repo";
  if (
    lower.includes("no commits yet") ||
    lower.includes("bad revision") ||
    lower.includes("unknown revision")
  ) {
    return "no-commits";
  }
  return "other";
}

export function DiffViewer() {
  const activeAgent = useAgentStore((s) => s.activeAgent);
  const pendingApproval = useAgentStore(
    (s) =>
      (activeAgent !== null && s.agents[activeAgent]?.pendingApproval) || null
  );
  const entries = useAgentStore((s) => s.planDiffs);
  const selectedDiffPath = useAgentStore((s) => s.selectedDiffPath);
  const selectDiffPath = useAgentStore((s) => s.selectDiffPath);

  // The view mode: "git" (default — the comprehensive working tree vs git
  // HEAD, staged + unstaged tracked changes) or "edit" (the captured
  // last-edit snapshots). Local state — the choice resets when the tab
  // remounts, defaulting back to the comprehensive view.
  const [mode, setMode] = useState<DiffViewMode>("git");
  // Whether the git view scopes to the WHOLE tree (the "all changed files"
  // dropdown option) instead of one file.
  const [allScope, setAllScope] = useState(false);
  // Results are keyed by scope (the diffed file path, or "__tree__" for the
  // whole tree) so a stale diff/error from a PREVIOUS file never renders
  // under the new file's label while its refetch is in flight (review B3) —
  // `scopedResult` drops any value not produced for the current scope.
  const [gitResult, setGitResult] = useState<{ scope: string; value: string } | null>(null);
  const [gitError, setGitError] = useState<{ scope: string; value: string } | null>(null);
  const [gitLoading, setGitLoading] = useState(false);
  // Bumped by the Refresh button to re-run the fetch effect.
  const [refreshKey, setRefreshKey] = useState(0);
  // The "Initialize Git Repository" button (shown when the open directory is
  // not a repo): tracks its own loading + error so the git-diff fetch state
  // stays clean. A successful init bumps refreshKey to re-fetch.
  const [initLoading, setInitLoading] = useState(false);
  const [initError, setInitError] = useState<string | null>(null);

  /** Initialize a git repo at the project root, then re-run the diff fetch. */
  const handleInitGit = () => {
    setInitLoading(true);
    setInitError(null);
    gitInit()
      .then(() => {
        // Clear the stale not-a-repo error so the re-fetch shows the loading
        // state (not the old error panel with a re-enabled button) during the
        // one IPC round-trip before the "no commits yet" result lands.
        setGitError(null);
        setRefreshKey((k) => k + 1);
      })
      .catch((e) => {
        setInitError(errMsg(e));
      })
      .finally(() => {
        setInitLoading(false);
      });
  };

  const toolName = pendingApproval?.toolName ?? "";
  const args = (pendingApproval?.args ?? {}) as FileEditArgs;
  const isFileEdit = isFileEditTool(toolName);
  // Resolve the pending path from the Rust preview first (args.path can be
  // absent when the preview carries the path).
  const pendingResolved =
    pendingApproval && isFileEdit
      ? contentFromPreviewOrArgs(pendingApproval.preview, toolName, args)
      : null;
  const pendingPath = pendingResolved ? pendingResolved.path : null;
  const selectedEntry = selectedDiffPath
    ? entries.find((e) => e.path === selectedDiffPath) ?? null
    : null;
  // The dropdown + content fall back to the newest entry when nothing is
  // explicitly selected (and no pending approval is in focus).
  const fallbackEntry = entries.length > 0 ? entries[0] : null;
  const shownEntry = selectedEntry ?? fallbackEntry;

  // Live pending approval: shown when a file approval is in flight AND the
  // user hasn't picked a specific past diff (picking one freezes that file).
  const showPending = pendingApproval !== null && isFileEdit && selectedEntry === null;

  // The dropdown row renders whenever there is something to browse: the vs-Git
  // mode (always — the whole-tree diff is meaningful even with no captured
  // edits), a pending file edit, or at least one captured diff.
  const showDropdown = mode === "git" || showPending || pendingPath !== null || entries.length > 0;

  // The file the git view diffs: the explicit selection, else the newest
  // captured edit's path (mirroring the edit-mode fallback), else the whole
  // tree. `allScope` (the "all changed files" option) forces the whole tree.
  const gitScopePath = allScope
    ? null
    : selectedDiffPath ?? fallbackEntry?.path ?? null;
  const gitScopeLabel = gitScopePath ?? "all changed files";
  // The identity of the current scope — stored results are keyed by this so
  // a scope change never shows the previous file's diff under the new label.
  const scopeKey = gitScopePath ?? "__tree__";
  const gitDiff = scopedResult(gitResult, scopeKey);
  const gitErrorMsg = scopedResult(gitError, scopeKey);

  // The vs-Git fetch: the comprehensive working-tree diff vs HEAD for
  // `gitScopePath`. Re-runs on mode/scope/selection changes, on Refresh
  // (refreshKey), and when a pending approval resolves (it owns the view
  // while in flight — shouldFetchGitDiff gates on that). A failed scope
  // replaces that scope's last diff — no stale body under a red error.
  useEffect(() => {
    if (!shouldFetchGitDiff(mode, showPending)) return;
    let disposed = false;
    setGitLoading(true);
    gitDiffHead(gitScopePath)
      .then((text) => {
        if (disposed) return;
        setGitResult({ scope: scopeKey, value: text });
        setGitError(null);
      })
      .catch((e) => {
        if (disposed) return;
        setGitError({ scope: scopeKey, value: errMsg(e) });
        setGitResult((r) => (r && r.scope === scopeKey ? null : r));
      })
      .finally(() => {
        if (!disposed) setGitLoading(false);
      });
    return () => {
      disposed = true;
    };
  }, [mode, allScope, gitScopePath, scopeKey, showPending, refreshKey]);

  let body: ReactNode;
  let header: ReactNode = null;

  if (showPending && pendingApproval && pendingResolved) {
    const resolved = pendingResolved;
    header = (
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <FileCode className="h-4 w-4 shrink-0 text-cyan-400" />
        <span className="truncate text-[0.75em] font-medium text-slate-300">
          {toolName}: {resolved.path}
        </span>
        <span className="ml-auto rounded bg-yellow-950/40 px-1.5 py-0.5 text-[0.625em] font-medium text-yellow-400">
          pending
        </span>
      </div>
    );
    body = <DiffBody resolved={resolved} />;
  } else if (pendingApproval && !isFileEdit) {
    // A pending approval for a non-file tool: no diff to show, but don't fall
    // through to the empty state while an approval is in flight.
    body = (
      <div className="flex h-full flex-col items-center justify-center text-[0.875em] text-slate-500">
        <GitCompare className="mb-2 h-8 w-8 opacity-50" />
        <p>Approval pending: {pendingApproval.toolName}</p>
        <p className="text-[0.75em]">No file diff for this tool.</p>
      </div>
    );
  } else if (mode === "git") {
    // The comprehensive view: the working tree vs git HEAD for the selected
    // file (or the whole tree). Empty text = tracked files match HEAD;
    // git failures (e.g. not a repo) surface as the error message.
    header = (
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <GitCompare className="h-4 w-4 shrink-0 text-cyan-400" />
        <span className="truncate text-[0.75em] font-medium text-slate-300">
          {gitScopeLabel}
        </span>
        <span className="shrink-0 rounded bg-sky-950/40 px-1.5 py-0.5 text-[0.625em] font-medium text-sky-400">
          vs Git
        </span>
        <button
          className="ml-auto flex shrink-0 items-center gap-1 rounded px-2 py-1 text-slate-300 hover:bg-slate-800 disabled:opacity-50"
          onClick={() => setRefreshKey((k) => k + 1)}
          disabled={gitLoading}
          title="Re-run git diff HEAD"
        >
          <RefreshCw className={`h-3 w-3 ${gitLoading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>
    );
    if (gitErrorMsg !== null) {
      const errKind = classifyGitDiffError(gitErrorMsg);
      if (errKind === "not-a-repo") {
        body = (
          <div className="flex h-full flex-col items-center justify-center gap-3 text-center text-[0.875em] text-slate-400">
            <GitCompare className="h-8 w-8 opacity-50" />
            <p className="font-medium">This directory is not a git repository.</p>
            <p className="text-[0.75em] text-slate-500">
              Initialize one to track changes against git HEAD.
            </p>
            <button
              className="flex items-center gap-1.5 rounded bg-sky-800/70 px-3 py-1.5 text-[0.75em] font-medium text-sky-100 hover:bg-sky-700 disabled:opacity-50"
              onClick={handleInitGit}
              disabled={initLoading || gitLoading}
              title="Run git init in the project root"
            >
              {initLoading ? "Initializing…" : "Initialize Git Repository"}
            </button>
            {initError && (
              <p className="text-[0.7em] text-red-400">{initError}</p>
            )}
          </div>
        );
      } else if (errKind === "no-commits") {
        body = (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-center text-[0.875em] text-slate-500">
            <GitCompare className="h-8 w-8 opacity-50" />
            <p className="font-medium text-slate-400">No commits yet</p>
            <p className="text-[0.75em]">
              Make your first commit to see changes here.
            </p>
          </div>
        );
      } else {
        body = (
          <div className="p-3 text-[0.75em] text-red-400">{gitErrorMsg}</div>
        );
      }
    } else if (gitDiff !== null && gitDiff !== "") {
      body = <UnifiedDiffView diffText={gitDiff} maxHeightClass="max-h-full" />;
    } else if (gitDiff === "" && !gitLoading) {
      body = (
        <div className="flex h-full flex-col items-center justify-center text-[0.875em] text-slate-500">
          <GitCompare className="mb-2 h-8 w-8 opacity-50" />
          <p>No changes vs git HEAD.</p>
          <p className="text-[0.75em]">
            Tracked files match the last commit.
          </p>
        </div>
      );
    } else {
      body = (
        <div className="p-3 text-[0.75em] text-slate-500">loading diff…</div>
      );
    }
  } else if (shownEntry) {
    const entry = shownEntry;
    const filePath = entry.path || "(unknown path)";
    header = (
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <FileCode className="h-4 w-4 shrink-0 text-cyan-400" />
        <span className="truncate text-[0.75em] font-medium text-slate-300">
          {entry.toolName}: {filePath}
        </span>
        <span className="ml-auto flex items-center gap-1 text-[0.625em] font-medium">
          {entry.success ? (
            <span className="flex items-center gap-0.5 text-green-400">
              <Check className="h-3 w-3" />
              applied
            </span>
          ) : (
            <span className="flex items-center gap-0.5 text-red-400">
              <X className="h-3 w-3" />
              failed
            </span>
          )}
        </span>
      </div>
    );
    body = <DiffEntryBody entry={entry} />;
  }

  return (
    <div className="flex h-full flex-col overflow-hidden">
      {showDropdown && (
        <div className="flex items-center gap-2 border-b border-border px-3 py-2">
          <select
            value={diffDropdownValue({
              allScope,
              selectedDiffPath,
              showPending,
              fallbackPath: fallbackEntry?.path ?? null,
              mode,
            })}
            onChange={(e) => {
              const v = e.target.value;
              if (v === "__all__") {
                // The whole-tree vs-Git view: also flip the mode so the pick
                // is immediately visible (the option is labeled "vs Git").
                setAllScope(true);
                setMode("git");
                selectDiffPath(null);
              } else {
                setAllScope(false);
                selectDiffPath(v === "" ? null : v);
              }
            }}
            className="min-w-0 flex-1 truncate rounded border border-border bg-bg-tertiary px-2 py-1 text-[0.75em] text-slate-300"
            title="Changed files in the current top-level plan, or the whole tree vs git"
          >
            {showPending && (
              <option value="">
                pending: {pendingPath ?? "(unknown path)"}
              </option>
            )}
            <option value="__all__">all changed files (vs Git)</option>
            {/* A deep-linked selection with no captured plan-diff entry gets
                a transient option so the control can label it (review M1). */}
            {needsTransientDiffOption(selectedDiffPath, entries) && (
              <option value={selectedDiffPath as string}>
                selected: {selectedDiffPath}
              </option>
            )}
            {entries.map((e) => (
              <option key={e.path} value={e.path}>
                {e.toolName}: {e.path}
              </option>
            ))}
          </select>
          {/* View-mode toggle: the comprehensive git view (default) or the
              captured last-edit snapshots. */}
          <div className="flex shrink-0 overflow-hidden rounded border border-border text-[0.625em]">
            <button
              onClick={() => setMode("git")}
              className={
                mode === "git"
                  ? "bg-sky-900/60 px-2 py-1 text-sky-300"
                  : "px-2 py-1 text-slate-400 hover:bg-slate-800"
              }
              title="Show the working tree vs git HEAD (staged + unstaged tracked changes)"
            >
              vs Git
            </button>
            <button
              onClick={() => {
                // Dropping out of git mode also drops the whole-tree scope —
                // otherwise the dropdown would label the shown per-file edit
                // "all changed files (vs Git)" (review nit).
                setAllScope(false);
                setMode("edit");
              }}
              className={
                mode === "edit"
                  ? "bg-sky-900/60 px-2 py-1 text-sky-300"
                  : "px-2 py-1 text-slate-400 hover:bg-slate-800"
              }
              title="Show the captured last-edit snapshots from the current plan"
            >
              last edit
            </button>
          </div>
          <span className="shrink-0 text-[0.7em] text-slate-500">
            {entries.length} file{entries.length === 1 ? "" : "s"} this plan
          </span>
        </div>
      )}

      {header && <div className="shrink-0">{header}</div>}

      {body !== undefined ? (
        <div className="flex-1 overflow-auto p-2">{body}</div>
      ) : (
        <div className="flex h-full flex-col items-center justify-center text-[0.875em] text-slate-500">
          <GitCompare className="mb-2 h-8 w-8 opacity-50" />
          <p>No pending edits.</p>
          <p className="text-[0.75em]">
            File-edit approvals will show a diff here.
          </p>
        </div>
      )}
    </div>
  );
}
